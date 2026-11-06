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
    let mut seen: HashMap<i64, (JobStatus, i64)> = HashMap::new();
    let mut inbox: Option<usize> = None;
    let mut first = true;
    loop {
        for j in db.recent_jobs(200)?.into_iter().rev() {
            let now = (j.status, j.updated_at);
            // On start, only remember the current state: report changes from here on.
            if seen.insert(j.id, now) != Some(now) && !first {
                if json {
                    println!("{}", job_json(&j));
                } else {
                    println!(
                        "job {} {}#{}: {}  {}",
                        j.id,
                        j.repo,
                        j.number,
                        j.status.label(),
                        j.summary.lines().next().unwrap_or_default()
                    );
                }
            }
        }
        let n = db.events_with(Disposition::Inbox)?.len();
        if inbox.is_some_and(|old| old != n) {
            if json {
                println!("{}", serde_json::json!({ "type": "inbox", "count": n }));
            } else {
                println!("inbox: {n} waiting");
            }
        }
        inbox = Some(n);
        first = false;
        std::thread::sleep(Duration::from_secs(1));
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
}
