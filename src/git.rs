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

/// Create or reset the worktree at `wt` to the fetched head. Returns (sha, `.git` file content);
/// the caller re-checks that file before trusting any git call in the worktree again.
pub async fn prepare_worktree(clone: &Path, wt: &Path, url: &str, fetch_ref: &str, branch: &str) -> Result<(String, String)> {
    let tmp_ref = format!("refs/nightporter/{}", branch.replace('/', "-"));
    git(clone, &["fetch", "--no-tags", "--quiet", "--end-of-options", url, &format!("+{fetch_ref}:{tmp_ref}")]).await?;
    let sha = git(clone, &["rev-parse", &tmp_ref]).await?;
    // A previous agent may have repointed the worktree's `.git` at a git dir it built (whose
    // config can run filters/hooks in the daemon). Never run git through it: start over.
    if wt.exists() && !gitfile_points_into(clone, wt).await {
        std::fs::remove_dir_all(wt).with_context(|| format!("remove tampered worktree {}", wt.display()))?;
    }
    if wt.join(".git").exists() {
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
    Ok((sha, gitfile))
}

/// `wt/.git` is a regular file naming a git dir directly inside the clone's `worktrees/`.
async fn gitfile_points_into(clone: &Path, wt: &Path) -> bool {
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

pub async fn head(wt: &Path) -> Result<String> {
    git(wt, &["rev-parse", "HEAD"]).await
}

pub async fn is_ancestor(wt: &Path, a: &str, b: &str) -> bool {
    git(wt, &["merge-base", "--is-ancestor", a, b]).await.is_ok()
}

/// The URL of the clone's remote that points at `repo`, in the clone's own scheme.
/// Read from `.git/config` as written (not `git remote -v`, which applies `insteadOf` rewrites).
pub async fn remote_url(clone: &Path, repo: &crate::forge::RepoId, accounts: &[crate::config::Account]) -> Result<String> {
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
    async fn tampered_gitfile_worktree_is_recreated() {
        let d = tempfile::tempdir().unwrap();
        let clone = d.path().join("clone");
        std::fs::create_dir(&clone).unwrap();
        repo(&clone);
        let url = clone.to_string_lossy().to_string();
        let wt = d.path().join("wt");
        let (_, gitfile) = prepare_worktree(&clone, &wt, &url, "refs/heads/main", "nightporter/pr-1").await.unwrap();
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
}
