//! SQLite state shared by the daemon and any number of TUIs (WAL mode).

use crate::forge::{Event, RepoId, Target};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub struct Db(pub Connection);

/// How an event was classified at intake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Trusted; will be batched into a job.
    Pending,
    /// Untrusted author; waits in the inbox.
    Inbox,
    /// Taken by a job.
    Taken,
    /// Ignored (filtered, denied or dismissed). Kept for history.
    Dropped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    /// Waiting for the user to approve push/post (draft or escalated).
    AwaitingApproval,
    /// Approved in the TUI; the daemon will push/post.
    Approved,
    /// `suggest` level: result ready for the user to copy.
    Ready,
    /// Needs the user's OK to run untrusted code (env setup / tests).
    AwaitingRunApproval,
    Done,
    Failed,
    Rejected,
    /// Put away without acting (nothing pushed or posted). Undo with restore.
    Dismissed,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::AwaitingApproval => "awaiting_approval",
            JobStatus::Approved => "approved",
            JobStatus::Ready => "ready",
            JobStatus::AwaitingRunApproval => "awaiting_run_approval",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Rejected => "rejected",
            JobStatus::Dismissed => "dismissed",
        }
    }
    pub fn parse(s: &str) -> Self {
        serde_json::from_value(serde_json::Value::String(s.into())).unwrap_or(JobStatus::Failed)
    }
    /// Human label for the TUI (never colour-only).
    pub fn label(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::AwaitingApproval => "NEEDS APPROVAL",
            JobStatus::Approved => "approved, pushing",
            JobStatus::Ready => "ready to copy",
            JobStatus::AwaitingRunApproval => "NEEDS RUN APPROVAL",
            JobStatus::Done => "done",
            JobStatus::Failed => "FAILED",
            JobStatus::Rejected => "rejected",
            JobStatus::Dismissed => "dismissed",
        }
    }
    pub fn needs_user(self) -> bool {
        matches!(self, JobStatus::AwaitingApproval | JobStatus::AwaitingRunApproval | JobStatus::Ready)
    }
    pub fn is_active(self) -> bool {
        matches!(self, JobStatus::Queued | JobStatus::Running | JobStatus::Approved)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    /// Blocking checks can't be approved past; only escalations can.
    pub blocking: bool,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: i64,
    pub repo: RepoId,
    pub target: Target,
    pub number: u64,
    pub mode: String,
    pub level: String,
    pub status: JobStatus,
    pub attempt: u32,
    pub created_at: i64,
    pub updated_at: i64,
    pub branch: String,
    pub head_before: String,
    pub head_after: String,
    pub summary: String,
    pub diff: String,
    pub replies: Vec<crate::forge::Reply>,
    pub checks: Vec<CheckResult>,
    pub error: String,
    pub worktree: String,
    pub meta: JobMeta,
}

impl Job {
    /// Someone else's code you haven't approved running at this head: nothing of yours (agent
    /// commands, env hooks, a take-over) may run on it.
    pub fn unapproved_code(&self) -> bool {
        self.meta.untrusted_code && !(self.meta.run_approved && self.meta.run_approved_head == self.head_before)
    }
}

/// Extra per-job data needed to deliver after approval.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct JobMeta {
    #[serde(default)]
    pub push_url: String,
    #[serde(default)]
    pub push_ref: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub verb: Option<crate::forge::Verb>,
    #[serde(default)]
    pub clone: String,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub run_approved: bool,
    /// The head SHA the run approval is for; any other head needs a new approval.
    #[serde(default)]
    pub run_approved_head: String,
    #[serde(default)]
    pub untrusted_code: bool,
    #[serde(default)]
    pub build_files: Vec<String>,
    #[serde(default)]
    pub pr_url: String,
    /// The PR/MR or issue title, for notifications.
    #[serde(default)]
    pub title: String,
    /// Indexes of replies already posted (delivery can be retried without double-posting).
    #[serde(default)]
    pub posted: Vec<usize>,
    /// The status a dismissed job had, so restore can put it back.
    #[serde(default)]
    pub dismissed_from: Option<JobStatus>,
}

pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let c = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        Self::init(c)
    }

    #[cfg(test)]
    pub fn memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(c: Connection) -> Result<Self> {
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS events(
               id TEXT PRIMARY KEY, repo TEXT NOT NULL, target TEXT NOT NULL, number INTEGER NOT NULL,
               author TEXT NOT NULL, disposition TEXT NOT NULL, reason TEXT NOT NULL DEFAULT '',
               job_id INTEGER, seen_at INTEGER NOT NULL, json TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS events_pending ON events(disposition, repo, number);
             CREATE TABLE IF NOT EXISTS jobs(
               id INTEGER PRIMARY KEY AUTOINCREMENT, repo TEXT NOT NULL, target TEXT NOT NULL,
               number INTEGER NOT NULL, mode TEXT NOT NULL, level TEXT NOT NULL, status TEXT NOT NULL,
               attempt INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
               branch TEXT NOT NULL DEFAULT '', head_before TEXT NOT NULL DEFAULT '',
               head_after TEXT NOT NULL DEFAULT '', summary TEXT NOT NULL DEFAULT '',
               diff TEXT NOT NULL DEFAULT '', replies TEXT NOT NULL DEFAULT '[]',
               checks TEXT NOT NULL DEFAULT '[]', error TEXT NOT NULL DEFAULT '',
               worktree TEXT NOT NULL DEFAULT '', meta TEXT NOT NULL DEFAULT '{}');
             CREATE INDEX IF NOT EXISTS jobs_status ON jobs(status);
             CREATE INDEX IF NOT EXISTS jobs_pr ON jobs(repo, number);
             CREATE TABLE IF NOT EXISTS job_log(job_id INTEGER NOT NULL, ts INTEGER NOT NULL, line TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS job_log_job ON job_log(job_id);
             CREATE TABLE IF NOT EXISTS kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        )?;
        Ok(Db(c))
    }

    // ---- kv ----
    pub fn get(&self, k: &str) -> Result<Option<String>> {
        Ok(self.0.query_row("SELECT v FROM kv WHERE k=?1", [k], |r| r.get(0)).optional()?)
    }
    pub fn set(&self, k: &str, v: &str) -> Result<()> {
        self.0.execute("INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v", [k, v])?;
        Ok(())
    }
    pub fn get_json<T: for<'de> Deserialize<'de> + Default>(&self, k: &str) -> Result<T> {
        Ok(self.get(k)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
    }
    pub fn set_json<T: Serialize>(&self, k: &str, v: &T) -> Result<()> {
        self.set(k, &serde_json::to_string(v)?)
    }

    // ---- events ----
    /// Insert if new. Returns false when the event was already seen.
    /// How many events from `author` on `repo` are waiting in the inbox.
    pub fn inbox_count(&self, repo: &RepoId, author: &str) -> Result<usize> {
        let n: i64 = self.0.query_row(
            "SELECT COUNT(*) FROM events WHERE disposition = 'inbox' AND repo = ?1 AND author = ?2 COLLATE NOCASE",
            params![repo.key(), author],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n)?)
    }

    pub fn has_event(&self, id: &str) -> Result<bool> {
        Ok(self.0.query_row("SELECT 1 FROM events WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some())
    }

    pub fn add_event(&self, e: &Event, d: Disposition, reason: &str) -> Result<bool> {
        let n = self.0.execute(
            "INSERT OR IGNORE INTO events(id,repo,target,number,author,disposition,reason,seen_at,json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                e.id,
                e.repo.key(),
                enum_str(&e.target),
                // Forge PR/issue numbers are far below 2^63; stored as-is and read back with cast_unsigned.
                e.number.cast_signed(),
                e.author,
                enum_str(&d),
                reason,
                now(),
                serde_json::to_string(e)?
            ],
        )?;
        Ok(n == 1)
    }

    pub fn set_disposition(&self, id: &str, d: Disposition, reason: &str) -> Result<()> {
        self.0.execute("UPDATE events SET disposition=?2, reason=?3 WHERE id=?1", params![id, enum_str(&d), reason])?;
        Ok(())
    }

    /// Re-classify every inbox event by `author` (after "allow always" / "deny").
    /// `repo`: only that repo's events (a per-repo decision must not release other repos').
    pub fn set_author_disposition(&self, author: &str, repo: Option<&RepoId>, d: Disposition, reason: &str) -> Result<usize> {
        Ok(self.0.execute(
            "UPDATE events SET disposition=?2, reason=?3 WHERE disposition='inbox' AND author=?1 COLLATE NOCASE
               AND (?4 IS NULL OR repo=?4)",
            params![author, enum_str(&d), reason, repo.map(RepoId::key)],
        )?)
    }

    pub fn events_with(&self, d: Disposition) -> Result<Vec<(Event, i64)>> {
        self.events_where(d, "ORDER BY seen_at")
    }

    /// Inbox events you ignored (not those dropped by filters or a deny), newest first.
    pub fn ignored_events(&self) -> Result<Vec<(Event, i64)>> {
        self.events_where(Disposition::Dropped, "AND reason='ignored' ORDER BY seen_at DESC")
    }

    /// `tail`: a fixed SQL suffix (conditions and order), never user input.
    fn events_where(&self, d: Disposition, tail: &str) -> Result<Vec<(Event, i64)>> {
        let mut st = self.0.prepare(&format!("SELECT json, seen_at FROM events WHERE disposition=?1 {tail}"))?;
        let rows = st.query_map([enum_str(&d)], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (json, seen) = row?;
            out.push((serde_json::from_str(&json)?, seen));
        }
        Ok(out)
    }

    /// Put a job's events back in the queue (e.g. the branch moved under us).
    /// A re-queued CI-fix doesn't count as a new attempt: give the attempt back.
    pub fn requeue_events(&self, job_id: i64) -> Result<usize> {
        let evs = self.job_events(job_id)?;
        if let Some(e) = evs.iter().find(|e| e.trigger == crate::forge::Trigger::CiFailure) {
            let n = self.ci_attempts(&e.repo, e.number)?;
            self.set_ci_attempts(&e.repo, e.number, n.saturating_sub(1))?;
        }
        Ok(self.0.execute("UPDATE events SET disposition='pending', job_id=NULL, seen_at=?2 WHERE job_id=?1", params![job_id, now()])?)
    }

    pub fn job_events(&self, job_id: i64) -> Result<Vec<Event>> {
        let mut st = self.0.prepare("SELECT json FROM events WHERE job_id=?1 ORDER BY seen_at")?;
        let rows = st.query_map([job_id], |r| r.get::<_, String>(0))?;
        rows.map(|j| Ok(serde_json::from_str(&j?)?)).collect()
    }

    /// Give a running job one more event, whatever happened to it before (never seen, dropped,
    /// still pending): the job found it on the PR itself.
    pub fn take_event(&self, job_id: i64, e: &Event) -> Result<()> {
        self.add_event(e, Disposition::Taken, "open review thread")?;
        self.0.execute("UPDATE events SET disposition='taken', job_id=?2 WHERE id=?1", params![e.id, job_id])?;
        Ok(())
    }

    // ---- jobs ----
    /// Create a job and atomically take the given pending events.
    pub fn create_job(&mut self, repo: &RepoId, target: Target, number: u64, mode: &str, level: &str, event_ids: &[String]) -> Result<i64> {
        let tx = self.0.transaction()?;
        let attempt: u32 = tx.query_row(
            "SELECT COUNT(*) FROM jobs WHERE repo=?1 AND number=?2 AND status IN ('failed','done') AND created_at > ?3",
            params![repo.key(), number.cast_signed(), now() - 86400],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO jobs(repo,target,number,mode,level,status,attempt,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,'queued',?6,?7,?7)",
            params![repo.key(), enum_str(&target), number.cast_signed(), mode, level, attempt + 1, now()],
        )?;
        let id = tx.last_insert_rowid();
        for e in event_ids {
            tx.execute("UPDATE events SET disposition='taken', job_id=?2 WHERE id=?1", params![e, id])?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn set_meta(&self, id: i64, meta: &JobMeta) -> Result<()> {
        self.0.execute("UPDATE jobs SET meta=?2 WHERE id=?1", params![id, serde_json::to_string(meta)?])?;
        Ok(())
    }

    pub fn set_status(&self, id: i64, s: JobStatus) -> Result<()> {
        self.0.execute("UPDATE jobs SET status=?2, updated_at=?3 WHERE id=?1", params![id, s.as_str(), now()])?;
        Ok(())
    }

    /// Transition only if the job is currently in `from` (prevents TUI/daemon races).
    pub fn transition(&self, id: i64, from: JobStatus, to: JobStatus) -> Result<bool> {
        Ok(self.0.execute(
            "UPDATE jobs SET status=?3, updated_at=?4 WHERE id=?1 AND status=?2",
            params![id, from.as_str(), to.as_str(), now()],
        )? == 1)
    }

    pub fn update_job(&self, j: &Job) -> Result<()> {
        self.0.execute(
            "UPDATE jobs SET status=?2, level=?3, branch=?4, head_before=?5, head_after=?6, summary=?7, diff=?8,
             replies=?9, checks=?10, error=?11, worktree=?12, updated_at=?13, meta=?14 WHERE id=?1",
            params![
                j.id,
                j.status.as_str(),
                j.level,
                j.branch,
                j.head_before,
                j.head_after,
                j.summary,
                j.diff,
                serde_json::to_string(&j.replies)?,
                serde_json::to_string(&j.checks)?,
                j.error,
                j.worktree,
                now(),
                serde_json::to_string(&j.meta)?
            ],
        )?;
        Ok(())
    }

    /// Delete a job, its log, and the events it took. Permanent.
    pub fn purge_job(&mut self, id: i64) -> Result<()> {
        let tx = self.0.transaction()?;
        tx.execute("DELETE FROM job_log WHERE job_id=?1", [id])?;
        tx.execute("DELETE FROM events WHERE job_id=?1", [id])?;
        tx.execute("DELETE FROM jobs WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn fail(&self, id: i64, err: &str) -> Result<()> {
        self.0.execute("UPDATE jobs SET status='failed', error=?2, updated_at=?3 WHERE id=?1", params![id, err, now()])?;
        Ok(())
    }

    pub fn job(&self, id: i64) -> Result<Option<Job>> {
        Ok(self.0.query_row(&format!("{JOB_SELECT} WHERE id=?1"), [id], row_job).optional()?)
    }

    pub fn jobs_with(&self, s: JobStatus) -> Result<Vec<Job>> {
        let mut st = self.0.prepare(&format!("{JOB_LIST} WHERE status=?1 ORDER BY id"))?;
        let rows = st.query_map([s.as_str()], row_job)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn recent_jobs(&self, limit: usize) -> Result<Vec<Job>> {
        let mut st = self.0.prepare(&format!("{JOB_LIST} ORDER BY id DESC LIMIT ?1"))?;
        let rows = st.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], row_job)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The newest job on each PR/issue that a `/nightporter` command on the forge may act on:
    /// waiting for you, or updated since `since` (bounds the API calls per poll).
    pub fn command_jobs(&self, since: i64) -> Result<Vec<Job>> {
        let mut st = self.0.prepare(&format!(
            "{JOB_LIST} WHERE id IN (SELECT MAX(id) FROM jobs GROUP BY repo, target, number)
               AND (status IN ('awaiting_approval','awaiting_run_approval','ready') OR updated_at >= ?1) ORDER BY id"
        ))?;
        let rows = st.query_map([since], row_job)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Any job on this PR that isn't finished (one job per PR at a time).
    pub fn pr_busy(&self, repo: &RepoId, number: u64) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE repo=?1 AND number=?2
               AND status IN ('queued','running','approved','awaiting_approval','awaiting_run_approval'))",
            params![repo.key(), number.cast_signed()],
            |r| r.get(0),
        )?)
    }

    pub fn jobs_since(&self, since: i64) -> Result<u32> {
        Ok(self.0.query_row("SELECT COUNT(*) FROM jobs WHERE created_at >= ?1", [since], |r| r.get(0))?)
    }

    /// Consecutive CI-fix attempts on a PR. Reset when CI goes green (see the sweep).
    pub fn ci_attempts(&self, repo: &RepoId, number: u64) -> Result<u32> {
        Ok(self.get(&format!("ci:{}#{number}", repo.key()))?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }
    pub fn set_ci_attempts(&self, repo: &RepoId, number: u64, n: u32) -> Result<()> {
        self.set(&format!("ci:{}#{number}", repo.key()), &n.to_string())
    }

    /// Jobs started by a review bot's comments on a PR. Never reset: a cap per PR.
    pub fn bot_rounds(&self, repo: &RepoId, number: u64) -> Result<u32> {
        Ok(self.get(&format!("bot:{}#{number}", repo.key()))?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }
    pub fn set_bot_rounds(&self, repo: &RepoId, number: u64, n: u32) -> Result<()> {
        self.set(&format!("bot:{}#{number}", repo.key()), &n.to_string())
    }

    pub fn log(&self, job: i64, line: &str) -> Result<()> {
        self.0.execute("INSERT INTO job_log(job_id,ts,line) VALUES(?1,?2,?3)", params![job, now(), line])?;
        Ok(())
    }

    pub fn job_log(&self, job: i64) -> Result<Vec<(i64, String)>> {
        let mut st = self.0.prepare("SELECT ts, line FROM job_log WHERE job_id=?1 ORDER BY rowid")?;
        let rows = st.query_map([job], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// On daemon start: anything left `running` died with the previous daemon.
    /// Jobs left `running` died with the previous daemon: fail them and re-queue their events.
    /// (`approved` jobs are simply delivered again; delivery is idempotent.)
    pub fn recover(&self) -> Result<usize> {
        let ids: Vec<i64> =
            self.0.prepare("SELECT id FROM jobs WHERE status='running'")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        for id in ids {
            self.requeue_events(id)?;
        }
        Ok(self.0.execute(
            "UPDATE jobs SET status='failed', error='daemon restarted while job was running; re-queued', updated_at=?1
             WHERE status='running'",
            [now()],
        )?)
    }

    pub fn prune(&self, retention_days: u32) -> Result<()> {
        let cutoff = now() - i64::from(retention_days) * 86400;
        self.0.execute("DELETE FROM job_log WHERE ts < ?1", [cutoff])?;
        self.0
            .execute("DELETE FROM jobs WHERE updated_at < ?1 AND status IN ('done','failed','rejected','ready','dismissed')", [cutoff])?;
        self.0.execute("DELETE FROM events WHERE seen_at < ?1 AND disposition IN ('taken','dropped','inbox')", [cutoff])?;
        Ok(())
    }
}

const JOB_SELECT: &str = "SELECT id, repo, target, number, mode, level, status, attempt, created_at, updated_at,
  branch, head_before, head_after, summary, diff, replies, checks, error, worktree, meta FROM jobs";
/// Same columns without the diff (can be megabytes). For lists only: never pass a job loaded
/// this way to `update_job`, it would clear the stored diff.
const JOB_LIST: &str = "SELECT id, repo, target, number, mode, level, status, attempt, created_at, updated_at,
  branch, head_before, head_after, summary, '' AS diff, replies, checks, error, worktree, meta FROM jobs";

fn row_job(r: &rusqlite::Row) -> rusqlite::Result<Job> {
    let repo: String = r.get(1)?;
    let target: String = r.get(2)?;
    Ok(Job {
        id: r.get(0)?,
        repo: RepoId::parse_key(&repo).ok_or(rusqlite::Error::InvalidQuery)?,
        target: if target == "issue" { Target::Issue } else { Target::Pr },
        // Written with cast_signed: the bit-exact inverse.
        number: r.get::<_, i64>(3)?.cast_unsigned(),
        mode: r.get(4)?,
        level: r.get(5)?,
        status: JobStatus::parse(&r.get::<_, String>(6)?),
        attempt: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        branch: r.get(10)?,
        head_before: r.get(11)?,
        head_after: r.get(12)?,
        summary: r.get(13)?,
        diff: r.get(14)?,
        replies: serde_json::from_str(&r.get::<_, String>(15)?).unwrap_or_default(),
        checks: serde_json::from_str(&r.get::<_, String>(16)?).unwrap_or_default(),
        error: r.get(17)?,
        worktree: r.get(18)?,
        meta: serde_json::from_str(&r.get::<_, String>(19)?).unwrap_or_default(),
    })
}

/// Serialise a unit enum to its serde name.
pub fn enum_str<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::forge::{ForgeKind, Role, Trigger};

    pub fn ev(id: &str, n: u64) -> Event {
        Event {
            id: id.into(),
            repo: RepoId::new(ForgeKind::GitHub, "github.com", "o/r"),
            target: Target::Pr,
            number: n,
            trigger: Trigger::ReviewComment,
            author: "alice".into(),
            author_id: 0,
            role: Role::Member,
            is_bot: false,
            body: "fix".into(),
            url: String::new(),
            thread: None,
            path: None,
            line: None,
            created_at: String::new(),
            verb: None,
        }
    }

    #[test]
    fn events_dedupe_and_jobs_take_them() {
        let mut db = Db::memory().unwrap();
        let e = ev("a", 1);
        assert!(db.add_event(&e, Disposition::Pending, "").unwrap());
        assert!(!db.add_event(&e, Disposition::Pending, "").unwrap());
        db.add_event(&ev("b", 1), Disposition::Inbox, "").unwrap();
        assert_eq!(db.events_with(Disposition::Pending).unwrap().len(), 1);
        let id = db.create_job(&e.repo, Target::Pr, 1, "my_prs", "draft", &["a".into()]).unwrap();
        assert!(db.pr_busy(&e.repo, 1).unwrap());
        assert_eq!(db.events_with(Disposition::Pending).unwrap().len(), 0);
        assert_eq!(db.job_events(id).unwrap(), vec![e.clone()]);
        let other = RepoId::new(ForgeKind::GitHub, "github.com", "x/y");
        assert_eq!(db.set_author_disposition("ALICE", Some(&other), Disposition::Pending, "allow").unwrap(), 0);
        assert_eq!(db.set_author_disposition("ALICE", Some(&e.repo), Disposition::Pending, "allow").unwrap(), 1);
    }

    #[test]
    fn transitions_are_guarded() {
        let mut db = Db::memory().unwrap();
        let r = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&r, Target::Pr, 2, "my_prs", "draft", &[]).unwrap();
        assert!(!db.transition(id, JobStatus::AwaitingApproval, JobStatus::Approved).unwrap());
        db.set_status(id, JobStatus::AwaitingApproval).unwrap();
        assert!(db.transition(id, JobStatus::AwaitingApproval, JobStatus::Approved).unwrap());
        assert_eq!(db.job(id).unwrap().unwrap().status, JobStatus::Approved);
        for s in [JobStatus::AwaitingRunApproval, JobStatus::Ready, JobStatus::Rejected] {
            assert_eq!(JobStatus::parse(s.as_str()), s);
        }
    }

    #[test]
    fn recover_fails_running() {
        let mut db = Db::memory().unwrap();
        let r = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&r, Target::Pr, 3, "my_prs", "draft", &[]).unwrap();
        db.set_status(id, JobStatus::Running).unwrap();
        assert_eq!(db.recover().unwrap(), 1);
        assert_eq!(db.job(id).unwrap().unwrap().status, JobStatus::Failed);
        // Its events are pending again.
        let mut db2 = Db::memory().unwrap();
        db2.add_event(&ev("x", 9), Disposition::Pending, "").unwrap();
        let j = db2.create_job(&r, Target::Pr, 9, "my_prs", "draft", &["x".into()]).unwrap();
        db2.set_status(j, JobStatus::Running).unwrap();
        db2.recover().unwrap();
        assert_eq!(db2.events_with(Disposition::Pending).unwrap().len(), 1);
    }
}
