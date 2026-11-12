//! Getting better over time: what jobs cost and how often you had to fix them (`nightporter
//! stats`), and learning from those fixes (`nightporter learn`): a job that proposes a better
//! instructions file for one repo, from your edits, amends, rejections and dismissals.

use crate::config::{JobKind, RepoSettings};
use crate::daemon::Ctx;
use crate::forge::{RepoId, Target, Verb};
use crate::notify::{self, Kind};
use crate::paths::Paths;
use crate::state::{Db, Job, JobStatus, now};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

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
        && !matches!(j.meta.verb, Some(Verb::Triage | Verb::Review | Verb::Reproduce))
}

/// You corrected this job: edited what it wrote, changed its code, or turned it down.
fn is_fix(j: &Job) -> bool {
    !j.meta.drafted.is_empty()
        || !j.meta.drafted_plan.is_empty()
        || !j.meta.amend_diff.is_empty()
        || j.status == JobStatus::Rejected
        || (j.status == JobStatus::Dismissed && matches!(j.meta.dismissed_from, Some(JobStatus::AwaitingApproval | JobStatus::Ready)))
}

fn learned_key(repo: &RepoId) -> String {
    format!("learned:{}", repo.key())
}

/// Fixes on `repo` since the last approved learn, newest first. Only jobs that started from
/// trusted people: not someone else's code or issue, and nothing let in from the inbox.
pub fn fixes(db: &Db, repo: &RepoId) -> Result<Vec<Job>> {
    let since: i64 = db.get(&learned_key(repo))?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut out = Vec::new();
    for j in db.recent_jobs(SCAN)? {
        // Fail closed: `kind` is set when a job runs (jobs from before this version have none), and
        // the trust flags are set on the job itself, so a retry that detaches its events can't
        // launder it.
        let trusted = j.meta.kind.is_some() && !j.meta.untrusted_code && !j.meta.untrusted_input;
        if j.repo == *repo && j.mode != LEARN && j.updated_at > since && is_fix(&j) && trusted {
            out.push(j);
        }
    }
    Ok(out)
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
        format!(
            "  {name:<34} {:>4} {:>4} {:>6} {:>4} {:>4} {:>4} {:>4} {:>4} {:>4} {:>4} {:>8} {:>8}",
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
            format!("${:.2}", self.cost),
            per
        )
    }
}

const HEAD: &str = "                                     jobs done pushed rej  dis  fail edit amnd take rtry     cost per push";

/// `nightporter stats`: the last `days` days, for one repo or all, by repo, kind of job and
/// agent/model, plus the repos with fixes to learn from.
pub fn stats(db: &Db, repo: Option<&RepoId>, days: u32) -> Result<String> {
    let since = now() - i64::from(days) * 86400;
    let jobs: Vec<Job> = db
        .recent_jobs(SCAN)?
        .into_iter()
        .filter(|j| j.created_at >= since && j.mode != LEARN && repo.is_none_or(|r| j.repo == *r))
        .collect();
    let mut s = String::new();
    if jobs.is_empty() {
        let _ = writeln!(s, "No jobs in the last {days} days.");
        return Ok(s);
    }
    let mut total = Row::default();
    let (mut by_repo, mut by_kind, mut by_model) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    for j in &jobs {
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
    let _ = writeln!(s, "Last {days} days: {} jobs, ${:.2}\n\n{HEAD}", total.jobs, total.cost);
    for (title, rows) in [("By repo", &by_repo), ("By kind of job", &by_kind), ("By agent and model", &by_model)] {
        let _ = writeln!(s, "{title}");
        for (name, r) in rows {
            let _ = writeln!(s, "{}", r.line(&name.chars().take(34).collect::<String>()));
        }
    }
    let _ = writeln!(
        s,
        "\npushed: done jobs that pushed code. edit/amnd: you edited replies or a plan (e), or changed files (m) before it went out."
    );
    let mut repos: Vec<&RepoId> = jobs.iter().map(|j| &j.repo).collect();
    repos.dedup();
    repos.sort_by_key(|r| r.key());
    repos.dedup();
    for r in repos {
        let n = fixes(db, r)?.len();
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
    let mut r = Row::default();
    for j in db.recent_jobs(SCAN)?.iter().filter(|j| j.repo == *repo && j.created_at >= since && j.mode != LEARN) {
        r.add(j);
    }
    let mut s = format!(
        "Last 30 days: {} jobs, {} pushed, {} rejected, {} edited, {} amended, ${:.2}",
        r.jobs, r.pushed, r.rejected, r.edited, r.amended, r.cost
    );
    let n = fixes(db, repo)?.len();
    if n > 0 {
        let _ =
            write!(s, "\n{n} fix{} not learned from yet: press L (or `nightporter learn {}`)", if n == 1 { "" } else { "es" }, repo.path);
    }
    Ok(s)
}

/// Queue a learn job for `repo` (TUI `L`, `nightporter learn`).
pub fn start_learn(db: &mut Db, repo: &RepoId) -> Result<i64> {
    let busy =
        db.recent_jobs(SCAN)?.into_iter().any(|j| j.repo == *repo && j.mode == LEARN && (j.status.is_active() || j.status.needs_user()));
    anyhow::ensure!(!busy, "a learn job for {repo} is already queued or waiting for you");
    anyhow::ensure!(
        !fixes(db, repo)?.is_empty(),
        "nothing to learn from on {repo} yet: no edits, amends, rejections or dismissals since the last learn"
    );
    db.create_job(repo, Target::Pr, 0, LEARN, "draft", &[])
}

/// The `prompt` the repo's own file sets (not one inherited from `[defaults]`, which other repos
/// share and learn must never overwrite).
fn own_prompt(paths: &Paths, repo: &RepoId) -> Option<String> {
    let t: toml::Table = std::fs::read_to_string(paths.repo_file(repo)).ok()?.parse().ok()?;
    t.get("prompt")?.as_str().map(String::from)
}

fn resolve(paths: &Paths, p: &str) -> PathBuf {
    let p = crate::paths::expand_tilde(p);
    if p.is_relative() { paths.config_dir.join(p) } else { p }
}

/// Where learn writes: the repo's own `prompt` file, or a new one for this repo in the config folder.
fn prompt_path(paths: &Paths, repo: &RepoId) -> PathBuf {
    own_prompt(paths, repo).map_or_else(
        || paths.config_dir.join("prompts").join(repo.forge.as_str()).join(&repo.host).join(format!("{}.md", repo.path)),
        |p| resolve(paths, &p),
    )
}

/// What the learn agent reads: each fix, as data.
fn corrections(fixes: &[Job]) -> String {
    let cut = |s: &str, n: usize| -> String { s.chars().take(n).collect() };
    let mut c = String::new();
    for j in fixes.iter().take(MAX_FIXES) {
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
            let now = j.replies.iter().map(|r| crate::prompts::unsign(&r.body)).collect::<Vec<_>>().join("\n\n---\n\n");
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
    let fixes = ctx.db(|d| fixes(d, &j.repo))?;
    anyhow::ensure!(!fixes.is_empty(), "nothing to learn from any more");
    let path = prompt_path(&ctx.paths, &j.repo);
    // A new per-repo file starts from what the repo reads today (a shared `defaults.prompt`), since
    // setting the repo's own `prompt` replaces that one.
    let current = std::fs::read_to_string(&path)
        .or_else(|_| s.prompt.as_deref().map_or(Ok(String::new()), |p| std::fs::read_to_string(resolve(&ctx.paths, p))))
        .unwrap_or_default();
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
    let _ = ctx.db(|d| d.log(j.id, &format!("learning from {} fix(es) with {}", fixes.len().min(MAX_FIXES), ls.agent.label())));
    let out = crate::agent::run(ls.agent, &ls, &empty, &job_dir.join("agent"), &prompt, true).await?;
    let proposal = out.replies.into_iter().map(|r| r.body).find(|b| !b.trim().is_empty()).context("the agent proposed no file")?;
    let proposal = format!("{}\n", proposal.trim_end());
    j.diff = file_diff(&job_dir, &current, &proposal);
    // Your instructions go into every future prompt: the reply checks (secrets, personal data) run on them.
    j.checks = crate::improve::text_checks(&proposal);
    j.meta.proposal = proposal;
    j.meta.prompt_path = path.to_string_lossy().into();
    j.meta.learned_until = fixes.iter().map(|f| f.updated_at).max().unwrap_or(0);
    j.meta.kind = Some(JobKind::Learn);
    j.meta.agent = ls.agent.label().into();
    j.meta.model = model_of(ls.agent_args.for_agent(ls.agent));
    j.meta.cost_usd = out.cost_usd;
    j.summary = format!("LEARN: {}", out.summary.lines().next().unwrap_or_default());
    j.status = JobStatus::AwaitingApproval;
    ctx.db(|d| d.update_job(j))?;
    let mut ev =
        notify::Event::new(Kind::NeedsApproval, &format!("{} (learn)", j.repo), "new instructions proposed: review the diff and approve");
    ev.job = Some(j.id);
    ctx.notify(&j.repo, &ev);
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

/// `git diff --no-index` of the old and new file, for the job view. Empty if git can't run.
pub fn file_diff(dir: &std::path::Path, old: &str, new: &str) -> String {
    let (a, b) = (dir.join("current.md"), dir.join("proposed.md"));
    if std::fs::create_dir_all(dir).is_err() || std::fs::write(&a, old).is_err() || std::fs::write(&b, new).is_err() {
        return String::new();
    }
    let out = std::process::Command::new("git").args(["diff", "--no-index", "--no-color", "--no-ext-diff", "--"]).arg(&a).arg(&b).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Approving a learn job: write the file, point the repo's `prompt` at it if it wasn't set, and
/// count its fixes as learned.
pub fn apply_learn(paths: &Paths, db: &Db, j: &mut Job) -> Result<String> {
    let path = PathBuf::from(&j.meta.prompt_path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, &j.meta.proposal).with_context(|| format!("write {}", path.display()))?;
    if own_prompt(paths, &j.repo).is_none() {
        let p = path.to_string_lossy().to_string();
        crate::config::edit_file(&paths.repo_file(&j.repo), |d| d["prompt"] = toml_edit::value(p))?;
    }
    db.set(&learned_key(&j.repo), &j.meta.learned_until.to_string())?;
    j.status = JobStatus::Done;
    db.update_job(j)?;
    Ok(format!("wrote {}: the next jobs on {} use it", path.display(), j.repo.path))
}

/// Hourly from the daemon: remind you, at most weekly per repo, when a repo has fixes to learn from.
pub fn nudge(ctx: &Ctx) -> Result<()> {
    let cfg = ctx.cfg();
    for (repo, _) in ctx.subscribed(&cfg) {
        let n = ctx.db(|d| fixes(d, &repo))?.len();
        let key = format!("learn_nudge:{}", repo.key());
        let last: i64 = ctx.db(|d| d.get(&key))?.and_then(|v| v.parse().ok()).unwrap_or(0);
        if n < NUDGE_AT || now() - last < NUDGE_EVERY {
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
            j.meta.kind = (f != 4).then_some(JobKind::ReviewComment);
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
            "plain, someone else's code, let in from the inbox, or from before kinds were recorded: not fixes"
        );
        assert!(start_learn(&mut db, &repo).is_ok());
        assert!(start_learn(&mut db, &repo).is_err(), "one learn job at a time");
        db.set(&learned_key(&repo), &now().to_string()).unwrap();
        assert!(fixes(&db, &repo).unwrap().is_empty(), "learned ones don't count again");
        let s = stats(&db, None, 30).unwrap();
        assert!(s.contains("Last 30 days: 5 jobs") && s.contains("\n  o/r ") && s.contains("edit"), "{s}");
    }
}
