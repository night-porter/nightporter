//! The worktrees Night Porter made: list them, remove one (into a trash you can undo for a
//! week), bring one back. Removing keeps the branch's commits and any uncommitted changes under
//! a ref in the clone (`refs/nightporter/trash/…`), so a restore gets your work back; ignored
//! files (`node_modules`, `.ddev` state) aren't kept.

use crate::config::Config;
use crate::paths::Paths;
use crate::state::{Db, Job};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// How long a removed worktree can be restored.
// ponytail: a fixed week; make it a setting if someone needs longer.
pub const KEEP_SECS: i64 = 7 * 86400;

pub struct Wt {
    /// The newest job that used this worktree.
    pub job: Job,
    pub path: PathBuf,
    /// Removed: when, and the ref holding its work.
    pub trashed: Option<(i64, String)>,
}

impl Wt {
    /// One line for the TUI and `nightporter worktrees`.
    pub fn line(&self) -> String {
        let day = |t: i64| jiff::Timestamp::from_second(t).map(|t| t.strftime("%m-%d %H:%M").to_string()).unwrap_or_default();
        let state = if let Some((at, _)) = &self.trashed {
            format!("REMOVED {}, restorable until {}", day(*at), day(at + KEEP_SECS))
        } else {
            let used = self.path.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            format!("last changed {}", used.map(|d| day(d.as_secs().cast_signed())).unwrap_or_default())
        };
        format!("#{:<4} {}#{:<5} {:<45} {}", self.job.id, self.job.repo, self.job.number, state, self.path.display())
    }
}

/// Every worktree that's still there, then every removed one that can still be restored, newest
/// first. One `git for-each-ref` per clone.
pub async fn list(db: &Db) -> Result<Vec<Wt>> {
    let jobs = db.recent_jobs(10_000)?;
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Wt> = jobs
        .iter()
        .filter(|j| !j.worktree.is_empty() && seen.insert(j.worktree.clone()) && Path::new(&j.worktree).is_dir())
        .map(|j| Wt { job: j.clone(), path: j.worktree.clone().into(), trashed: None })
        .collect();
    let clones: std::collections::BTreeSet<&str> = jobs.iter().map(|j| j.meta.clone.as_str()).filter(|c| !c.is_empty()).collect();
    let mut trashed = vec![];
    for c in clones {
        for (r, slug, at) in crate::git::trash_refs(Path::new(c)).await.unwrap_or_default() {
            // The newest job in this clone on that branch.
            if let Some(j) = jobs.iter().find(|j| j.meta.clone == c && j.branch.replace('/', "-") == slug) {
                trashed.push(Wt { job: j.clone(), path: j.worktree.clone().into(), trashed: Some((at, r)) });
            }
        }
    }
    trashed.sort_by_key(|w| std::cmp::Reverse(w.trashed.as_ref().map(|t| t.0)));
    out.extend(trashed);
    Ok(out)
}

/// Remove the worktree of job `id` (it stays restorable for a week). Returns a message.
pub async fn remove(paths: &Paths, cfg: &Config, db: &Db, id: i64) -> Result<String> {
    let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    if j.worktree.is_empty() || !Path::new(&j.worktree).is_dir() {
        bail!("job {id} has no worktree left");
    }
    if db.pr_busy(&j.repo, j.number)? {
        bail!("a job on {}#{} is queued, running or waiting for you: finish or dismiss it first", j.repo, j.number);
    }
    let kept = crate::job::remove(paths, cfg, &j).await?;
    Ok(match kept {
        Some(_) => format!("removed {} (u or `nightporter worktrees restore {id}` brings it back for 7 days)", j.worktree),
        None => format!("removed {} (its .git had been changed, so nothing could be kept)", j.worktree),
    })
}

/// Bring back the most recently removed worktree of job `id`'s PR or issue.
pub async fn restore(db: &Db, id: i64) -> Result<String> {
    let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    if db.pr_busy(&j.repo, j.number)? {
        bail!("a job on {}#{} is queued or running: it would reset the worktree", j.repo, j.number);
    }
    let clone = Path::new(&j.meta.clone);
    let slug = j.branch.replace('/', "-");
    let refs = crate::git::trash_refs(clone).await?;
    let (r, ..) = refs
        .iter()
        .filter(|(_, s, _)| *s == slug)
        .max_by_key(|t| t.2)
        .with_context(|| format!("nothing removed to restore for job {id}"))?;
    crate::git::restore(clone, Path::new(&j.worktree), &j.branch, r).await?;
    Ok(format!("restored {}", j.worktree))
}

/// Run an async worktree action from sync code (the TUI).
pub fn block<T>(f: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, cmd: &str) {
        let ok = std::process::Command::new("sh").args(["-c", cmd]).current_dir(dir).status().unwrap().success();
        assert!(ok, "{cmd}");
    }

    #[tokio::test]
    async fn remove_then_restore_keeps_commits_and_uncommitted_changes() {
        let d = tempfile::tempdir().unwrap();
        let clone = d.path().join("clone");
        std::fs::create_dir(&clone).unwrap();
        crate::git::tests::repo(&clone);
        let wt = d.path().join("wt/pr-1");
        let url = clone.to_string_lossy().to_string();
        crate::git::prepare_worktree(&clone, &wt, &url, "refs/heads/main", "nightporter/pr-1").await.unwrap();
        // A local commit (suggest level) plus edits from a handoff, one of them a new file.
        sh(&wt, "echo fix > a.txt && git -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -qam fix");
        sh(&wt, "echo wip >> a.txt && echo new > b.txt");

        let r = crate::git::trash(&clone, &wt, "nightporter/pr-1", 100).await.unwrap().unwrap();
        crate::git::remove_worktree(&clone, &wt, "nightporter/pr-1").await.unwrap();
        assert!(!wt.exists());
        let refs = crate::git::trash_refs(&clone).await.unwrap();
        assert_eq!(refs, vec![(r.clone(), "nightporter-pr-1".into(), 100)]);

        crate::git::restore(&clone, &wt, "nightporter/pr-1", &r).await.unwrap();
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "fix\nwip\n");
        assert_eq!(std::fs::read_to_string(wt.join("b.txt")).unwrap(), "new\n");
        // The local commit is back as a commit, the edits as uncommitted changes.
        let log = crate::git::git(&wt, &["log", "-1", "--format=%s"]).await.unwrap();
        assert_eq!(log, "fix");
        assert_ne!(crate::git::git(&wt, &["status", "--porcelain"]).await.unwrap(), "");
        assert_eq!(crate::git::trash_refs(&clone).await.unwrap(), vec![]);
        // Restoring over a worktree that exists again is refused.
        assert!(crate::git::restore(&clone, &wt, "nightporter/pr-1", &r).await.is_err());
    }
}
