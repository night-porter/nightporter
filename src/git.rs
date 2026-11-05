//! Git via the CLI (so SSH keys, credential helpers and worktrees behave exactly as in a shell).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
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
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Local branch used for a job's worktree. Lives in the user's clone until cleanup.
pub fn branch_name(target: crate::forge::Target, n: u64) -> String {
    match target {
        crate::forge::Target::Pr => format!("nightporter/pr-{n}"),
        crate::forge::Target::Issue => format!("nightporter/issue-{n}"),
    }
}

/// Create or reset the worktree at `wt` to `sha` (fetched from `url` `refspec`).
pub async fn prepare_worktree(clone: &Path, wt: &Path, url: &str, fetch_ref: &str, branch: &str) -> Result<String> {
    let tmp_ref = format!("refs/nightporter/{}", branch.replace('/', "-"));
    git(clone, &["fetch", "--no-tags", "--quiet", url, &format!("+{fetch_ref}:{tmp_ref}")]).await?;
    let sha = git(clone, &["rev-parse", &tmp_ref]).await?;
    if wt.join(".git").exists() {
        git(wt, &["checkout", "--quiet", "-B", branch, &sha]).await?;
        git(wt, &["reset", "--hard", "--quiet", &sha]).await?;
        // Keep ignored files (node_modules, .ddev state) so env setup stays fast.
        git(wt, &["clean", "-fd", "--quiet"]).await?;
    } else {
        if let Some(p) = wt.parent() {
            std::fs::create_dir_all(p)?;
        }
        git(clone, &["worktree", "add", "--quiet", "-B", branch, &wt.to_string_lossy(), &sha]).await?;
    }
    Ok(sha)
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

/// Plain push (never forced): a non-fast-forward is rejected by the server.
pub async fn push(wt: &Path, url: &str, remote_branch: &str) -> Result<()> {
    git(wt, &["push", "--quiet", "--no-verify", url, &format!("HEAD:refs/heads/{remote_branch}")]).await.map(|_| ())
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
mod tests {
    use super::*;

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
