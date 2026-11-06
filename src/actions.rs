//! User decisions, shared by the TUI and the CLI (so everything is scriptable and works without
//! the TUI, e.g. with a screen reader).

use crate::checks::blocking_failed as checks_blocking;
use crate::config::edit_file;
use crate::forge::RepoId;
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
}
