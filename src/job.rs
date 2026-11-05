//! One job: prepare the worktree, (optionally) set up the environment, run pack gates and the
//! agent, check the result, then deliver or wait for approval.

use crate::checks::{self, Diff};
use crate::config::{Level, RepoSettings};
use crate::daemon::Ctx;
use crate::forge::{Event, PrState, Target, Verb};
use crate::git::{self, git};
use crate::notify::Kind;
use crate::packs::{self, Hook};
use crate::state::{CheckResult, Job, JobStatus};
use crate::{agent, prompts};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

const BUILD_FILES: &[&str] = &[
    "package.json",
    "composer.json",
    "Makefile",
    "Dockerfile",
    "docker-compose.yml",
    "compose.yaml",
    "build.rs",
    "setup.py",
    "pyproject.toml",
    "Cargo.toml",
    "Gemfile",
    "Rakefile",
    "gulpfile.js",
    "webpack.config.js",
    "vite.config.ts",
    "vite.config.js",
    "phpunit.xml",
    "phpunit.xml.dist",
    ".ddev/config.yaml",
    "justfile",
];

fn level(j: &Job) -> Level {
    match j.level.as_str() {
        "observe" => Level::Observe,
        "suggest" => Level::Suggest,
        "auto" => Level::Auto,
        _ => Level::Draft,
    }
}

pub fn worktree_path(ctx: &Ctx, j: &Job) -> PathBuf {
    let kind = if j.target == Target::Pr { "pr" } else { "issue" };
    ctx.paths.worktrees_dir().join(j.repo.forge.as_str()).join(&j.repo.host).join(&j.repo.path).join(format!("{kind}-{}", j.number))
}

fn log(ctx: &Ctx, id: i64, msg: &str) {
    tracing::info!("job {id}: {msg}");
    let _ = ctx.db(|d| d.log(id, msg));
}

/// Verbs that never push code (their commits stay local).
fn read_only_verb(v: Option<Verb>) -> bool {
    matches!(v, Some(Verb::Triage | Verb::Review))
}
fn pushes_code(v: Option<Verb>) -> bool {
    !matches!(v, Some(Verb::Triage | Verb::Review | Verb::Reproduce))
}

pub async fn run(ctx: &Ctx, id: i64) -> Result<()> {
    let mut j = ctx.db(|d| d.job(id))?.context("no such job")?;
    let events = ctx.db(|d| d.job_events(id))?;
    let cfg = ctx.cfg();
    let s = cfg.settings_for(&ctx.paths, &j.repo)?;
    ctx.db(|d| d.set_status(id, JobStatus::Running))?;
    j.status = JobStatus::Running;
    let lvl = level(&j);
    let verb = j.meta.verb;
    let where_ = format!("{}#{}", j.repo, j.number);

    if lvl == Level::Observe {
        j.summary = format!("{} new event(s); observe only", events.len());
        j.status = JobStatus::Done;
        ctx.db(|d| d.update_job(&j))?;
        ctx.notify(Kind::Done, &where_, &j.summary);
        return Ok(());
    }

    let clone = ctx.clone_for(&j.repo).with_context(|| format!("{} is not cloned under the code dir", j.repo))?;
    let forge = ctx.forge(&j.repo)?;
    let me = forge.lock().await.me().to_string();
    let repo_url = git::remote_url(&clone, &j.repo, &cfg.accounts).await?;
    let branch = git::branch_name(j.target, j.number);
    let wt = worktree_path(ctx, &j);
    j.worktree = wt.to_string_lossy().into();
    j.branch = branch.clone();
    j.meta.clone = clone.to_string_lossy().into();

    // Where to fetch from and push to.
    let (pr, fetch_url, fetch_ref) = match j.target {
        Target::Pr => {
            let pr = forge.lock().await.pr(&j.repo, j.number).await?;
            if pr.state != PrState::Open {
                j.summary = format!("PR is {:?}; nothing to do", pr.state).to_lowercase();
                j.status = JobStatus::Done;
                ctx.db(|d| d.update_job(&j))?;
                return Ok(());
            }
            let url = git::same_scheme(&repo_url, &pr.head_repo_ssh, &pr.head_repo_https);
            j.meta.push_url = url.clone();
            j.meta.push_ref = pr.head_ref.clone();
            j.meta.base = pr.base_ref.clone();
            j.meta.untrusted_code = verb.is_some() && !pr.author.eq_ignore_ascii_case(&me);
            let r = format!("refs/heads/{}", pr.head_ref);
            (Some(pr), url, r)
        }
        Target::Issue => {
            let base = default_branch(&clone, &repo_url).await.unwrap_or_else(|_| "main".into());
            j.meta.push_url = repo_url.clone();
            j.meta.push_ref = branch.clone();
            j.meta.base = base.clone();
            (None, repo_url.clone(), format!("refs/heads/{base}"))
        }
    };

    log(ctx, id, &format!("preparing worktree {}", wt.display()));
    j.head_before = git::prepare_worktree(&clone, &wt, &fetch_url, &fetch_ref, &branch).await?;
    if let Some(pr) = &pr
        && pr.head_sha != j.head_before
    {
        log(ctx, id, "note: branch moved between API read and fetch; using the fetched head");
    }
    let step = prompts::step_for(&events, verb, j.target == Target::Issue);
    if step == "conflict" {
        git(&wt, &["fetch", "--quiet", "--no-tags", &repo_url, &format!("+refs/heads/{}:refs/nightporter/base", j.meta.base)]).await?;
    }

    // Untrusted code may only run (env setup, tests) after the user approves this job.
    let runs_code = !s.env.setup.is_empty() || !s.allow_commands.is_empty();
    if j.meta.untrusted_code {
        j.meta.build_files = changed_build_files(&wt, &repo_url, &j.meta.base).await;
        if runs_code && !j.meta.run_approved {
            j.status = JobStatus::AwaitingRunApproval;
            j.summary = "untrusted code: approve before env setup / tests run".into();
            ctx.db(|d| d.update_job(&j))?;
            ctx.notify(Kind::NeedsApproval, &where_, "Untrusted code wants to run: review build files and approve");
            return Ok(());
        }
    }

    let job_dir = ctx.paths.job_dir(id);
    std::fs::create_dir_all(&job_dir)?;

    // Pack filter gates (read-only).
    for (name, text, _) in packs::steps(&ctx.paths, &s.packs, Hook::Filter) {
        let p = gate_prompt(&text, &events);
        let o = agent::run(s.agent, &s, &wt, &job_dir.join(format!("gate-{}", name.replace('/', "-"))), &p, true).await?;
        if o.verdict.as_deref() == Some("block") {
            j.summary = format!("filtered by {name}: {}", o.summary);
            j.status = JobStatus::Done;
            ctx.db(|d| d.update_job(&j))?;
            log(ctx, id, &j.summary);
            return Ok(());
        }
    }

    let vars = Vars { repo: &j.repo.path, n: j.number, forge: j.repo.forge.as_str(), wt: &wt, branch: &branch };
    for c in &s.env.setup {
        log(ctx, id, &format!("env setup: {c}"));
        sh(&wt, &job_dir, &vars.apply(c), &s).await.with_context(|| format!("env setup `{c}`"))?;
    }

    let mut pack_prompts: Vec<String> = Vec::new();
    for hook in [Hook::PreFix, Hook::PostFix] {
        for (name, text, _) in packs::steps(&ctx.paths, &s.packs, hook) {
            let when = if hook == Hook::PreFix { "Before you start" } else { "After the main task" };
            pack_prompts.push(format!("### {when} ({name})\n{text}"));
        }
    }
    let prompt = prompts::build(&prompts::Input {
        paths: &ctx.paths,
        settings: &s,
        repo: &j.repo.path,
        pr: pr.as_ref(),
        issue_title: None,
        events: &events,
        verb,
        is_issue: j.target == Target::Issue,
        me: &me,
        pack_prompts: &pack_prompts,
    })?;
    log(ctx, id, &format!("running {} ({step})", s.agent.label()));
    let outcome = agent::run(s.agent, &s, &wt, &job_dir, &prompt, read_only_verb(verb)).await;
    for c in &s.env.teardown {
        if let Err(e) = sh(&wt, &job_dir, &vars.apply(c), &s).await {
            log(ctx, id, &format!("env teardown `{c}` failed: {e:#}"));
        }
    }
    let outcome = outcome?;
    j.meta.cost_usd = outcome.cost_usd;
    j.summary = outcome.summary.clone();

    // Inspect what the agent did.
    j.head_after = git::head(&wt).await?;
    let range = format!("{}..{}", j.head_before, j.head_after);
    let commits: usize = git(&wt, &["rev-list", "--count", &range]).await?.parse().unwrap_or(0);
    let d = Diff {
        files: checks::parse_numstat(&git(&wt, &["diff", "--numstat", &j.head_before, &j.head_after]).await?),
        patch: git(&wt, &["diff", "--no-color", &j.head_before, &j.head_after]).await?,
        fast_forward: git::is_ancestor(&wt, &j.head_before, &j.head_after).await,
    };
    let dirty = !git(&wt, &["status", "--porcelain"]).await?.is_empty();
    if dirty {
        log(ctx, id, "agent left uncommitted changes; they are not pushed");
    }
    let gl = gitleaks(&wt, &range, &job_dir).await;
    let mut results = checks::run(&s.checks, &d, gl.as_deref());
    // Pack pre-push gates can only add failures.
    if commits > 0 {
        for (name, text, _) in packs::steps(&ctx.paths, &s.packs, Hook::PrePush) {
            let p = gate_prompt(&format!("{text}\n\nThe diff to review:\n```diff\n{}\n```", truncate(&d.patch, 60_000)), &events);
            let o = agent::run(s.agent, &s, &wt, &job_dir.join(format!("gate-{}", name.replace('/', "-"))), &p, true).await?;
            match o.verdict.as_deref() {
                Some("block") => results.push(CheckResult { name, ok: false, blocking: true, detail: o.summary }),
                Some("needs_approval") => results.push(CheckResult { name, ok: false, blocking: false, detail: o.summary }),
                _ => {}
            }
        }
    }
    j.checks = results;
    j.diff = truncate(&d.patch, 2_000_000).to_string();
    j.replies = outcome.replies;
    if s.reply.footer {
        for r in &mut j.replies {
            r.body.push_str(&prompts::footer(s.agent));
        }
    }

    let has_code = commits > 0 && pushes_code(verb);
    if commits == 0 && j.replies.is_empty() {
        j.status = JobStatus::Done;
        if j.summary.is_empty() {
            j.summary = "nothing to do".into();
        }
        ctx.db(|d| d.update_job(&j))?;
        return Ok(());
    }
    let clean = checks::all_ok(&j.checks) || !has_code;
    j.status = match lvl {
        Level::Suggest => JobStatus::Ready,
        Level::Auto if clean => JobStatus::Approved,
        _ => JobStatus::AwaitingApproval,
    };
    ctx.db(|d| d.update_job(&j))?;
    match j.status {
        JobStatus::Approved => deliver(ctx, id).await?,
        JobStatus::Ready => ctx.notify(Kind::Done, &where_, "Suggestion ready to copy"),
        _ => {
            let why = if clean { "draft ready".to_string() } else { format!("checks need you: {}", failed_names(&j.checks)) };
            ctx.notify(Kind::NeedsApproval, &where_, &why);
        }
    }
    Ok(())
}

/// Push (if there is code) and post replies. Called for approved jobs.
pub async fn deliver(ctx: &Ctx, id: i64) -> Result<()> {
    let mut j = ctx.db(|d| d.job(id))?.context("no such job")?;
    if j.status != JobStatus::Approved {
        bail!("job {id} is {:?}, not approved", j.status);
    }
    let events = ctx.db(|d| d.job_events(id))?;
    let cfg = ctx.cfg();
    let s = cfg.settings_for(&ctx.paths, &j.repo)?;
    let forge = ctx.forge(&j.repo)?;
    let verb = j.meta.verb;
    let wt = PathBuf::from(&j.worktree);
    let has_code = !j.head_after.is_empty() && j.head_after != j.head_before && pushes_code(verb);
    let where_ = format!("{}#{}", j.repo, j.number);

    if has_code {
        // What gets pushed must be exactly what was reviewed.
        let now_head = git::head(&wt).await?;
        if now_head != j.head_after {
            bail!("worktree HEAD changed since review ({} → {}); not pushing", short(&j.head_after), short(&now_head));
        }
        if !git::is_ancestor(&wt, &j.head_before, &j.head_after).await {
            bail!("history rewrite detected; not pushing");
        }
        if j.target == Target::Pr {
            let pr = forge.lock().await.pr(&j.repo, j.number).await?;
            if pr.head_sha != j.head_before {
                ctx.db(|d| d.requeue_events(id))?;
                j.status = JobStatus::Failed;
                j.error = "the PR branch moved while we worked; re-queued on the new head".into();
                ctx.db(|d| d.update_job(&j))?;
                return Ok(());
            }
        }
        log(ctx, id, &format!("pushing {} to {}", short(&j.head_after), j.meta.push_ref));
        if let Err(e) = git::push(&wt, &j.meta.push_url, &j.meta.push_ref).await {
            let msg = format!("{e:#}");
            if msg.contains("non-fast-forward") || msg.contains("rejected") || msg.contains("fetch first") {
                ctx.db(|d| d.requeue_events(id))?;
                j.status = JobStatus::Failed;
                j.error = "push rejected (branch moved); re-queued".into();
                ctx.db(|d| d.update_job(&j))?;
                return Ok(());
            }
            return Err(e);
        }
    }

    let f = forge.lock().await;
    if j.target == Target::Issue && has_code && verb == Some(Verb::Fix) {
        let pr_body = j.replies.iter().find(|r| r.event.is_none()).map(|r| r.body.clone()).unwrap_or_else(|| j.summary.clone());
        let title = format!("Fix #{}: {}", j.number, first_line(&j.summary));
        let url = f.open_draft(&j.repo, &j.meta.push_ref, &j.meta.base, &title, &format!("{pr_body}\n\nCloses #{}", j.number)).await?;
        log(ctx, id, &format!("opened draft {url}"));
        j.meta.pr_url = url;
    }
    for r in &j.replies {
        let ev: Option<&Event> = r.event.as_deref().and_then(|eid| events.iter().find(|e| e.id == eid));
        // For issue fixes, the general reply became the PR body; post only event replies.
        if j.target == Target::Issue && has_code && ev.is_none() && verb == Some(Verb::Fix) {
            continue;
        }
        f.reply(&j.repo, j.target, j.number, ev, &r.body)
            .await
            .with_context(|| format!("post reply to {}", r.event.as_deref().unwrap_or("PR")))?;
        if s.reply.resolve_threads
            && has_code
            && let Some(t) = ev.and_then(|e| e.thread.as_deref())
            && let Err(e) = f.resolve(&j.repo, j.number, t).await
        {
            log(ctx, id, &format!("could not resolve thread {t}: {e:#}"));
        }
    }
    drop(f);
    j.status = JobStatus::Done;
    ctx.db(|d| d.update_job(&j))?;
    let pushed = if has_code { format!("pushed {} and ", short(&j.head_after)) } else { String::new() };
    ctx.notify(Kind::Done, &where_, &format!("{pushed}posted {} repl{}", j.replies.len(), if j.replies.len() == 1 { "y" } else { "ies" }));
    Ok(())
}

/// Commands the user can copy for a `suggest` job.
pub fn manual_commands(j: &Job) -> String {
    if j.head_after.is_empty() || j.head_after == j.head_before {
        return String::new();
    }
    format!("git -C {} push {} HEAD:refs/heads/{}", shell_quote(&j.worktree), shell_quote(&j.meta.push_url), j.meta.push_ref)
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-:@".contains(c)) { s.into() } else { format!("'{}'", s.replace('\'', "'\\''")) }
}

/// Remove worktrees of merged/closed PRs and ones idle past the limit; prune old state.
pub async fn housekeeping(ctx: &Ctx) -> Result<()> {
    let cfg = ctx.cfg();
    ctx.db(|d| d.prune(cfg.retention_days))?;
    gc(ctx, false).await
}

/// `nightporter gc`: clean worktrees. `all` removes every idle worktree.
pub async fn gc(ctx: &Ctx, all: bool) -> Result<()> {
    let cfg = ctx.cfg();
    let idle = std::time::Duration::from_secs(cfg.worktree_idle_days as u64 * 86400);
    let jobs = ctx.db(|d| d.recent_jobs(10_000))?;
    let mut seen = std::collections::HashSet::new();
    for j in jobs {
        if j.worktree.is_empty() || !seen.insert(j.worktree.clone()) {
            continue;
        }
        let wt = PathBuf::from(&j.worktree);
        if !wt.exists() || ctx.db(|d| d.pr_busy(&j.repo, j.number))? {
            continue;
        }
        let old = wt.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e > idle);
        let closed = j.target == Target::Pr
            && match ctx.forge(&j.repo) {
                Ok(f) => f.lock().await.pr(&j.repo, j.number).await.is_ok_and(|p| p.state != PrState::Open),
                Err(_) => false,
            };
        if all || old || closed {
            let s = cfg.settings_for(&ctx.paths, &j.repo).ok();
            if let Some(s) = &s {
                let vars = Vars { repo: &j.repo.path, n: j.number, forge: j.repo.forge.as_str(), wt: &wt, branch: &j.branch };
                for c in &s.env.cleanup {
                    let _ = sh(&wt, &ctx.paths.job_dir(j.id), &vars.apply(c), s).await;
                }
            }
            let clone = PathBuf::from(&j.meta.clone);
            tracing::info!("removing worktree {}", wt.display());
            git::remove_worktree(&clone, &wt, &j.branch).await?;
        }
    }
    Ok(())
}

struct Vars<'a> {
    repo: &'a str,
    n: u64,
    forge: &'a str,
    wt: &'a Path,
    branch: &'a str,
}

impl Vars<'_> {
    /// `{repo}` is made safe for names (DDEV project names, container names).
    fn apply(&self, s: &str) -> String {
        let slug: String = self.repo.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
        s.replace("{repo}", &slug)
            .replace("{n}", &self.n.to_string())
            .replace("{forge}", self.forge)
            .replace("{wt}", &self.wt.to_string_lossy())
            .replace("{branch}", self.branch)
    }
}

/// Run a user-configured env command (from the user's own config, never from the repo).
async fn sh(wt: &Path, job_dir: &Path, cmd: &str, s: &RepoSettings) -> Result<()> {
    let child = tokio::process::Command::new("sh")
        .args(["-c", cmd])
        .current_dir(wt)
        .env_clear()
        .envs(agent::agent_env(job_dir))
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .output();
    let out = tokio::time::timeout(s.timeout, child).await.map_err(|_| anyhow::anyhow!("timed out"))??;
    let _ = std::fs::write(job_dir.join("env.log"), [out.stdout.as_slice(), out.stderr.as_slice()].concat());
    if !out.status.success() {
        bail!("exit {}: {}", out.status, String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or_default());
    }
    Ok(())
}

async fn default_branch(clone: &Path, url: &str) -> Result<String> {
    let out = git(clone, &["ls-remote", "--symref", url, "HEAD"]).await?;
    out.lines()
        .find_map(|l| l.strip_prefix("ref: refs/heads/").and_then(|r| r.split_whitespace().next()))
        .map(String::from)
        .context("no default branch")
}

async fn changed_build_files(wt: &Path, url: &str, base: &str) -> Vec<String> {
    if git(wt, &["fetch", "--quiet", "--no-tags", url, &format!("+refs/heads/{base}:refs/nightporter/base")]).await.is_err() {
        return vec!["(could not compare with base; treat everything as changed)".into()];
    }
    let names = git(wt, &["diff", "--name-only", "refs/nightporter/base...HEAD"]).await.unwrap_or_default();
    names
        .lines()
        .filter(|p| {
            let name = p.rsplit('/').next().unwrap_or(p);
            BUILD_FILES.contains(&name)
                || BUILD_FILES.contains(p)
                || p.starts_with(".github/")
                || p.starts_with(".gitlab")
                || p.starts_with(".ddev/")
                || p.ends_with(".sh")
        })
        .map(String::from)
        .collect()
}

async fn gitleaks(wt: &Path, range: &str, job_dir: &Path) -> Option<String> {
    let report = job_dir.join("gitleaks.json");
    let out = tokio::process::Command::new("gitleaks")
        .args(["detect", "--no-banner", "--redact", "--exit-code", "0", "--report-format", "json", "--log-opts", range, "--source"])
        .arg(wt)
        .arg("--report-path")
        .arg(&report)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).ok()?).ok()?;
    let findings: Vec<String> = v
        .as_array()?
        .iter()
        .map(|f| format!("{} in {}:{}", f["RuleID"].as_str().unwrap_or("?"), f["File"].as_str().unwrap_or("?"), f["StartLine"]))
        .collect();
    Some(findings.join("\n"))
}

fn gate_prompt(step: &str, events: &[Event]) -> String {
    let mut p = String::from(prompts::SYSTEM);
    p.push_str("\n## Gate\nYou are a read-only gate. Do not change anything.\n");
    p.push_str(step);
    p.push_str("\n\n## Requests\n");
    for e in events {
        p.push_str(&format!("- @{} ({:?}): {}\n", e.author, e.trigger, prompts::untrusted(&e.body)));
    }
    p.push_str("\nEnd with a ```nightporter block: {\"verdict\": \"pass\" | \"block\" | \"needs_approval\", \"summary\": \"reason\"}\n");
    p
}

fn failed_names(c: &[CheckResult]) -> String {
    c.iter().filter(|c| !c.ok).map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut i = n;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("Night Porter fix").chars().take(72).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vars_template() {
        let v = Vars { repo: "Org/My.Module", n: 12, forge: "gitlab", wt: Path::new("/w"), branch: "b" };
        assert_eq!(
            v.apply("ddev config --project-name=np-{repo}-pr{n} && cd {wt}"),
            "ddev config --project-name=np-org-my-module-pr12 && cd /w"
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/a/b"), "/a/b");
        assert_eq!(shell_quote("/a b/it's"), "'/a b/it'\\''s'");
    }

    #[test]
    fn verbs() {
        assert!(!pushes_code(Some(Verb::Review)) && pushes_code(Some(Verb::Fix)) && pushes_code(None));
        assert!(read_only_verb(Some(Verb::Triage)) && !read_only_verb(Some(Verb::Reproduce)));
    }
}
