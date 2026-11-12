//! Git via the CLI (so SSH keys, credential helpers and worktrees behave exactly as in a shell).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Config overrides on every daemon git call: never run repo-controlled code (hooks, fsmonitor)
/// and never use the `ext::` transport.
const HARDEN: &[&str] = &["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "protocol.ext.allow=never"];

pub async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(HARDEN)
        .arg("-C")
        .arg(dir)
        .args(args)
        // Never prompt for credentials or open an editor from a background daemon.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .kill_on_drop(true)
        .output()
        .await
        .context("run git")?;
    if !out.status.success() {
        bail!("git {}: {}", redact(&args.join(" ")), redact(String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Hide credentials embedded in URLs (`https://user:token@host`) in messages and logs.
pub fn redact(s: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"://[^/@\s]+@").expect("regex"));
    RE.replace_all(s, "://***@").into_owned()
}

/// Local branch used for a job's worktree. Lives in the user's clone until cleanup.
pub fn branch_name(target: crate::forge::Target, n: u64) -> String {
    match target {
        crate::forge::Target::Pr => format!("nightporter/pr-{n}"),
        crate::forge::Target::Issue => format!("nightporter/issue-{n}"),
    }
}

/// Create or reset the worktree at `wt` to the fetched head. Returns (sha, `.git` file content,
/// trash ref): the caller re-checks that file before trusting any git call in the worktree again.
/// Uncommitted changes or commits that aren't on the fetched head (yours after a handoff, or an
/// earlier job's unpushed ones) are kept in the trash before the reset, under the returned ref.
pub async fn prepare_worktree(
    clone: &Path,
    wt: &Path,
    url: &str,
    fetch_ref: &str,
    branch: &str,
) -> Result<(String, String, Option<String>)> {
    let tmp_ref = format!("refs/nightporter/{}", branch.replace('/', "-"));
    git(clone, &["fetch", "--no-tags", "--quiet", "--end-of-options", url, &format!("+{fetch_ref}:{tmp_ref}")]).await?;
    let sha = git(clone, &["rev-parse", &tmp_ref]).await?;
    // A previous agent may have repointed the worktree's `.git` at a git dir it built (whose
    // config can run filters/hooks in the daemon). Never run git through it: start over.
    if wt.exists() && !gitfile_points_into(clone, wt).await {
        std::fs::remove_dir_all(wt).with_context(|| format!("remove tampered worktree {}", wt.display()))?;
    }
    let mut kept = None;
    if wt.join(".git").exists() {
        let dirty = !git(wt, &["status", "--porcelain"]).await?.is_empty();
        if dirty || !is_ancestor(wt, "HEAD", &sha).await {
            kept = trash(clone, wt, branch, crate::state::now()).await?;
        }
        git(wt, &["checkout", "--quiet", "-B", branch, &sha]).await?;
        git(wt, &["reset", "--hard", "--quiet", &sha]).await?;
        // Keep ignored files (node_modules, .ddev state) so env setup stays fast.
        git(wt, &["clean", "-fd", "--quiet"]).await?;
    } else {
        if let Some(p) = wt.parent() {
            std::fs::create_dir_all(p)?;
        }
        // A worktree dir deleted by hand leaves stale metadata that blocks `add`.
        git(clone, &["worktree", "prune"]).await?;
        git(clone, &["worktree", "add", "--quiet", "-B", branch, &wt.to_string_lossy(), &sha]).await?;
    }
    let gitfile = std::fs::read_to_string(wt.join(".git")).context("worktree has no .git file")?;
    Ok((sha, gitfile, kept))
}

/// `wt/.git` is a regular file naming a git dir directly inside the clone's `worktrees/`.
pub async fn gitfile_points_into(clone: &Path, wt: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(wt.join(".git")) else { return false };
    let Ok(text) = std::fs::read_to_string(wt.join(".git")) else { return false };
    let Some(dir) = text.trim_end().strip_prefix("gitdir: ") else { return false };
    let Ok(common) = git(clone, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await else { return false };
    let (Ok(dir), Ok(worktrees)) = (Path::new(dir).canonicalize(), Path::new(&common).join("worktrees").canonicalize()) else {
        return false;
    };
    meta.is_file() && dir.parent() == Some(worktrees.as_path())
}

/// The agent must not be able to point the worktree at another git dir (its config could run
/// commands in the daemon). Call before any git command in the worktree after the agent ran.
pub fn verify_gitfile(wt: &Path, expected: &str) -> Result<()> {
    let meta = std::fs::symlink_metadata(wt.join(".git"))?;
    let now = std::fs::read_to_string(wt.join(".git")).unwrap_or_default();
    if !meta.is_file() || now != expected {
        bail!("the worktree's .git was modified during the job; refusing to continue");
    }
    Ok(())
}

/// The git config files our own git calls in a worktree read, as they were before the agent ran:
/// the clone's `config`, and the worktree's own `config.worktree`, `commondir` and `gitdir`.
pub struct ConfigGuard(Vec<(PathBuf, Option<Vec<u8>>)>);

impl ConfigGuard {
    pub fn take(wt: &Path) -> Self {
        let admin = std::fs::read_to_string(wt.join(".git")).ok().and_then(|t| t.trim_end().strip_prefix("gitdir: ").map(PathBuf::from));
        let mut files = vec![];
        if let Some(admin) = admin {
            if let Some(common) = admin.parent().and_then(Path::parent) {
                files.extend([common.join("config"), common.join("config.worktree")]);
            }
            files.extend([admin.join("config.worktree"), admin.join("commondir"), admin.join("gitdir")]);
        }
        Self(files.into_iter().map(|f| (f.clone(), std::fs::read(&f).ok())).collect())
    }

    /// Put back anything that changed (so your own git in the clone is safe too), then fail.
    pub fn verify(&self) -> Result<()> {
        let mut changed = vec![];
        for (f, before) in &self.0 {
            if std::fs::read(f).ok() != *before {
                changed.push(f.display().to_string());
                match before {
                    Some(b) => std::fs::write(f, b)?,
                    None => {
                        let _ = std::fs::remove_file(f);
                    }
                }
            }
        }
        if !changed.is_empty() {
            bail!("the agent changed git config ({}); restored it and stopped the job", changed.join(", "));
        }
        Ok(())
    }
}

/// Where a PR's base branch is fetched for merges and comparisons. One per PR, so concurrent jobs
/// in the same clone never see each other's base.
pub fn base_ref(branch: &str) -> String {
    format!("refs/nightporter/base-{}", branch.replace('/', "-"))
}

pub async fn remove_worktree(clone: &Path, wt: &Path, branch: &str) -> Result<()> {
    if wt.exists() {
        git(clone, &["worktree", "remove", "--force", &wt.to_string_lossy()]).await.ok();
        if wt.exists() {
            std::fs::remove_dir_all(wt)?;
        }
    }
    git(clone, &["worktree", "prune"]).await.ok();
    git(clone, &["branch", "-D", branch]).await.ok();
    git(clone, &["update-ref", "-d", &format!("refs/nightporter/{}", branch.replace('/', "-"))]).await.ok();
    git(clone, &["update-ref", "-d", &base_ref(branch)]).await.ok();
    Ok(())
}

/// Where removed worktrees are kept: `refs/nightporter/trash/<branch>/<unix time>`, in the clone.
pub const TRASH: &str = "refs/nightporter/trash/";
/// Subject of the commit holding a worktree's uncommitted changes; restore undoes it.
const WIP: &str = "nightporter: uncommitted changes";

/// Keep a worktree's work (its commits and any uncommitted changes) under a trash ref before it's
/// removed. Returns the ref, or `None` when there's no worktree, or its `.git` doesn't point into
/// the clone (never run git through that).
pub async fn trash(clone: &Path, wt: &Path, branch: &str, now: i64) -> Result<Option<String>> {
    if !wt.exists() || !gitfile_points_into(clone, wt).await {
        return Ok(None);
    }
    let head = git(wt, &["rev-parse", "HEAD"]).await?;
    // ponytail: untracked files that aren't ignored go in too; skip them if snapshots get big.
    git(wt, &["add", "-A"]).await?;
    let tree = git(wt, &["write-tree"]).await?;
    let sha = if git(wt, &["rev-parse", "HEAD^{tree}"]).await? == tree {
        head
    } else {
        let id = ["-c", "user.name=Night Porter", "-c", "user.email=nightporter@localhost"];
        git(wt, &[&id[..], &["commit-tree", &tree, "-p", &head, "-m", WIP]].concat()).await?
    };
    // Never overwrite an entry made in the same second (a swap trashes twice in a row).
    let mut at = now;
    let r = loop {
        let r = format!("{TRASH}{}/{at}", branch.replace('/', "-"));
        if git(clone, &["rev-parse", "--verify", "--quiet", &r]).await.is_err() {
            break r;
        }
        at += 1;
    };
    git(clone, &["update-ref", &r, &sha]).await?;
    Ok(Some(r))
}

/// Trash refs in a clone: (ref, branch slug, unix time it was removed).
pub async fn trash_refs(clone: &Path) -> Result<Vec<(String, String, i64)>> {
    let out = git(clone, &["for-each-ref", "--format=%(refname)", TRASH]).await?;
    Ok(out
        .lines()
        .filter_map(|r| {
            let (slug, ts) = r.strip_prefix(TRASH)?.rsplit_once('/')?;
            Some((r.to_string(), slug.to_string(), ts.parse().ok()?))
        })
        .collect())
}

/// Bring a trashed worktree back at `wt`, uncommitted changes uncommitted again.
pub async fn restore(clone: &Path, wt: &Path, branch: &str, trash_ref: &str) -> Result<()> {
    if wt.exists() {
        bail!("{} exists (a newer job made it again): remove that one first", wt.display());
    }
    let sha = git(clone, &["rev-parse", "--verify", &format!("{trash_ref}^{{commit}}")]).await?;
    if let Some(p) = wt.parent() {
        std::fs::create_dir_all(p)?;
    }
    git(clone, &["worktree", "prune"]).await?;
    git(clone, &["worktree", "add", "--quiet", "-B", branch, &wt.to_string_lossy(), &sha]).await?;
    if git(clone, &["log", "-1", "--format=%s", &sha]).await? == WIP {
        git(wt, &["reset", "--quiet", "HEAD~1"]).await?;
    }
    git(clone, &["update-ref", "-d", trash_ref]).await?;
    Ok(())
}

/// The remote branch's current SHA, or `None` if it doesn't exist.
pub async fn remote_head(clone: &Path, url: &str, branch: &str) -> Result<Option<String>> {
    let out = git(clone, &["ls-remote", "--end-of-options", url, &format!("refs/heads/{branch}")]).await?;
    Ok(out.split_whitespace().next().map(String::from))
}

/// Push exactly `sha` from the clone (not the worktree). Never forced: a non-fast-forward is
/// rejected by the server.
pub async fn push(clone: &Path, url: &str, sha: &str, remote_branch: &str) -> Result<()> {
    git(clone, &["push", "--quiet", "--no-verify", "--end-of-options", url, &format!("{sha}:refs/heads/{remote_branch}")]).await.map(|_| ())
}

/// When your git config asks for signed commits (`commit.gpgsign`), re-create the commits in
/// `before..after` signed with your own setup (key, format, gpg program, agent), run here in the
/// daemon: the agent never sees your keys, so its commits are unsigned. Trees, parents' order,
/// authors, dates and messages are kept; only the signatures, and so the SHAs, change.
/// Returns the old → new SHA of each commit, oldest first (`None`: signing isn't configured).
pub async fn sign_range(clone: &Path, before: &str, after: &str) -> Result<Option<Vec<(String, String)>>> {
    if git(clone, &["config", "--get", "--type=bool", "commit.gpgsign"]).await.ok().as_deref() != Some("true") {
        return Ok(None);
    }
    let revs = git(clone, &["rev-list", "--reverse", "--topo-order", &format!("{before}..{after}")]).await?;
    let mut map: Vec<(String, String)> = Vec::new();
    for old in revs.lines() {
        let meta = git(clone, &["show", "-s", "--format=%T%n%P%n%an%n%ae%n%aI%n%cn%n%ce%n%cI", old]).await?;
        let f: Vec<&str> = meta.lines().collect();
        let [tree, parents, an, ae, ad, cn, ce, cd] = f[..] else { bail!("unexpected commit metadata for {old}") };
        let message = git(clone, &["show", "-s", "--format=%B", old]).await?;
        let mut cmd = Command::new("git");
        cmd.args(HARDEN).arg("-C").arg(clone).args(["commit-tree", "-S", tree, "-m", &message]);
        for p in parents.split_whitespace() {
            cmd.args(["-p", map.iter().find(|(o, _)| o == p).map_or(p, |(_, n)| n)]);
        }
        cmd.envs([
            ("GIT_AUTHOR_NAME", an),
            ("GIT_AUTHOR_EMAIL", ae),
            ("GIT_AUTHOR_DATE", ad),
            ("GIT_COMMITTER_NAME", cn),
            ("GIT_COMMITTER_EMAIL", ce),
            ("GIT_COMMITTER_DATE", cd),
        ]);
        // A key that needs a passphrase nobody is there to type must not hang the job.
        let out = tokio::time::timeout(std::time::Duration::from_secs(60), cmd.kill_on_drop(true).output())
            .await
            .context("signing timed out (is the key waiting for a passphrase?)")?
            .context("run git")?;
        if !out.status.success() {
            bail!("git commit-tree -S: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        map.push((old.to_string(), String::from_utf8_lossy(&out.stdout).trim().to_string()));
    }
    Ok(Some(map))
}

pub async fn head(wt: &Path) -> Result<String> {
    git(wt, &["rev-parse", "HEAD"]).await
}

pub async fn is_ancestor(wt: &Path, a: &str, b: &str) -> bool {
    git(wt, &["merge-base", "--is-ancestor", a, b]).await.is_ok()
}

/// The URL of the clone's remote that points at `repo`, in the clone's own scheme.
/// Read from `.git/config` as written (not `git remote -v`, which applies `insteadOf` rewrites).
pub fn remote_url(clone: &Path, repo: &crate::forge::RepoId, accounts: &[crate::config::Account]) -> Result<String> {
    let cfg = std::fs::read_to_string(clone.join(".git/config")).with_context(|| format!("read {}/.git/config", clone.display()))?;
    crate::repos::remote_urls(&cfg)
        .into_iter()
        .find(|u| crate::repos::parse_remote(u, accounts).as_ref() == Some(repo))
        .with_context(|| format!("no remote for {repo} in {}", clone.display()))
}

/// Pick the head repo URL in the same scheme the clone uses (ssh vs https).
pub fn same_scheme(clone_url: &str, ssh: &str, https: &str) -> String {
    let https_like = clone_url.starts_with("http");
    if (https_like && !https.is_empty()) || ssh.is_empty() { https.into() } else { ssh.into() }
}

/// Free space in GiB on the filesystem holding `p` (via `df`, portable across macOS/Linux).
pub fn free_gb(p: &Path) -> Result<u64> {
    let mut probe = PathBuf::from(p);
    while !probe.exists() {
        if !probe.pop() {
            break;
        }
    }
    let out = std::process::Command::new("df").arg("-Pk").arg(&probe).output().context("run df")?;
    let s = String::from_utf8_lossy(&out.stdout);
    let avail_kb: u64 =
        s.lines().nth(1).and_then(|l| l.split_whitespace().nth(3)).and_then(|v| v.parse().ok()).context("parse df output")?;
    Ok(avail_kb / 1024 / 1024)
}

/// A sha shortened for messages.
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

#[cfg(test)]
pub(crate) fn tests_head(dir: &Path) -> String {
    let o = std::process::Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A throwaway repo with one commit (unsigned, no user config needed).
    pub(crate) fn repo(dir: &Path) {
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-qm", "base"]);
    }

    #[tokio::test]
    async fn sign_range_resigns_commits_only_when_configured() {
        let d = tempfile::tempdir().unwrap();
        let clone = d.path().join("clone");
        std::fs::create_dir(&clone).unwrap();
        repo(&clone);
        let run = |args: &[&str]| {
            let o = std::process::Command::new("git").args(args).current_dir(&clone).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        run(&["config", "user.name", "t"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "commit.gpgsign", "false"]);
        let before = run(&["rev-parse", "HEAD"]);
        for n in ["one", "two"] {
            run(&["commit", "-q", "--allow-empty", "-m", n]);
        }
        let after = run(&["rev-parse", "HEAD"]);
        assert_eq!(sign_range(&clone, &before, &after).await.unwrap(), None, "signing not configured");

        // An SSH signing key: no gpg keyring needed.
        let key = d.path().join("key");
        let made = std::process::Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(&key).output().unwrap();
        assert!(made.status.success());
        run(&["config", "commit.gpgsign", "true"]);
        run(&["config", "gpg.format", "ssh"]);
        run(&["config", "user.signingkey", &key.to_string_lossy()]);
        let map = sign_range(&clone, &before, &after).await.unwrap().unwrap();
        assert_eq!((map.len(), map[1].0.as_str()), (2, after.as_str()));
        let head = &map[1].1;
        assert!(run(&["cat-file", "commit", head]).contains("gpgsig"), "signed");
        assert_eq!(run(&["rev-parse", &format!("{head}^")]), map[0].1, "re-parented onto the signed commit");
        assert_eq!(run(&["rev-parse", &format!("{head}~2")]), before);
        assert_eq!(run(&["log", "-1", "--format=%s|%an|%T", head]), run(&["log", "-1", "--format=%s|%an|%T", &after]));
    }

    #[tokio::test]
    async fn tampered_gitfile_worktree_is_recreated() {
        let d = tempfile::tempdir().unwrap();
        let clone = d.path().join("clone");
        std::fs::create_dir(&clone).unwrap();
        repo(&clone);
        let url = clone.to_string_lossy().to_string();
        let wt = d.path().join("wt");
        let (_, gitfile, _) = prepare_worktree(&clone, &wt, &url, "refs/heads/main", "nightporter/pr-1").await.unwrap();
        assert!(gitfile_points_into(&clone, &wt).await);

        // An agent repoints .git at a git dir it built inside the worktree.
        let evil = wt.join("evil-git");
        std::fs::create_dir_all(&evil).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", evil.display())).unwrap();
        assert!(!gitfile_points_into(&clone, &wt).await);
        assert!(verify_gitfile(&wt, &gitfile).is_err());

        // The next job must not run git through it: the worktree is rebuilt.
        prepare_worktree(&clone, &wt, &url, "refs/heads/main", "nightporter/pr-1").await.unwrap();
        assert!(gitfile_points_into(&clone, &wt).await);
        assert!(!evil.exists());
    }

    #[test]
    fn redacts() {
        assert_eq!(redact("push https://x-token:ghp_abc@github.com/o/r"), "push https://***@github.com/o/r");
        assert_eq!(redact("git@github.com:o/r"), "git@github.com:o/r");
    }

    #[test]
    fn schemes() {
        assert_eq!(same_scheme("git@github.com:o/r", "git@x:f/r", "https://x/f/r"), "git@x:f/r");
        assert_eq!(same_scheme("https://github.com/o/r", "git@x:f/r", "https://x/f/r"), "https://x/f/r");
    }

    #[test]
    fn df_works() {
        assert!(free_gb(Path::new("/nonexistent/deep/path")).is_ok());
    }

    #[tokio::test]
    async fn config_guard_undoes_git_config_changes() {
        let d = tempfile::tempdir().unwrap();
        let clone = d.path().join("clone");
        std::fs::create_dir(&clone).unwrap();
        repo(&clone);
        let wt = d.path().join("wt");
        prepare_worktree(&clone, &wt, &clone.to_string_lossy(), "refs/heads/main", "nightporter/pr-1").await.unwrap();
        let g = ConfigGuard::take(&wt);
        g.verify().unwrap();
        let cfg = clone.join(".git/config");
        let before = std::fs::read_to_string(&cfg).unwrap();
        // What `git stash show --output=` could do: a config that runs a command on fetch.
        std::fs::write(&cfg, format!("{before}[core]\n\tsshCommand = touch PWNED\n")).unwrap();
        std::fs::write(clone.join(".git/config.worktree"), "[core]\n").unwrap();
        let e = g.verify().unwrap_err().to_string();
        assert!(e.contains("changed git config"), "{e}");
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), before);
        assert!(!clone.join(".git/config.worktree").exists());
    }
}
