//! Getting better over time: what jobs cost and how often you had to fix them (`nightporter
//! stats`), and learning from those fixes (`nightporter learn`): a job that proposes a better
//! instructions file for one repo, from your edits, amends, rejections and dismissals.

use crate::config::{JobKind, RepoSettings};
use crate::daemon::Ctx;
use crate::forge::{RepoId, Target};
use crate::notify::{self, Kind};
use crate::paths::Paths;
use crate::state::{Db, Job, JobStatus, now};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The `mode` of a learn job (it has no PR: number 0).
pub const LEARN: &str = "learn";
/// Fixes since the last learn before Night Porter reminds you, and how often (per repo).
const NUDGE_AT: usize = 5;
const NUDGE_EVERY: i64 = 7 * 86400;
/// Most recent fixes one learn job reads.
const MAX_FIXES: usize = 20;
/// Jobs read for stats and fixes. ponytail: a scan of recent rows; an index on `created_at` if it ever gets slow.
const SCAN: usize = 5000;

/// The model in agent args (`--model x`, `--model=x`, `-m x`), or empty for the agent's default.
pub fn model_of(args: &[String]) -> String {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(m) = a.strip_prefix("--model=") {
            return m.into();
        }
        if a == "--model" || a == "-m" {
            return it.next().cloned().unwrap_or_default();
        }
    }
    String::new()
}

fn pushes(j: &Job) -> bool {
    j.status == JobStatus::Done
        && j.level != "suggest"
        && !j.head_after.is_empty()
        && j.head_after != j.head_before
        && crate::job::pushes_code(j.meta.verb)
}

/// You corrected this job: edited what it wrote, changed its code, or turned it down.
fn is_fix(j: &Job) -> bool {
    !j.meta.drafted.is_empty()
        || !j.meta.drafted_plan.is_empty()
        || !j.meta.amend_diff.is_empty()
        || j.status == JobStatus::Rejected
        || (j.status == JobStatus::Dismissed && matches!(j.meta.dismissed_from, Some(JobStatus::AwaitingApproval | JobStatus::Ready)))
}

/// Fixes on `repo` no approved learn has read yet, newest first. Only jobs that started from
/// trusted people: not someone else's code or issue, no CI output, and nothing let in from the inbox.
pub fn fixes(db: &Db, repo: &RepoId) -> Result<Vec<Job>> {
    Ok(fixes_in(&db.recent_jobs(SCAN)?, repo).into_iter().cloned().collect())
}

fn fixes_in<'a>(jobs: &'a [Job], repo: &RepoId) -> Vec<&'a Job> {
    // Fail closed: `agent` is set when a job runs on this version, after its trust flags (older
    // jobs have none). The flags are on the job itself, so a retry that detaches its events can't
    // launder it.
    let trusted = |j: &Job| !j.meta.agent.is_empty() && !j.meta.untrusted_code && !j.meta.untrusted_input;
    jobs.iter().filter(|j| j.repo == *repo && j.mode != LEARN && !j.meta.learned && is_fix(j) && trusted(j)).collect()
}

#[derive(Default)]
struct Row {
    jobs: u32,
    done: u32,
    pushed: u32,
    rejected: u32,
    dismissed: u32,
    failed: u32,
    edited: u32,
    amended: u32,
    took_over: u32,
    retried: u32,
    cost: f64,
}

impl Row {
    fn add(&mut self, j: &Job) {
        self.jobs += 1;
        match j.status {
            JobStatus::Done => self.done += 1,
            JobStatus::Rejected => self.rejected += 1,
            JobStatus::Dismissed => self.dismissed += 1,
            JobStatus::Failed => self.failed += 1,
            _ => {}
        }
        self.pushed += u32::from(pushes(j));
        self.edited += u32::from(!j.meta.drafted.is_empty() || !j.meta.drafted_plan.is_empty());
        self.amended += u32::from(!j.meta.amend_diff.is_empty());
        self.took_over += u32::from(j.meta.took_over);
        self.retried += u32::from(j.meta.retried);
        self.cost += j.meta.cost_usd.unwrap_or(0.0);
    }

    fn line(&self, name: &str) -> String {
        let per = if self.pushed > 0 { format!("${:.2}", self.cost / f64::from(self.pushed)) } else { "-".into() };
        let n = [
            self.jobs,
            self.done,
            self.pushed,
            self.rejected,
            self.dismissed,
            self.failed,
            self.edited,
            self.amended,
            self.took_over,
            self.retried,
        ];
        row(name, n.iter().map(ToString::to_string).chain([format!("${:.2}", self.cost), per]))
    }
}

/// Full words, right-aligned over their numbers (a screen reader reads the header as written).
const COLS: [&str; 10] = ["jobs", "done", "pushed", "rejected", "dismissed", "failed", "edited", "amended", "took over", "retried"];

fn row(name: &str, cells: impl Iterator<Item = String>) -> String {
    let mut s = format!("  {:<34}", name.chars().take(34).collect::<String>());
    for (c, w) in cells.zip(COLS.iter().map(|h| h.len()).chain([8, 8])) {
        let _ = write!(s, " {c:>w$}");
    }
    s
}

/// `nightporter stats`: the last `days` days, for one repo or all, by repo, kind of job and
/// agent/model, plus the repos with fixes to learn from.
pub fn stats(db: &Db, repo: Option<&RepoId>, days: u32) -> Result<String> {
    let since = now() - i64::from(days) * 86400;
    let all = db.recent_jobs(SCAN)?;
    let jobs: Vec<&Job> = all.iter().filter(|j| j.created_at >= since && j.mode != LEARN && repo.is_none_or(|r| j.repo == *r)).collect();
    let mut s = String::new();
    if jobs.is_empty() {
        let _ = writeln!(s, "No jobs in the last {days} days.");
        return Ok(s);
    }
    let mut total = Row::default();
    let (mut by_repo, mut by_kind, mut by_model) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    for &j in &jobs {
        total.add(j);
        by_repo.entry(j.repo.to_string()).or_insert_with(Row::default).add(j);
        let kind = j.meta.kind.map_or_else(|| "mixed or older".into(), |k| crate::state::enum_str(&k));
        by_kind.entry(kind).or_insert_with(Row::default).add(j);
        let model = match (j.meta.agent.as_str(), j.meta.model.as_str()) {
            ("", _) => "older jobs".to_string(),
            (a, "") => format!("{a} (default model)"),
            (a, m) => format!("{a} {m}"),
        };
        by_model.entry(model).or_insert_with(Row::default).add(j);
    }
    let head = row("", COLS.iter().map(ToString::to_string).chain(["cost".into(), "per push".into()]));
    let _ = writeln!(s, "Last {days} days: {} jobs, ${:.2}\n\n{head}", total.jobs, total.cost);
    for (title, rows) in [("By repo", &by_repo), ("By kind of job", &by_kind), ("By agent and model", &by_model)] {
        let _ = writeln!(s, "{title}");
        for (name, r) in rows {
            let _ = writeln!(s, "{}", r.line(name));
        }
    }
    let _ = writeln!(
        s,
        "\npushed: done jobs that pushed code. edited: you edited its replies or plan (e). amended: you changed its files (m) before they went out. took over: you opened it in your own session (h). retried: you ran it again (t)."
    );
    let repos: BTreeSet<&RepoId> = jobs.iter().map(|j| &j.repo).collect();
    for r in repos {
        let n = fixes_in(&all, r).len();
        if n > 0 {
            let _ = writeln!(
                s,
                "{n} fix{} on {r} not learned from yet: `nightporter learn {}` (L on the Repos tab)",
                if n == 1 { "" } else { "es" },
                r.path
            );
        }
    }
    Ok(s)
}

/// A few lines for one repo's detail view.
pub fn repo_summary(db: &Db, repo: &RepoId) -> Result<String> {
    let since = now() - 30 * 86400;
    let all = db.recent_jobs(SCAN)?;
    let mut r = Row::default();
    for j in all.iter().filter(|j| j.repo == *repo && j.created_at >= since && j.mode != LEARN) {
        r.add(j);
    }
    let mut s = format!(
        "Last 30 days: {} jobs, {} pushed, {} rejected, {} edited, {} amended, ${:.2}",
        r.jobs, r.pushed, r.rejected, r.edited, r.amended, r.cost
    );
    let n = fixes_in(&all, repo).len();
    if n > 0 {
        let _ =
            write!(s, "\n{n} fix{} not learned from yet: press L (or `nightporter learn {}`)", if n == 1 { "" } else { "es" }, repo.path);
    }
    Ok(s)
}

/// Queue a learn job for `repo` (TUI `L`, `nightporter learn`).
pub fn start_learn(db: &mut Db, repo: &RepoId) -> Result<i64> {
    let all = db.recent_jobs(SCAN)?;
    anyhow::ensure!(!learning(&all, repo), "a learn job for {repo} is already queued or waiting for you");
    anyhow::ensure!(
        !fixes_in(&all, repo).is_empty(),
        "nothing to learn from on {repo} yet: no edits, amends, rejections or dismissals since the last learn"
    );
    db.create_job(repo, Target::Pr, 0, LEARN, "draft", &[])
}

fn learning(jobs: &[Job], repo: &RepoId) -> bool {
    jobs.iter().any(|j| j.repo == *repo && j.mode == LEARN && (j.status.is_active() || j.status.needs_user()))
}

/// The `prompt` a TOML table sets, resolved.
fn prompt_in(paths: &Paths, file: &Path, table: Option<&str>) -> Option<PathBuf> {
    let t: toml::Table = std::fs::read_to_string(file).ok()?.parse().ok()?;
    let t = match table {
        Some(k) => t.get(k)?.as_table()?,
        None => &t,
    };
    Some(paths.resolve(t.get("prompt")?.as_str()?))
}

/// The `prompt` the repo's own file sets (not one inherited from `[defaults]`).
fn own_prompt(paths: &Paths, repo: &RepoId) -> Option<PathBuf> {
    prompt_in(paths, &paths.repo_file(repo), None)
}

/// Where learn writes: the repo's own `prompt` file, unless `[defaults]` or another repo reads it
/// too (learn changes one repo's instructions only); then a new file for this repo.
fn prompt_path(paths: &Paths, repo: &RepoId) -> PathBuf {
    let fresh = || paths.config_dir.join("prompts").join(repo.forge.as_str()).join(&repo.host).join(format!("{}.md", repo.path));
    let Some(own) = own_prompt(paths, repo) else { return fresh() };
    let me = paths.repo_file(repo);
    let others = crate::doctor::walk_repo_files(&paths.repos_dir()).unwrap_or_default();
    let shared = prompt_in(paths, &paths.config_file(), Some("defaults")).as_ref() == Some(&own)
        || others.iter().filter(|f| **f != me).any(|f| prompt_in(paths, f, None).as_ref() == Some(&own));
    if shared { fresh() } else { own }
}

/// What the learn agent reads: each fix, as data.
fn corrections(fixes: &[Job]) -> String {
    let cut = crate::job::truncate;
    let mut c = String::new();
    for j in fixes {
        let kind = j.meta.kind.map_or_else(|| "job".into(), |k| crate::state::enum_str(&k));
        let _ = writeln!(c, "\n### Job {} ({kind}, #{}): {}", j.id, j.number, cut(&j.summary, 300));
        if !j.meta.drafted_plan.is_empty() {
            let _ = writeln!(
                c,
                "The agent's plan:\n{}\n\nThe maintainer changed it to:\n{}",
                cut(&j.meta.drafted_plan, 4000),
                cut(&j.meta.plan, 4000)
            );
        }
        if !j.meta.drafted.is_empty() {
            let now = j.reply_text();
            let _ = writeln!(c, "The agent drafted:\n{}\n\nThe maintainer changed it to:\n{}", cut(&j.meta.drafted, 4000), cut(&now, 4000));
        }
        if !j.meta.amend_diff.is_empty() {
            let _ = writeln!(c, "The maintainer changed the agent's code:\n{}", cut(&j.meta.amend_diff, 6000));
        }
        match j.status {
            JobStatus::Rejected => c.push_str("The maintainer rejected this job: nothing went out.\n"),
            JobStatus::Dismissed => c.push_str("The maintainer dismissed this job without sending it.\n"),
            _ => {}
        }
    }
    c
}

/// The learn job's run: the agent reads the corrections and the current file (no repo, no
/// commands) and proposes the whole new file. It waits for you; approving writes it.
pub async fn run_learn(ctx: &Ctx, j: &mut Job, s: &RepoSettings) -> Result<()> {
    let mut fixes = ctx.db(|d| fixes(d, &j.repo))?;
    fixes.truncate(MAX_FIXES);
    anyhow::ensure!(!fixes.is_empty(), "nothing to learn from any more");
    let path = prompt_path(&ctx.paths, &j.repo);
    let before = std::fs::read_to_string(&path).ok();
    // A new per-repo file starts from what the repo reads today (a shared prompt), since setting
    // the repo's own `prompt` replaces that one.
    let current =
        before.clone().or_else(|| s.prompt.as_deref().and_then(|p| crate::prompts::read_user_file(&ctx.paths, p).ok())).unwrap_or_default();
    let prompt = format!(
        "{}\n## Repository\n{}\n\n## Current instructions file\n<current>\n{current}\n</current>\n\n## Corrections\n{}\n",
        crate::prompts::LEARN,
        j.repo,
        crate::prompts::untrusted(&corrections(&fixes))
    );
    // No repository and no commands: an empty folder, read-only, nothing allowed to run.
    let job_dir = ctx.paths.job_dir(j.id);
    let empty = job_dir.join("empty");
    std::fs::create_dir_all(&empty)?;
    let mut ls = s.clone();
    ls.allow_commands.clear();
    ls.for_job(Some(JobKind::Learn));
    let _ = ctx.db(|d| d.log(j.id, &format!("learning from {} fix(es) with {}", fixes.len(), ls.agent.label())));
    let out = crate::agent::run(ls.agent, &ls, &empty, &job_dir.join("agent"), &prompt, true).await?;
    let proposal = out.replies.into_iter().map(|r| r.body).find(|b| !b.trim().is_empty()).context("the agent proposed no file")?;
    let proposal = format!("{}\n", proposal.trim_end());
    j.diff = file_diff(&job_dir, &current, &proposal);
    // Your instructions go into every future prompt: the reply checks (secrets, personal data) run on them.
    j.checks = text_checks(&proposal);
    j.meta.proposal = proposal;
    j.meta.prompt_path = path.to_string_lossy().into();
    j.meta.baseline = current;
    j.meta.prompt_before = before;
    j.meta.learned_from = fixes.iter().map(|f| f.id).collect();
    j.meta.kind = Some(JobKind::Learn);
    j.meta.agent = ls.agent.label().into();
    j.meta.model = model_of(ls.agent_args.for_agent(ls.agent));
    j.meta.cost_usd = out.cost_usd;
    j.summary = format!("LEARN: {}", crate::job::first_line(&out.summary));
    j.status = JobStatus::AwaitingApproval;
    ctx.db(|d| d.update_job(j))?;
    crate::job::note(ctx, j, Kind::NeedsApproval, "new instructions proposed: review the diff and approve");
    Ok(())
}

/// The reply checks (secrets, personal data) on a proposed instructions file: it goes into every
/// future prompt. Only failures are kept.
pub fn text_checks(text: &str) -> Vec<crate::state::CheckResult> {
    let mut c = Vec::new();
    crate::checks::recheck_replies(&mut c, &[text]);
    c.retain(|c| !c.ok);
    c
}

/// The diff of the old and new file, for the job view. Empty if it can't be made.
pub fn file_diff(dir: &Path, old: &str, new: &str) -> String {
    let (a, b) = (dir.join("current.md"), dir.join("proposed.md"));
    if std::fs::create_dir_all(dir).is_err() || std::fs::write(&a, old).is_err() || std::fs::write(&b, new).is_err() {
        return String::new();
    }
    crate::packs::diff_dirs(&a, &b)
}

/// Approving a learn job: write the file, point the repo's `prompt` at it if it doesn't already,
/// and count its fixes as learned. Refuses if the file or the setting changed since the proposal,
/// so what's written is the diff you approved.
pub fn apply_learn(paths: &Paths, db: &Db, j: &mut Job) -> Result<String> {
    let path = PathBuf::from(&j.meta.prompt_path);
    let again = "learn again (L) for a proposal against it";
    anyhow::ensure!(prompt_path(paths, &j.repo) == path, "the repo's `prompt` setting changed since the proposal; {again}");
    anyhow::ensure!(std::fs::read_to_string(&path).ok() == j.meta.prompt_before, "{} changed since the proposal; {again}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, &j.meta.proposal).with_context(|| format!("write {}", path.display()))?;
    if own_prompt(paths, &j.repo).as_ref() != Some(&path) {
        let p = path.to_string_lossy().to_string();
        crate::config::edit_file(&paths.repo_file(&j.repo), |d| d["prompt"] = toml_edit::value(p))?;
    }
    db.mark_learned(&j.meta.learned_from)?;
    j.status = JobStatus::Done;
    db.update_job(j)?;
    Ok(format!("wrote {}: the next jobs on {} use it", path.display(), j.repo.path))
}

/// Hourly from the daemon: remind you, at most weekly per repo, when a repo has fixes to learn from.
pub fn nudge(ctx: &Ctx) -> Result<()> {
    let cfg = ctx.cfg();
    let all = ctx.db(|d| d.recent_jobs(SCAN))?;
    for (repo, _) in ctx.subscribed(&cfg) {
        let n = fixes_in(&all, &repo).len();
        let key = format!("learn_nudge:{}", repo.key());
        let last: i64 = ctx.db(|d| d.get(&key))?.and_then(|v| v.parse().ok()).unwrap_or(0);
        if n < NUDGE_AT || now() - last < NUDGE_EVERY || learning(&all, &repo) {
            continue;
        }
        let first = ctx.db(|d| d.get("learn_nudged"))?.is_none();
        let mut text = format!(
            "{n} fixes since Night Porter last learned from you: press L on the Repos tab, or run `nightporter learn {}`",
            repo.path
        );
        if first {
            text.push_str(". It reads your edits, amends and rejections and proposes better instructions for this repo; nothing changes until you approve");
        }
        ctx.notify(&repo, &notify::Event::new(Kind::Done, &format!("{repo}: learn from your fixes"), &text));
        ctx.db(|d| d.set(&key, &now().to_string()).and_then(|()| d.set("learn_nudged", "1")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::ForgeKind;

    #[test]
    fn learn_never_writes_a_shared_file_or_over_your_change() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        let (a, b) = (RepoId::new(ForgeKind::GitHub, "github.com", "o/a"), RepoId::new(ForgeKind::GitHub, "github.com", "o/b"));
        let fresh = paths.config_dir.join("prompts/github/github.com/o/a.md");
        assert_eq!(prompt_path(&paths, &a), fresh, "no prompt of its own");
        for r in [&a, &b] {
            std::fs::create_dir_all(paths.repo_file(r).parent().unwrap()).unwrap();
            std::fs::write(paths.repo_file(r), "prompt = \"shared.md\"\n").unwrap();
        }
        assert_eq!(prompt_path(&paths, &a), fresh, "o/b reads the same file");
        std::fs::write(paths.repo_file(&b), "").unwrap();
        let own = paths.config_dir.join("shared.md");
        assert_eq!(prompt_path(&paths, &a), own, "now only o/a reads it");

        let mut db = Db::memory().unwrap();
        let id = db.create_job(&a, Target::Pr, 0, LEARN, "draft", &[]).unwrap();
        let mut j = db.job(id).unwrap().unwrap();
        j.status = JobStatus::Approved;
        j.meta.prompt_path = own.to_string_lossy().into();
        j.meta.proposal = "new\n".into();
        std::fs::write(&own, "edited meanwhile\n").unwrap();
        db.seed_job(&j).unwrap();
        assert!(apply_learn(&paths, &db, &mut j).is_err(), "the file changed since the proposal");
        assert_eq!(std::fs::read_to_string(&own).unwrap(), "edited meanwhile\n");
        j.meta.prompt_before = Some("edited meanwhile\n".into());
        apply_learn(&paths, &db, &mut j).unwrap();
        assert_eq!(std::fs::read_to_string(&own).unwrap(), "new\n");
    }

    #[test]
    fn model_from_args() {
        let a = |v: &[&str]| model_of(&v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["--model", "opus"]), "opus");
        assert_eq!(a(&["--model=haiku"]), "haiku");
        assert_eq!(a(&["-m", "gpt-5"]), "gpt-5");
        assert_eq!(a(&["--verbose"]), "");
    }

    #[test]
    fn fixes_count_trusted_corrections_since_the_last_learn() {
        let mut db = Db::memory().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let edited = db.create_job(&repo, Target::Pr, 1, "my_prs", "draft", &[]).unwrap();
        let plain = db.create_job(&repo, Target::Pr, 2, "my_prs", "draft", &[]).unwrap();
        let theirs = db.create_job(&repo, Target::Pr, 3, "maintainer", "draft", &[]).unwrap();
        let let_in = db.create_job(&repo, Target::Pr, 4, "my_prs", "draft", &[]).unwrap();
        let old = db.create_job(&repo, Target::Pr, 5, "my_prs", "draft", &[]).unwrap();
        for (id, f) in [(edited, 0), (plain, 1), (theirs, 2), (let_in, 3), (old, 4)] {
            let mut j = db.job(id).unwrap().unwrap();
            j.status = JobStatus::Rejected;
            j.meta.kind = Some(JobKind::ReviewComment);
            j.meta.agent = if f == 4 { String::new() } else { "Claude Code".into() };
            match f {
                0 => (j.status, j.meta.drafted) = (JobStatus::Done, "draft".into()),
                1 => j.status = JobStatus::Done,
                2 => j.meta.untrusted_code = true,
                3 => j.meta.untrusted_input = true,
                _ => {}
            }
            db.seed_job(&j).unwrap();
        }
        let f = fixes(&db, &repo).unwrap();
        assert_eq!(
            f.iter().map(|j| j.id).collect::<Vec<_>>(),
            vec![edited],
            "plain, someone else's code, let in from the inbox, or from an older version: not fixes"
        );
        assert!(start_learn(&mut db, &repo).is_ok());
        assert!(start_learn(&mut db, &repo).is_err(), "one learn job at a time");
        db.mark_learned(&[edited]).unwrap();
        assert!(fixes(&db, &repo).unwrap().is_empty(), "learned ones don't count again");
        let s = stats(&db, None, 30).unwrap();
        assert!(s.contains("rejected") && s.contains("took over"), "full words in the header: {s}");
        assert!(s.contains("Last 30 days: 5 jobs") && s.contains("\n  o/r ") && s.contains("edit"), "{s}");
    }
}
