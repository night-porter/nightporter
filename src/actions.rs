//! User decisions, shared by the TUI and the CLI (so everything is scriptable and works without
//! the TUI, e.g. with a screen reader).

use crate::checks::blocking_failed as checks_blocking;
use crate::config::edit_file;
use crate::forge::{Control, RepoId};
use crate::paths::Paths;
use crate::state::{Db, Disposition, JobStatus};
use anyhow::{Context, Result, bail};

#[derive(Clone, Copy)]
pub enum Scope<'a> {
    Repo(&'a RepoId),
    Global,
}

/// Approve a job. Blocking check failures need `override_blocking` (an explicit second step).
pub fn approve(db: &Db, id: i64, override_blocking: bool) -> Result<&'static str> {
    let mut j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    match j.status {
        JobStatus::AwaitingApproval => {
            if checks_blocking(&j.checks) && !override_blocking {
                bail!("job {id} has blocking check failures; review them and approve with override");
            }
            if !db.transition(id, JobStatus::AwaitingApproval, JobStatus::Approved)? {
                bail!("job {id} changed state; reload");
            }
            Ok("approved: the daemon will push and post")
        }
        JobStatus::AwaitingRunApproval => {
            j.meta.run_approved = true;
            j.status = JobStatus::Queued;
            db.update_job(&j)?;
            Ok("run approved: the job is queued")
        }
        JobStatus::Ready => {
            db.transition(id, JobStatus::Ready, JobStatus::Done)?;
            Ok("marked done")
        }
        s => bail!("job {id} is {}, nothing to approve", s.label()),
    }
}

pub fn reject(db: &Db, id: i64) -> Result<()> {
    let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    if !j.status.needs_user() && j.status != JobStatus::Queued {
        bail!("job {id} is {}", j.status.label());
    }
    db.set_status(id, JobStatus::Rejected)
}

/// Re-run a finished or failed job's events as a new job.
pub fn retry(db: &Db, id: i64) -> Result<usize> {
    let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    if j.status.is_active() || j.status.needs_user() {
        bail!("job {id} is still {}", j.status.label());
    }
    db.requeue_events(id)
}

/// A `/nightporter <command>` you posted on the PR (see `daemon::pr_commands`). Returns a status
/// line to reply with, or `None` to just acknowledge. Stricter than the TUI: `approve` only
/// releases a job waiting for push approval, never past blocking checks, and never approves
/// running someone else's code (that needs the TUI, which shows the changed build files).
pub fn pr_command(db: &Db, id: i64, c: Control) -> Result<Option<String>> {
    let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    match c {
        Control::Approve if j.status != JobStatus::AwaitingApproval => bail!("job {id} is {}, nothing to approve here", j.status.label()),
        Control::Approve => approve(db, id, false).map(|_| None),
        Control::Dismiss => dismiss(db, id).map(|()| None),
        Control::Retry => retry(db, id).map(|_| None),
        Control::Status => Ok(Some(format!("Night Porter job {id}: {}", j.status.label()))),
    }
}

/// Put a job away without acting on it: it leaves the Queue, nothing is pushed or posted, and
/// [`restore`] brings it back. Running and queued jobs can't be dismissed (reject those).
pub fn dismiss(db: &Db, id: i64) -> Result<()> {
    let mut j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    match j.status {
        JobStatus::Dismissed => bail!("job {id} is already dismissed"),
        s if s.is_active() => bail!("job {id} is {}; reject it instead", s.label()),
        s => {
            j.meta.dismissed_from = Some(s);
            j.status = JobStatus::Dismissed;
        }
    }
    db.update_job(&j)
}

/// Undo [`dismiss`]: the job goes back to the status it had.
pub fn restore(db: &Db, id: i64) -> Result<JobStatus> {
    let mut j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    if j.status != JobStatus::Dismissed {
        bail!("job {id} is {}, not dismissed", j.status.label());
    }
    let to = j.meta.dismissed_from.take().unwrap_or(JobStatus::Done);
    // Waiting for approval holds the PR; another job may have started on it meanwhile.
    if matches!(to, JobStatus::AwaitingApproval | JobStatus::AwaitingRunApproval) && db.pr_busy(&j.repo, j.number)? {
        bail!("{}#{} has a newer job; restore that one's history instead", j.repo, j.number);
    }
    j.status = to;
    db.update_job(&j)?;
    Ok(to)
}

/// Delete a finished job for good (with its log and requests). Not undoable.
pub fn purge(db: &mut Db, id: i64) -> Result<()> {
    let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    if j.status.is_active() || j.status.needs_user() {
        bail!("job {id} is {}; dismiss or reject it first", j.status.label());
    }
    db.purge_job(id)
}

/// Undo an inbox ignore: the event waits in the inbox again.
pub fn unignore(db: &Db, event_id: &str) -> Result<()> {
    if !db.ignored_events()?.iter().any(|(e, _)| e.id == event_id) {
        bail!("{event_id} is not an ignored inbox event");
    }
    db.set_disposition(event_id, Disposition::Inbox, "")
}

pub fn set_paused(db: &Db, paused: bool) -> Result<()> {
    db.set("paused", if paused { "1" } else { "0" })
}

pub fn paused(db: &Db) -> bool {
    db.get("paused").ok().flatten().as_deref() == Some("1")
}

impl Scope<'_> {
    fn repo(&self) -> Option<&RepoId> {
        match self {
            Scope::Repo(r) => Some(r),
            Scope::Global => None,
        }
    }
}

fn file_for(paths: &Paths, scope: Scope) -> std::path::PathBuf {
    match scope {
        Scope::Repo(r) => paths.repo_file(r),
        Scope::Global => paths.config_file(),
    }
}

fn push_unique(doc: &mut toml_edit::DocumentMut, scope: Scope, key: &str, user: &str) {
    let trust = match scope {
        Scope::Repo(_) => doc.entry("trust"),
        Scope::Global => {
            let d = doc.entry("defaults").or_insert(toml_edit::table());
            if let Some(t) = d.as_table_mut() {
                t.set_implicit(true);
            }
            d.as_table_mut().expect("defaults table").entry("trust")
        }
    };
    let t = trust.or_insert(toml_edit::table());
    let arr = t[key].or_insert(toml_edit::value(toml_edit::Array::new()));
    if let Some(a) = arr.as_array_mut()
        && !a.iter().any(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(user)))
    {
        a.push(user);
    }
}

/// Inbox: allow this one event only.
pub fn allow_once(db: &Db, event_id: &str) -> Result<()> {
    db.set_disposition(event_id, Disposition::Pending, "allowed once")
}

pub fn ignore(db: &Db, event_id: &str) -> Result<()> {
    db.set_disposition(event_id, Disposition::Dropped, "ignored")
}

/// Inbox: trust this author from now on (repo or global), and release their waiting events.
pub fn allow_always(paths: &Paths, db: &Db, user: &str, scope: Scope) -> Result<usize> {
    edit_file(&file_for(paths, scope), |d| push_unique(d, scope, "allow_users", user))?;
    db.set_author_disposition(user, scope.repo(), Disposition::Pending, "allowed")
}

pub fn deny(paths: &Paths, db: &Db, user: &str, scope: Scope) -> Result<usize> {
    edit_file(&file_for(paths, scope), |d| push_unique(d, scope, "deny_users", user))?;
    db.set_author_disposition(user, scope.repo(), Disposition::Dropped, "denied")
}

pub fn set_subscribed(paths: &Paths, repo: &RepoId, on: bool) -> Result<()> {
    edit_file(&paths.repo_file(repo), |d| d["subscribed"] = toml_edit::value(on))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{ForgeKind, Target};

    #[test]
    fn allow_and_deny_edit_config() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        std::fs::write(paths.config_file(), "# mine\nversion = 1\n").unwrap();
        let db = Db::memory().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        allow_always(&paths, &db, "carol", Scope::Repo(&repo)).unwrap();
        allow_always(&paths, &db, "Carol", Scope::Repo(&repo)).unwrap();
        deny(&paths, &db, "mallory", Scope::Global).unwrap();
        let repo_toml = std::fs::read_to_string(paths.repo_file(&repo)).unwrap();
        assert_eq!(repo_toml.matches("carol").count(), 1, "{repo_toml}");
        let cfg = crate::config::Config::load(&paths).unwrap();
        let s = cfg.settings_for(&paths, &repo).unwrap();
        assert_eq!(s.trust.allow_users, vec!["carol"]);
        assert_eq!(s.trust.deny_users, vec!["mallory"]);
        assert!(std::fs::read_to_string(paths.config_file()).unwrap().starts_with("# mine"));
        set_subscribed(&paths, &repo, true).unwrap();
        assert!(cfg.settings_for(&paths, &repo).unwrap().subscribed);
    }

    #[test]
    fn approvals() {
        let mut db = Db::memory().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&repo, Target::Pr, 1, "my_prs", "draft", &[]).unwrap();
        assert!(approve(&db, id, false).is_err());
        let mut j = db.job(id).unwrap().unwrap();
        j.status = JobStatus::AwaitingApproval;
        j.checks = vec![crate::state::CheckResult { name: "secrets".into(), ok: false, blocking: true, detail: String::new() }];
        db.update_job(&j).unwrap();
        assert!(approve(&db, id, false).is_err());
        approve(&db, id, true).unwrap();
        assert_eq!(db.job(id).unwrap().unwrap().status, JobStatus::Approved);
    }

    #[test]
    fn dismiss_restore_purge() {
        let mut db = Db::memory().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&repo, Target::Pr, 1, "my_prs", "draft", &[]).unwrap();
        assert!(dismiss(&db, id).is_err(), "queued jobs are rejected, not dismissed");
        db.set_status(id, JobStatus::AwaitingApproval).unwrap();
        dismiss(&db, id).unwrap();
        assert_eq!(db.job(id).unwrap().unwrap().status, JobStatus::Dismissed);
        assert!(!db.pr_busy(&repo, 1).unwrap(), "a dismissed job frees the PR");
        assert!(purge(&mut db, id).is_ok_and(|()| db.job(id).unwrap().is_none()));
        let id = db.create_job(&repo, Target::Pr, 1, "my_prs", "draft", &[]).unwrap();
        db.set_status(id, JobStatus::AwaitingApproval).unwrap();
        dismiss(&db, id).unwrap();
        assert_eq!(restore(&db, id).unwrap(), JobStatus::AwaitingApproval);
        assert!(restore(&db, id).is_err());
        assert!(purge(&mut db, id).is_err(), "only finished jobs can be purged");
        // Restoring into a PR that has a newer pending job is refused.
        dismiss(&db, id).unwrap();
        let newer = db.create_job(&repo, Target::Pr, 1, "my_prs", "draft", &[]).unwrap();
        assert!(restore(&db, id).is_err());
        db.set_status(newer, JobStatus::Done).unwrap();
        assert!(restore(&db, id).is_ok());
        // Ignored inbox events can come back.
        db.add_event(&crate::state::tests::ev("e1", 5), Disposition::Inbox, "").unwrap();
        ignore(&db, "e1").unwrap();
        assert_eq!(db.ignored_events().unwrap().len(), 1);
        unignore(&db, "e1").unwrap();
        assert_eq!(db.events_with(Disposition::Inbox).unwrap().len(), 1);
        assert!(unignore(&db, "e1").is_err());
    }
}
