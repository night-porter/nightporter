//! User decisions, shared by the TUI and the CLI (so everything is scriptable and works without
//! the TUI, e.g. with a screen reader).

use crate::checks::blocking_failed as checks_blocking;
use crate::config::edit_file;
use crate::forge::{Control, RepoId};
use crate::paths::Paths;
use crate::state::{Db, Disposition, JobStatus};
use anyhow::{Context, Result, bail};
use std::fmt::Write as _;

#[derive(Clone, Copy)]
pub enum Scope<'a> {
    Repo(&'a RepoId),
    Global,
}

/// Approve a job. Blocking check failures need `override_blocking` (an explicit second step).
pub fn approve(db: &Db, id: i64, override_blocking: bool) -> Result<&'static str> {
    let mut j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    match j.status {
        JobStatus::AwaitingApproval if j.meta.planning => {
            j.meta.planning = false;
            j.status = JobStatus::Queued;
            db.update_job(&j)?;
            Ok("plan approved: the build is queued and follows it")
        }
        JobStatus::AwaitingApproval => {
            if checks_blocking(&j.checks) && !override_blocking {
                bail!("job {id} has blocking check failures; review them and approve with override");
            }
            if !db.transition(id, JobStatus::AwaitingApproval, JobStatus::Approved)? {
                bail!("job {id} changed state; reload");
            }
            Ok(if j.mode == crate::improve::LEARN {
                "approved: the daemon writes the new instructions file"
            } else {
                "approved: the daemon will push and post"
            })
        }
        JobStatus::AwaitingRunApproval => {
            j.meta.run_approved = true;
            j.status = JobStatus::Queued;
            db.update_job(&j)?;
            Ok("run approved: the job is queued")
        }
        JobStatus::Ready => {
            if !db.transition(id, JobStatus::Ready, JobStatus::Done)? {
                bail!("job {id} changed state; reload");
            }
            // Suggest never delivers; say so, or "approve" reads as if it pushed and posted.
            if j.replies.is_empty() && j.diff.is_empty() {
                Ok("marked done")
            } else {
                Ok(
                    "marked done; nothing was pushed or posted (suggest level): copy the diff and replies from `nightporter show`, or raise the level",
                )
            }
        }
        s => bail!("job {id} is {}, nothing to approve", s.label()),
    }
}

pub fn reject(db: &Db, id: i64) -> Result<()> {
    let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    if !j.status.can_reject() {
        bail!("job {id} is {}, nothing to reject", j.status.label());
    }
    // Guarded: the daemon may have started a queued job a moment ago.
    if !db.transition(id, j.status, JobStatus::Rejected)? {
        bail!("job {id} changed state; reload");
    }
    Ok(())
}

/// Re-run a finished or failed job's events as a new job.
pub fn retry(db: &Db, id: i64) -> Result<usize> {
    let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    if j.status.is_active() || j.status.needs_user() {
        bail!("job {id} is still {}", j.status.label());
    }
    anyhow::ensure!(j.mode != crate::improve::LEARN, "a learn job has no requests to retry; start a new one (L on the Repos tab)");
    let mut m = j.meta.clone();
    m.retried = true;
    db.set_meta(id, &m)?;
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
        // The plan is never posted: approve it where you can read it.
        Control::Approve if j.meta.planning => {
            bail!("job {id} waits for its plan: approve it in the TUI or with `nightporter approve {id}`, where you can read it")
        }
        Control::Approve => approve(db, id, false).map(|_| None),
        Control::Dismiss => dismiss(db, id).map(|()| None),
        Control::Retry => retry(db, id).map(|_| None),
        Control::Status => Ok(Some(format!("Job {id}: {}", j.status.label()))),
    }
}

const REPLY_SEP: &str = "<!-- reply ";

/// A job's replies as one Markdown file to edit: each after a `<!-- reply N ... -->` line, without
/// the footer and marker (they're added back on save).
pub fn replies_file(db: &Db, id: i64) -> Result<String> {
    let j = editable(db, id)?;
    if let Some((what, text)) = single_text(&j) {
        return Ok(format!("{SINGLE}{id}, {}: edit {what}, save and quit. -->\n{text}", j.repo));
    }
    let events = db.job_events(id)?;
    let mut out = format!(
        "<!-- Job {id}, {}#{}. Edit the replies, save and quit. Keep each \"{REPLY_SEP}N\" line; empty a reply to drop it. -->\n",
        j.repo, j.number
    );
    for (i, r) in j.replies.iter().enumerate() {
        let to = match r.event.as_deref().and_then(|eid| events.iter().find(|e| e.id == eid)) {
            Some(e) => format!("answers @{}: {}", e.author, e.body.lines().next().unwrap_or_default().chars().take(60).collect::<String>()),
            None if i == 0 && j.meta.review.is_some() => "the review".into(),
            None => "a comment on the PR/issue".into(),
        };
        // `--` can't appear inside an HTML comment; their text is only a hint.
        let _ = write!(out, "\n{REPLY_SEP}{}: {} -->\n{}\n", i + 1, to.replace("--", "-"), crate::prompts::unsign(&r.body));
    }
    Ok(out)
}

/// Save edited replies (from [`replies_file`]): signed again, and the reply checks (secrets,
/// personal data) run on the new text. Returns what changed.
pub fn set_replies(paths: &Paths, db: &Db, id: i64, text: &str) -> Result<String> {
    let mut j = editable(db, id)?;
    if single_text(&j).is_some() {
        return set_single(paths, db, j, text);
    }
    let mut bodies: Vec<(usize, String)> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(REPLY_SEP) {
            let n: usize = rest.split(':').next().and_then(|n| n.trim().parse().ok()).context("a reply line lost its number")?;
            anyhow::ensure!(
                (1..=j.replies.len()).contains(&n) && !bodies.iter().any(|(i, _)| *i == n - 1),
                "reply {n} doesn't exist or is there twice"
            );
            bodies.push((n - 1, String::new()));
        } else if let Some((_, b)) = bodies.last_mut() {
            b.push_str(line);
            b.push('\n');
        }
    }
    anyhow::ensure!(bodies.len() == j.replies.len(), "a \"{REPLY_SEP}N\" line is missing; nothing saved");
    let s = crate::config::Config::load(paths)?.settings_for(paths, &j.repo)?;
    let (mut changed, mut dropped) = (0, 0);
    let mut kept = Vec::new();
    bodies.sort_by_key(|(i, _)| *i);
    for (i, body) in bodies {
        let mut r = j.replies[i].clone();
        let body = body.trim();
        if body.is_empty() {
            anyhow::ensure!(!(i == 0 && j.meta.review.is_some()), "reply 1 is the review; dismiss the job instead (x)");
            dropped += 1;
            continue;
        }
        if body != crate::prompts::unsign(&r.body) {
            changed += 1;
            r.body = crate::prompts::sign(body, s.reply.attribution(), s.agent);
        }
        kept.push(r);
    }
    if changed + dropped == 0 {
        return Ok("no changes".into());
    }
    // What the agent wrote, kept the first time: `nightporter learn` compares it with your edit.
    if j.meta.drafted.is_empty() {
        j.meta.drafted = j.reply_text();
    }
    anyhow::ensure!(!kept.is_empty() || !j.diff.is_empty(), "every reply is empty; dismiss the job instead (x)");
    j.replies = kept;
    let texts: Vec<&str> = j.replies.iter().map(|r| r.body.as_str()).chain([j.summary.as_str()]).collect();
    crate::checks::recheck_replies(&mut j.checks, &texts);
    db.update_job(&j)?;
    let checks = if crate::checks::all_ok(&j.checks) { "checks pass" } else { "checks need you" };
    Ok(format!("job {id}: {changed} repl{} edited, {dropped} dropped; {checks}", if changed == 1 { "y" } else { "ies" }))
}

/// Open the replies in `$VISUAL`/`$EDITOR` and save them (TUI `e`, `nightporter edit`).
pub fn edit_replies(paths: &Paths, db: &Db, id: i64) -> Result<String> {
    let tmp = std::env::temp_dir().join(format!("nightporter-job-{id}-{}.md", std::process::id()));
    std::fs::write(&tmp, replies_file(db, id)?)?;
    let res = match crate::settings::run_editor(&tmp) {
        Ok(true) => std::fs::read_to_string(&tmp).map_err(Into::into).and_then(|t| set_replies(paths, db, id, &t)),
        Ok(false) => Ok("editor quit with an error; nothing saved".into()),
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&tmp);
    res
}

/// Replies can be edited while nothing is posted yet: waiting for approval, or ready to copy.
fn editable(db: &Db, id: i64) -> Result<crate::state::Job> {
    let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    anyhow::ensure!(
        matches!(j.status, JobStatus::AwaitingApproval | JobStatus::Ready),
        "job {id} is {}; only a job waiting for you can be edited",
        j.status.label()
    );
    anyhow::ensure!(j.meta.posted.is_empty(), "job {id} has already posted some replies");
    anyhow::ensure!(!j.replies.is_empty() || single_text(&j).is_some(), "job {id} has no replies to edit");
    Ok(j)
}

const SINGLE: &str = "<!-- Job ";

/// A waiting plan, or a learn job's proposed file: one text to edit instead of replies.
fn single_text(j: &crate::state::Job) -> Option<(&'static str, &str)> {
    if j.meta.planning {
        Some(("the plan the build will follow", &j.meta.plan))
    } else if j.mode == crate::improve::LEARN {
        Some(("the proposed instructions file", &j.meta.proposal))
    } else {
        None
    }
}

fn set_single(paths: &Paths, db: &Db, mut j: crate::state::Job, text: &str) -> Result<String> {
    let body = text.strip_prefix(SINGLE).map_or(text, |t| t.split_once('\n').map_or("", |(_, rest)| rest)).trim();
    anyhow::ensure!(!body.is_empty(), "it's empty; nothing saved (reject or dismiss the job instead)");
    let id = j.id;
    if j.meta.planning {
        if body == j.meta.plan.trim() {
            return Ok("no changes".into());
        }
        if j.meta.drafted_plan.is_empty() {
            j.meta.drafted_plan.clone_from(&j.meta.plan);
        }
        j.meta.plan = body.to_string();
        db.update_waiting(&j)?;
        return Ok(format!("job {id}: plan edited; approve to build it"));
    }
    if body == j.meta.proposal.trim() {
        return Ok("no changes".into());
    }
    j.meta.proposal = format!("{body}\n");
    j.diff = crate::improve::file_diff(&paths.job_dir(id), &j.meta.baseline, &j.meta.proposal);
    j.checks = crate::improve::text_checks(&j.meta.proposal);
    db.update_waiting(&j)?;
    let checks = if crate::checks::all_ok(&j.checks) { "checks pass" } else { "checks need you" };
    Ok(format!("job {id}: proposal edited; {checks}"))
}

/// Queue a job waiting for push approval to pick up your own edits in its worktree (made by hand
/// or in a take-over): the daemon commits them, signs and checks everything again, and the job
/// comes back for approval. No agent runs.
pub fn amend(db: &Db, id: i64) -> Result<&'static str> {
    let mut j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    anyhow::ensure!(
        j.status == JobStatus::AwaitingApproval,
        "job {id} is {}; only a job waiting for push approval can be amended",
        j.status.label()
    );
    anyhow::ensure!(crate::job::pushes_code(j.meta.verb), "job {id} doesn't push code; edit its replies instead (e)");
    anyhow::ensure!(!j.meta.planning && j.mode != crate::improve::LEARN, "job {id} hasn't written code yet; edit it instead (e)");
    anyhow::ensure!(!j.worktree.is_empty() && std::path::Path::new(&j.worktree).exists(), "job {id} has no worktree any more");
    j.meta.amend = true;
    j.status = JobStatus::Queued;
    db.update_job(&j)?;
    Ok("queued: your worktree edits will be committed, signed and checked, then the job waits for you again")
}

/// Put a job away without acting on it: it leaves the Queue, nothing is pushed or posted, and
/// [`restore`] brings it back. Running and queued jobs can't be dismissed (reject those).
pub fn dismiss(db: &Db, id: i64) -> Result<()> {
    let mut j = db.job(id)?.with_context(|| format!("no job {id}"))?;
    match j.status {
        JobStatus::Dismissed => bail!("job {id} is already dismissed"),
        s if !s.can_become(JobStatus::Dismissed) => bail!("job {id} is {}; reject it instead", s.label()),
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
    if !db.ignored_events()?.iter().any(|(e, _)| e.id == event_id)
        || !db.move_event(event_id, Disposition::Dropped, Disposition::Inbox, "")?
    {
        bail!("{event_id} is not an ignored inbox event");
    }
    Ok(())
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
    from_inbox(db, event_id, Disposition::Pending, crate::state::ALLOWED_ONCE)
}

pub fn ignore(db: &Db, event_id: &str) -> Result<()> {
    from_inbox(db, event_id, Disposition::Dropped, "ignored")
}

/// Only an event still waiting in the inbox: allowing a taken one would run it again.
fn from_inbox(db: &Db, event_id: &str, to: Disposition, reason: &str) -> Result<()> {
    if !db.move_event(event_id, Disposition::Inbox, to, reason)? {
        bail!("{event_id} is not waiting in the inbox (already handled?)");
    }
    Ok(())
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
        db.seed_job(&j).unwrap();
        assert!(approve(&db, id, false).is_err());
        approve(&db, id, true).unwrap();
        assert_eq!(db.job(id).unwrap().unwrap().status, JobStatus::Approved);
        // Approving a suggest job only marks it done, and says nothing went out.
        let id = db.create_job(&repo, Target::Pr, 2, "maintainer", "suggest", &[]).unwrap();
        db.set_status(id, JobStatus::Ready).unwrap();
        assert_eq!(approve(&db, id, false).unwrap(), "marked done");
        let id = db.create_job(&repo, Target::Pr, 3, "maintainer", "suggest", &[]).unwrap();
        let mut j = db.job(id).unwrap().unwrap();
        j.status = JobStatus::Ready;
        j.diff = "+x".into();
        db.seed_job(&j).unwrap();
        assert!(approve(&db, id, false).unwrap().contains("nothing was pushed or posted"));
        assert_eq!(db.job(id).unwrap().unwrap().status, JobStatus::Done);
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
        assert!(ignore(&db, "e1").is_ok() && ignore(&db, "e1").is_err(), "only inbox events");
        // Rejecting a job the daemon has just started is refused, not silently overwritten.
        let id = db.create_job(&repo, Target::Pr, 7, "my_prs", "draft", &[]).unwrap();
        db.transition(id, JobStatus::Queued, JobStatus::Running).unwrap();
        assert!(reject(&db, id).is_err());
        assert_eq!(db.job(id).unwrap().unwrap().status, JobStatus::Running);
    }
}
