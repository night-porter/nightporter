//! `nightporter events` and `nightporter wait`: follow jobs from scripts, status bars or a
//! terminal (herdr, Rex) without polling the TUI.

use crate::paths::Paths;
use crate::state::{Db, Disposition, Job, JobStatus};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};

fn job_json(j: &Job) -> serde_json::Value {
    serde_json::json!({
        "type": "job",
        "id": j.id,
        "repo": j.repo.to_string(),
        "number": j.number,
        "status": j.status.label(),
        "needs_you": j.status.needs_user(),
        "summary": j.summary.lines().next().unwrap_or_default(),
        "at": j.updated_at,
    })
}

/// Print one line per job state change (and inbox size change) until interrupted.
pub fn events(paths: &Paths, json: bool) -> Result<()> {
    let db = Db::open(&paths.db())?;
    let mut f = Follow::default();
    loop {
        for line in f.changes(&db, json)? {
            println!("{line}");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// What `events` has seen so far; each [`Follow::changes`] call reports what changed since.
#[derive(Default)]
struct Follow {
    seen: HashMap<i64, (JobStatus, i64)>,
    inbox: Option<usize>,
    started: bool,
}

impl Follow {
    fn changes(&mut self, db: &Db, json: bool) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for j in db.recent_jobs(200)?.into_iter().rev() {
            let now = (j.status, j.updated_at);
            // On start, only remember the current state: report changes from here on.
            if self.seen.insert(j.id, now) != Some(now) && self.started {
                out.push(if json {
                    job_json(&j).to_string()
                } else {
                    format!("job {} {}#{}: {}  {}", j.id, j.repo, j.number, j.status.label(), j.summary.lines().next().unwrap_or_default())
                });
            }
        }
        let n = db.events_with(Disposition::Inbox)?.len();
        if self.inbox.is_some_and(|old| old != n) {
            out.push(if json { serde_json::json!({ "type": "inbox", "count": n }).to_string() } else { format!("inbox: {n} waiting") });
        }
        self.inbox = Some(n);
        self.started = true;
        Ok(out)
    }
}

/// How `wait` ended, as a process exit code.
pub fn exit_code(s: JobStatus) -> Option<i32> {
    match s {
        JobStatus::Done => Some(0),
        JobStatus::Failed | JobStatus::Rejected | JobStatus::Dismissed => Some(1),
        s if s.needs_user() => Some(2),
        _ => None,
    }
}

/// Block until job `id` is done (0), failed or rejected (1) or needs you (2); 3 on timeout.
pub fn wait(paths: &Paths, id: i64, timeout: Duration) -> Result<i32> {
    let db = Db::open(&paths.db())?;
    let start = Instant::now();
    loop {
        let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
        if let Some(code) = exit_code(j.status) {
            println!("job {id}: {}", j.status.label());
            return Ok(code);
        }
        if start.elapsed() >= timeout {
            println!("job {id}: still {} after {}", j.status.label(), crate::config::format_duration(timeout));
            return Ok(3);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_exit_codes() {
        assert_eq!(exit_code(JobStatus::Done), Some(0));
        assert_eq!(exit_code(JobStatus::Rejected), Some(1));
        assert_eq!(exit_code(JobStatus::AwaitingApproval), Some(2));
        assert_eq!(exit_code(JobStatus::Running), None);
    }

    fn home() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        (d, paths)
    }

    fn repo() -> crate::forge::RepoId {
        crate::forge::RepoId::new(crate::forge::ForgeKind::GitHub, "github.com", "o/r")
    }

    #[test]
    fn follow_reports_only_changes_after_start() {
        let (_d, paths) = home();
        let mut db = Db::open(&paths.db()).unwrap();
        let old = db.create_job(&repo(), crate::forge::Target::Pr, 1, "my_prs", "draft", &[]).unwrap();
        let mut f = Follow::default();
        assert!(f.changes(&db, false).unwrap().is_empty(), "existing state is not news");
        assert_eq!(f.changes(&db, false).unwrap(), Vec::<String>::new());

        let mut j = db.job(old).unwrap().unwrap();
        j.status = JobStatus::Failed;
        j.summary = "tests broke\nmore".into();
        j.updated_at += 1;
        db.update_job(&j).unwrap();
        db.add_event(&crate::state::tests::ev("e1", 5), Disposition::Inbox, "").unwrap();
        assert_eq!(f.changes(&db, false).unwrap(), [format!("job {old} o/r#1: FAILED  tests broke"), "inbox: 1 waiting".into()]);

        let new = db.create_job(&repo(), crate::forge::Target::Pr, 2, "my_prs", "draft", &[]).unwrap();
        let mut f2 = Follow { started: true, ..Follow::default() };
        f2.inbox = Some(0);
        let lines = f2.changes(&db, true).unwrap();
        let v: Vec<serde_json::Value> = lines.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(v.len(), 3, "{lines:?}");
        assert_eq!((v[1]["id"].as_i64(), v[1]["number"].as_u64(), v[1]["needs_you"].as_bool()), (Some(new), Some(2), Some(false)));
        assert_eq!(v[2], serde_json::json!({"type": "inbox", "count": 1}));
    }

    #[test]
    fn wait_returns_at_once_for_finished_jobs_and_times_out() {
        let (_d, paths) = home();
        let mut db = Db::open(&paths.db()).unwrap();
        let id = db.create_job(&repo(), crate::forge::Target::Pr, 1, "my_prs", "draft", &[]).unwrap();
        assert_eq!(wait(&paths, id, Duration::ZERO).unwrap(), 3, "queued past the timeout");
        db.set_status(id, JobStatus::Done).unwrap();
        assert_eq!(wait(&paths, id, Duration::from_secs(60)).unwrap(), 0);
        assert!(wait(&paths, 99, Duration::ZERO).is_err());
    }
}
