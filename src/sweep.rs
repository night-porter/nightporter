//! `nightporter sweep`: queue maintainer jobs for a repo's backlog of open issues or PRs/MRs,
//! picked by label and age. The jobs go through the normal queue (debounce, schedule, caps) and
//! stop at maintainer mode's level (at most draft), so you review each one.

use crate::daemon::Ctx;
use crate::forge::{Event, Listed, Pick, RepoId, Role, Target, Trigger, Verb};
use crate::state::{Disposition, JobStatus};
use anyhow::{Context, Result, bail};

/// Event ids start with this, so dispatch can tell backlog work from your own PRs.
pub const PREFIX: &str = "sweep-";

pub struct Report {
    pub picked: Vec<Listed>,
    /// Newly queued; the rest were already queued by an earlier sweep.
    pub queued: usize,
}

pub async fn run(ctx: &Ctx, repo: &RepoId, target: Target, verb: Verb, mut pick: Pick, dry_run: bool) -> Result<Report> {
    let s = ctx.cfg().settings_for(&ctx.paths, repo)?;
    // A dry run only lists, so it works before you've set the repo up.
    if !dry_run && !s.subscribed {
        bail!("{repo} isn't subscribed: run `nightporter subscribe {}` first", repo.path);
    }
    if !dry_run && !s.maintainer.enabled {
        bail!("maintainer mode is off for {repo}: set enabled = true under [maintainer] in its repo file");
    }
    let forge = ctx.forge(repo)?.lock().await;
    let me = forge.me().to_string();
    if target == Target::Pr {
        // Your own PRs are already watched as PRs; a maintainer job on them would double up.
        pick.skip_author = Some(me.clone());
    }
    let items = forge.open_items(repo, target).await?;
    drop(forge);
    resolve_milestone(&mut pick, &items)?;
    let picked = crate::forge::pick(items, &pick);
    let queued = if dry_run { 0 } else { queue(ctx, repo, target, verb, &me, &picked, "`nightporter sweep`")? };
    Ok(Report { picked, queued })
}

/// `@current` becomes the open milestone due soonest among `items`.
fn resolve_milestone(pick: &mut Pick, items: &[Listed]) -> Result<()> {
    if pick.milestone.as_deref() == Some("@current") {
        // Due dates are UTC timestamps (GitHub, Gitea) or plain dates (GitLab): compare in UTC.
        let today = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
        pick.milestone =
            Some(crate::forge::current_milestone(items, &today).context("no open issue is in a milestone with a due date from today on")?);
    }
    Ok(())
}

/// `[maintainer.pickup]`, every `sweep_interval`: settle the issues it holds, then claim
/// (assign to you, with a comment) and queue unassigned matching issues until `max_claimed` are
/// held. A claim is let go when its issue closes, when its job ends without a PR to wait on, or
/// when it fails or you put it away; an issue it handed back isn't claimed again. One repo's
/// error doesn't stop the others.
pub async fn pickup(ctx: &Ctx, cfg: &crate::config::Config, key: &(crate::forge::ForgeKind, String)) -> Result<()> {
    for (repo, s) in ctx.subscribed(cfg) {
        let Some(p) = s.maintainer.pickup.as_ref().filter(|_| s.maintainer.enabled) else { continue };
        if repo.forge != key.0 || repo.host != key.1 {
            continue;
        }
        let kv = format!("pickup:{}", repo.key());
        let last: Option<i64> = ctx.db(|d| d.get(&kv))?.and_then(|v| v.parse().ok());
        if last.is_some_and(|t| crate::state::now() - t < i64::try_from(cfg.sweep_interval.as_secs()).unwrap_or(i64::MAX)) {
            continue;
        }
        match pickup_repo(ctx, &repo, &s, p).await {
            Ok(()) => ctx.db(|d| d.set(&kv, &crate::state::now().to_string()))?,
            Err(e) => ctx.warn_every(&kv, 3600, &format!("{repo}: pickup: {e:#}")),
        }
    }
    Ok(())
}

/// What happens to an issue Night Porter holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Claim {
    Hold,
    /// The issue closed, or the work is done with nothing left to wait for.
    Forget,
    /// Done without a change: hand it back, saying so if it was meant to be fixed.
    Finished,
    /// The job failed or you put it away: hand it back and don't take it again.
    GiveBack,
}

/// What a claim comment says Night Porter will do.
fn will(verb: Verb) -> &'static str {
    match verb {
        Verb::Fix => "work on a fix and open a pull request for review",
        Verb::Reproduce => "try to reproduce this",
        _ => "triage this",
    }
}

/// Why it hands an issue back, when it says so: a triage or reproduction says what it found itself.
fn why_back(c: Claim, verb: Verb) -> Option<&'static str> {
    match (c, verb) {
        (Claim::GiveBack, _) => Some("Night Porter stopped working on this"),
        (_, Verb::Fix) => Some("Night Porter looked into this and found nothing to change in the code"),
        _ => None,
    }
}

/// `job`: the newest job on the issue since it was claimed. `pending`: its events are waiting
/// to run (again, e.g. after a restart).
fn settle(open: bool, job: Option<&crate::state::Job>, pending: bool) -> Claim {
    if !open {
        return Claim::Forget;
    }
    let Some(j) = job.filter(|_| !pending) else { return Claim::Hold };
    match j.status {
        // A PR is open for it: held until the issue closes (the PR merging closes it), even if
        // something after opening it failed.
        _ if !j.meta.pr_url.is_empty() => Claim::Hold,
        JobStatus::Failed | JobStatus::Rejected | JobStatus::Dismissed => Claim::GiveBack,
        JobStatus::Done => Claim::Finished,
        _ => Claim::Hold,
    }
}

async fn pickup_repo(ctx: &Ctx, repo: &RepoId, s: &crate::config::RepoSettings, p: &crate::config::Pickup) -> Result<()> {
    let forge = ctx.forge(repo)?.lock().await;
    let me = forge.me().to_string();
    let items = forge.open_items(repo, Target::Issue).await?;
    let claim_key = |n: u64| format!("claim:{}#{n}", repo.key());
    let pending = ctx.db(|d| d.events_with(Disposition::Pending))?;
    let mut held = Vec::new();
    for (n, since) in ctx.db(|d| d.prefixed(&format!("claim:{}#", repo.key())))? {
        let (Ok(n), Ok(since)) = (n.parse::<u64>(), since.parse::<i64>()) else { continue };
        // Not in the (page-capped) list doesn't prove it's closed: ask.
        let open = items.iter().any(|i| i.number == n) || forge.issue(repo, n).await?.open;
        let job = ctx.db(|d| d.latest_job(repo, Target::Issue, n, since))?;
        let waiting = pending.iter().any(|(e, _)| e.repo == *repo && e.number == n && e.target == Target::Issue);
        match settle(open, job.as_ref(), waiting) {
            Claim::Hold => held.push(n),
            Claim::Forget => ctx.db(|d| d.del(&claim_key(n)))?,
            c @ (Claim::Finished | Claim::GiveBack) => {
                forge.assign(repo, n, false).await?;
                if let Some(why) = why_back(c, p.verb)
                    && let Err(e) = forge.comment(repo, n, &sign(s, &format!("{why}, so it's free again."))).await
                {
                    tracing::warn!("{repo}#{n}: couldn't say it's free again: {e:#}");
                }
                ctx.db(|d| d.del(&claim_key(n)))?;
                if c == Claim::GiveBack {
                    ctx.db(|d| d.set(&format!("released:{}#{n}", repo.key()), "1"))?;
                }
                tracing::info!("{repo}#{n}: handed back");
            }
        }
    }
    if held.len() >= p.max_claimed {
        return Ok(());
    }
    let mut pick = Pick { labels: p.label.iter().cloned().collect(), milestone: p.milestone.clone(), unassigned: true, ..Pick::default() };
    resolve_milestone(&mut pick, &items)?;
    if let (Some(project), Some(iteration)) = (&p.project, &p.iteration) {
        let today = jiff::Zoned::now().date().to_string();
        pick.numbers = Some(forge.iteration_issues(repo, project, iteration, &today).await?);
    }
    let released = ctx.db(|d| d.prefixed(&format!("released:{}#", repo.key())))?;
    // Not ones it holds or handed back, even if the list hasn't caught up with the assignment.
    // Nor ones something else is already on (a triage of a new issue, a sweep): one job would
    // take both requests and run only one of them.
    let busy = |n: u64| {
        pending.iter().any(|(e, _)| e.repo == *repo && e.number == n && e.target == Target::Issue)
            || ctx.db(|d| d.pr_busy(repo, n)).unwrap_or(true)
    };
    let free = crate::forge::pick(items, &pick)
        .into_iter()
        .filter(|i| !held.contains(&i.number) && !released.contains_key(&i.number.to_string()) && !busy(i.number));
    for i in free.take(p.max_claimed.saturating_sub(held.len())) {
        forge.assign(repo, i.number, true).await?;
        // Recorded the moment it's assigned, so a later failure still leaves it settled above.
        let now = crate::state::now();
        ctx.db(|d| d.set(&claim_key(i.number), &now.to_string()))?;
        queue_claim(ctx, repo, p.verb, &me, i.number, now)?;
        if let Err(e) = forge
            .comment(repo, i.number, &sign(s, &format!("Night Porter picked this up from the sprint: it will {}.", will(p.verb))))
            .await
        {
            tracing::warn!("{repo}#{}: claimed, but couldn't say so: {e:#}", i.number);
        }
        tracing::info!("{repo}#{}: claimed", i.number);
    }
    Ok(())
}

/// Jira issue numbers start here, so they never meet a forge PR/issue number in the per-PR
/// bookkeeping (one job at a time, retries, caps), whichever site they come from.
pub const JIRA_NUMBERS: u64 = 1 << 48;

/// A Jira issue a repo holds, under `jclaim:{site}:{id}`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct JiraClaim {
    pub repo: String,
    pub number: u64,
    pub since: i64,
    #[serde(flatten)]
    pub issue: crate::jira::JiraRef,
    /// Handed back or let go: not claimed again.
    #[serde(default)]
    pub released: bool,
    /// The `[[jira]]` entry's JQL, so two entries on one repo and site settle only their own.
    #[serde(default)]
    pub source: String,
}

/// The claim behind a `Target::Jira` job's number (`None` for any other number).
pub fn jira_claim(db: &crate::state::Db, repo: &RepoId, number: u64) -> Result<Option<JiraClaim>> {
    if number < JIRA_NUMBERS {
        return Ok(None);
    }
    Ok(db
        .prefixed("jclaim:")?
        .into_values()
        .filter_map(|v| serde_json::from_str::<JiraClaim>(&v).ok())
        .find(|c| c.repo == repo.key() && c.number == number))
}

/// `[[jira]]`, every `sweep_interval`: per source, settle the issues it holds, then claim new
/// matches up to `max_claimed`. Repos go by `priority`, then name; an issue another repo holds
/// is skipped with a warning. Returns the last `acli` error, if any, for `status`.
pub async fn jira_pickup(ctx: &Ctx, cfg: &crate::config::Config) -> Option<String> {
    let mut sources: Vec<(RepoId, crate::config::RepoSettings, usize)> =
        ctx.subscribed(cfg).into_iter().flat_map(|(repo, s)| (0..s.jira.len()).map(move |idx| (repo.clone(), s.clone(), idx))).collect();
    sources.sort_by_key(|(repo, s, idx)| (std::cmp::Reverse(s.jira[*idx].priority), repo.key()));
    let mut errs = Vec::new();
    for (repo, s, idx) in &sources {
        let src = &s.jira[*idx];
        if let Err(e) = jira_source(ctx, repo, s, src).await {
            let msg = format!("{repo} ({}): {e:#}", src.site);
            ctx.warn_every(&format!("jira:{}:{idx}", repo.key()), 3600, &msg);
            errs.push(msg);
        }
    }
    (!errs.is_empty()).then(|| errs.join("\n"))
}

async fn jira_source(ctx: &Ctx, repo: &RepoId, s: &crate::config::RepoSettings, src: &crate::config::JiraSource) -> Result<()> {
    let acli = crate::jira::Acli::new(&ctx.paths);
    let mine =
        acli.account(&src.site).with_context(|| format!("acli isn't logged in to {0}: acli jira auth login --site {0}", src.site))?;
    let held = settle_jira(ctx, &acli, repo, s, src, &mine).await?;
    if held < src.max_claimed {
        claim_jira(ctx, &acli, repo, s, src, &mine, held).await?;
    }
    Ok(())
}

/// Every claim on `site`, by Jira issue id.
fn jira_claims(ctx: &Ctx, site: &str) -> Result<Vec<(String, JiraClaim)>> {
    let all = ctx.db(|d| d.prefixed(&format!("jclaim:{site}:")))?;
    Ok(all.into_iter().filter_map(|(id, v)| Some((id, serde_json::from_str(&v).ok()?))).collect())
}

fn save_claim(ctx: &Ctx, site: &str, id: &str, claim: &JiraClaim) -> Result<()> {
    ctx.db(|d| d.set_json(&format!("jclaim:{site}:{id}"), claim))
}

/// The source's claims: kept, let go, or handed back. Returns how many it still holds.
async fn settle_jira(
    ctx: &Ctx,
    acli: &crate::jira::Acli,
    repo: &RepoId,
    s: &crate::config::RepoSettings,
    src: &crate::config::JiraSource,
    mine: &str,
) -> Result<usize> {
    let site = src.site.clone();
    let pending = ctx.db(|d| d.events_with(Disposition::Pending))?;
    let mut held = 0;
    let mine_only = |c: &JiraClaim| c.repo == repo.key() && c.source == src.jql && !c.released;
    let claims: Vec<(String, JiraClaim)> = jira_claims(ctx, &site)?.into_iter().filter(|(_, c)| mine_only(c)).collect();
    if claims.is_empty() {
        return Ok(0);
    }
    let (site2, keys) = (site.clone(), claims.iter().map(|(_, c)| c.issue.key.clone()).collect::<Vec<_>>());
    let issues = acli.call(move |a| Ok(a.issues(&site2, &keys))).await?;
    for ((id, mut claim), issue) in claims.into_iter().zip(issues) {
        // One issue that can't be read (deleted, moved out of sight) mustn't stop the rest.
        let issue = match issue {
            Ok(i) => i,
            Err(e) => {
                ctx.warn_every(&format!("jira-view:{site}:{id}"), 86400, &format!("{repo}: can't read {}: {e:#}", claim.issue.key));
                held += 1;
                continue;
            }
        };
        // Done, or assigned to someone else: not ours any more.
        let open = !issue.done && issue.assignee == mine;
        let job = ctx.db(|d| d.latest_job(repo, Target::Jira, claim.number, claim.since))?;
        let waiting = pending.iter().any(|(e, _)| e.repo == *repo && e.number == claim.number);
        let how = settle(open, job.as_ref(), waiting);
        if how == Claim::Hold {
            held += 1;
            continue;
        }
        // Let go for good, before saying so: it may still match the JQL, and a failed comment
        // mustn't bring it back.
        claim.released = true;
        save_claim(ctx, &site, &id, &claim)?;
        if how == Claim::Forget {
            continue;
        }
        // Someone chose you for `assigned` issues: say so, but leave the assignment.
        let unassign = src.pickup == crate::config::JiraPickup::New;
        let body = why_back(how, src.verb)
            .map(|w| sign(s, &if unassign { format!("{w}, so it's free again.") } else { format!("{w}; it stays with its assignee.") }));
        let (site2, key) = (site.clone(), claim.issue.key.clone());
        let said = acli
            .call(move |a| {
                if unassign {
                    a.assign(&site2, &key, false)?;
                }
                body.map_or(Ok(()), |b| a.comment(&site2, &key, &b))
            })
            .await;
        match said {
            Ok(()) => tracing::info!("{repo}: {} handed back", claim.issue.key),
            Err(e) => tracing::warn!("{repo}: handed {} back, but couldn't say so in Jira: {e:#}", claim.issue.key),
        }
    }
    Ok(held)
}

/// Claim and queue new matches until the source holds `max_claimed`.
async fn claim_jira(
    ctx: &Ctx,
    acli: &crate::jira::Acli,
    repo: &RepoId,
    s: &crate::config::RepoSettings,
    src: &crate::config::JiraSource,
    mine: &str,
    mut held: usize,
) -> Result<()> {
    let site = src.site.clone();
    let mut jql = src.query();
    if src.pickup == crate::config::JiraPickup::New {
        // Only issues created since this source was first seen, not the backlog.
        let k = format!("jsrc:{}:{site}:{}", repo.key(), src.jql);
        let now = crate::state::now();
        let first = ctx.db(|d| d.get(&k))?.and_then(|v| v.parse().ok()).unwrap_or(now);
        ctx.db(|d| d.set(&k, &first.to_string()))?;
        jql = format!("{jql} AND created >= -{}m", (now - first) / 60 + 1);
    }
    let claims = jira_claims(ctx, &site)?;
    // Issues it holds or let go still match, so the page is big enough to see past them. (Not
    // `key not in (…)`: one deleted key fails the whole search.) ponytail: past this many
    // matching open issues, newer ones wait until older ones close.
    let (site2, limit) = (site.clone(), 500);
    let found = acli.call(move |a| a.search(&site2, &jql, limit)).await?;
    let me = match ctx.forge(repo) {
        Ok(f) => f.lock().await.me().to_string(),
        Err(_) => String::new(),
    };
    for issue in found.into_iter().filter(|i| !i.done) {
        if held >= src.max_claimed {
            break;
        }
        if let Some((_, other)) = claims.iter().find(|(id, _)| *id == issue.id.to_string()) {
            if other.repo != repo.key() && !other.released {
                let msg = format!("{} matches {repo}'s [[jira]] too; {} has it", issue.key, other.repo);
                ctx.warn_every(&format!("jira-dup:{site}:{}", issue.id), 86400, &msg);
            }
            continue;
        }
        // Recorded before anything else, so whatever fails later still leaves it settled.
        let number = ctx.db(|d| {
            let n = d.get("jnext")?.and_then(|v| v.parse().ok()).unwrap_or(JIRA_NUMBERS);
            d.set("jnext", &(n + 1).to_string())?;
            Ok(n)
        })?;
        let jira = crate::jira::JiraRef { site: site.clone(), key: issue.key.clone(), paths: src.paths.clone() };
        let mut claim =
            JiraClaim { repo: repo.key(), number, since: crate::state::now(), issue: jira, released: false, source: src.jql.clone() };
        let id = issue.id.to_string();
        save_claim(ctx, &site, &id, &claim)?;
        if src.pickup == crate::config::JiraPickup::New {
            let (site2, key) = (site.clone(), issue.key.clone());
            // Only if still unassigned, then checked: someone may have taken it since the search.
            // One it can't assign mustn't stop the rest: skipped for good.
            let got = acli.call(move |a| a.assign(&site2, &key, true).and_then(|()| a.view(&site2, &key))).await;
            let why = match got {
                Ok(i) if i.assignee == mine => None,
                Ok(i) => Some(format!("someone else took it ({})", i.assignee)),
                Err(e) => Some(format!("{e:#}")),
            };
            if let Some(why) = why {
                tracing::warn!("{repo}: didn't claim {}: {why}", issue.key);
                claim.released = true;
                save_claim(ctx, &site, &id, &claim)?;
                continue;
            }
        }
        held += 1;
        queue_jira(ctx, repo, src, &claim, &me)?;
        let body = sign(s, &format!("Night Porter picked this up: it will {}.", will(src.verb)));
        let (site2, key) = (site.clone(), issue.key.clone());
        if let Err(e) = acli.call(move |a| a.comment(&site2, &key, &body)).await {
            tracing::warn!("{repo}: claimed {}, but couldn't say so: {e:#}", issue.key);
        }
        tracing::info!("{repo}: claimed {}", issue.key);
    }
    Ok(())
}

/// The claim's event: yours (trusted), naming the part of the repo the work belongs in.
fn queue_jira(ctx: &Ctx, repo: &RepoId, src: &crate::config::JiraSource, claim: &JiraClaim, me: &str) -> Result<()> {
    use std::fmt::Write;
    let base = event(repo, Target::Jira, src.verb, claim.number, me);
    let mut body = format!("(claimed from Jira {}: {})", claim.issue.key, crate::state::enum_str(&src.verb));
    if !src.paths.is_empty() {
        let _ = write!(body, "\nThis issue belongs in this part of the repository: {}. Keep the work there.", src.paths.join(", "));
    }
    let e = Event { id: format!("{}-jira-{}", base.id, claim.since), body, url: claim.issue.url(), ..base };
    ctx.db(|d| d.add_event(&e, Disposition::Pending, "jira")).map(|_| ())
}

/// The claim's own event (its id carries the claim time), so an issue swept or claimed before
/// still gets a new job.
fn queue_claim(ctx: &Ctx, repo: &RepoId, verb: Verb, me: &str, n: u64, at: i64) -> Result<()> {
    let base = event(repo, Target::Issue, verb, n, me);
    let e = Event {
        id: format!("{}-pickup-{at}", base.id),
        body: format!("(claimed by sprint pickup: {})", crate::state::enum_str(&verb)),
        ..base
    };
    ctx.db(|d| d.add_event(&e, Disposition::Pending, "pickup")).map(|_| ())
}

/// A post as you, marked as Night Porter's per the repo's `reply` settings.
fn sign(s: &crate::config::RepoSettings, body: &str) -> String {
    crate::prompts::sign(body, s.reply.attribution(), s.agent)
}

/// One pending, trusted (it's you) maintainer event per item. Ids are stable, so running the
/// same sweep again doesn't queue anything twice.
pub fn queue(ctx: &Ctx, repo: &RepoId, target: Target, verb: Verb, me: &str, items: &[Listed], by: &str) -> Result<usize> {
    let mut n = 0;
    for i in items {
        let e = Event { body: format!("(queued by {by}: {})", crate::state::enum_str(&verb)), ..event(repo, target, verb, i.number, me) };
        n += usize::from(ctx.db(|d| d.add_event(&e, Disposition::Pending, by))?);
    }
    Ok(n)
}

/// Same id as `sweep --verb review`, so a PR swept and watched waits once.
fn event(repo: &RepoId, target: Target, verb: Verb, number: u64, author: &str) -> Event {
    Event {
        id: format!("{PREFIX}{}-{}-{}-{number}", crate::state::enum_str(&verb), repo.key(), crate::state::enum_str(&target)),
        repo: repo.clone(),
        target,
        number,
        trigger: Trigger::Comment,
        author: author.to_string(),
        author_id: 0,
        role: Role::Owner,
        is_bot: false,
        body: String::new(),
        url: String::new(),
        thread: None,
        path: None,
        line: None,
        created_at: crate::forge::now_iso(),
        verb: Some(verb),
    }
}

/// `[maintainer] triage_new` / `review_new`: every `sweep_interval`, queue a triage of each issue
/// and a review of each ready PR/MR by someone else opened since that setting was turned on (the
/// backlog before is left to `nightporter sweep`; turning one off and on again starts afresh).
/// Authors on `trust.deny_users` / `deny_teams` are skipped. Stable ids: once each. Runs before
/// `watch_prs`, so a PR reviewed here isn't also put in the inbox.
pub async fn watch_new(ctx: &Ctx, cfg: &crate::config::Config, key: &(crate::forge::ForgeKind, String)) -> Result<()> {
    for (repo, s) in ctx.subscribed(cfg) {
        if repo.forge != key.0 || repo.host != key.1 {
            continue;
        }
        let kv = format!("watch-new:{}", repo.key());
        let last: Option<i64> = ctx.db(|d| d.get(&kv))?.and_then(|v| v.parse().ok());
        if last.is_some_and(|t| crate::state::now() - t < i64::try_from(cfg.sweep_interval.as_secs()).unwrap_or(i64::MAX)) {
            continue;
        }
        match watch_new_repo(ctx, &repo, &s).await {
            Ok(0) => {}
            Ok(n) => tracing::info!("{repo}: queued {n} new issue(s)/PR(s)"),
            Err(e) => {
                ctx.warn_every(&kv, 3600, &format!("{repo}: triage/review new: {e:#}"));
                continue;
            }
        }
        ctx.db(|d| d.set(&kv, &crate::state::now().to_string()))?;
    }
    Ok(())
}

async fn watch_new_repo(ctx: &Ctx, repo: &RepoId, s: &crate::config::RepoSettings) -> Result<usize> {
    let m = &s.maintainer;
    let mut queued = 0;
    for (on, target, verb, name) in
        [(m.triage_new, Target::Issue, Verb::Triage, "triage_new"), (m.review_new, Target::Pr, Verb::Review, "review_new")]
    {
        let since_key = format!("new-since:{name}:{}", repo.key());
        if !(on && m.enabled) {
            ctx.db(|d| d.del(&since_key))?;
            continue;
        }
        let Some(since) = ctx.db(|d| d.get(&since_key))?.and_then(|v| v.parse::<jiff::Timestamp>().ok()) else {
            ctx.db(|d| d.set(&since_key, &jiff::Timestamp::now().to_string()))?;
            continue;
        };
        let forge = ctx.forge(repo)?.lock().await;
        let me = forge.me().to_string();
        let pick = Pick { created_after: Some(since), skip_author: Some(me.clone()), ..Pick::default() };
        let mut items = Vec::new();
        for i in crate::forge::pick(forge.open_items(repo, target).await?, &pick).into_iter().filter(|i| !i.draft) {
            if !denied(&forge, &s.trust, &i.author).await {
                items.push(i);
            }
        }
        drop(forge);
        queued += queue(ctx, repo, target, verb, &me, &items, name)?;
    }
    Ok(queued)
}

/// On a deny list (a team lookup that fails counts as denied).
async fn denied(forge: &crate::forge::Forge, t: &crate::config::Trust, author: &str) -> bool {
    if t.deny_users.iter().any(|u| u.eq_ignore_ascii_case(author)) {
        return true;
    }
    for team in &t.deny_teams {
        if forge.in_team(team, author).await.unwrap_or(true) {
            return true;
        }
    }
    false
}

/// `[maintainer] watch_prs`: every `sweep_interval`, put each ready PR/MR by someone else in the
/// inbox. Allow once queues a review (at maintainer level); ignore drops it for good.
pub async fn watch_prs(ctx: &Ctx, cfg: &crate::config::Config, key: &(crate::forge::ForgeKind, String)) -> Result<()> {
    for (repo, s) in ctx.subscribed(cfg) {
        if repo.forge != key.0 || repo.host != key.1 || !s.maintainer.enabled || !s.maintainer.watch_prs {
            continue;
        }
        let kv = format!("watch-prs:{}", repo.key());
        let last: Option<i64> = ctx.db(|d| d.get(&kv))?.and_then(|v| v.parse().ok());
        let every = i64::try_from(cfg.sweep_interval.as_secs()).unwrap_or(i64::MAX);
        if last.is_some_and(|t| crate::state::now() - t < every) {
            continue;
        }
        let forge = ctx.forge(&repo)?.lock().await;
        let me = forge.me().to_string();
        let items = forge.open_items(&repo, Target::Pr).await?;
        drop(forge);
        // The first look at a repo fills the inbox with its backlog quietly; later ones notify.
        let n = inbox_new(ctx, &repo, &me, &items, last.is_some())?;
        ctx.db(|d| d.set(&kv, &crate::state::now().to_string()))?;
        if n > 0 {
            tracing::info!("{repo}: {n} new ready PR(s) in the inbox");
        }
    }
    Ok(())
}

/// Inbox events for `items` not drafts and not yours. Returns how many are new.
pub fn inbox_new(ctx: &Ctx, repo: &RepoId, me: &str, items: &[Listed], notify: bool) -> Result<usize> {
    let mut n = 0;
    for i in items.iter().filter(|i| !i.draft && !i.author.eq_ignore_ascii_case(me)) {
        // The title is the author's text: kept out of the body, which becomes a trusted request
        // once allowed. The job reads the PR itself, marked untrusted.
        let e = Event {
            role: Role::None,
            body: "(ready PR you weren't asked to review: allow once to queue a review)".into(),
            ..event(repo, Target::Pr, Verb::Review, i.number, &i.author)
        };
        if !ctx.db(|d| d.add_event(&e, Disposition::Inbox, "watch_prs"))? {
            continue;
        }
        n += 1;
        if notify {
            let msg = format!("@{} opened a PR: review it?", i.author);
            ctx.notify(repo, &crate::notify::Event::about(crate::notify::Kind::NewAuthor, repo, Target::Pr, i.number, &msg));
        }
    }
    Ok(n)
}

pub fn is_sweep(e: &Event) -> bool {
    e.id.starts_with(PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::ForgeKind;

    #[tokio::test]
    async fn jira_pickup_claims_queues_and_settles() {
        let d = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        let f = d.path().join("fake");
        std::fs::create_dir_all(&f).unwrap();
        let bin = f.join("acli");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nd={}\necho \"$*\" >> $d/calls\ncase \"$2 $3\" in\n\
                 'auth status') echo \"  Site: $(cat $d/site)\" ;;\n\
                 'auth switch') echo \"$5\" > $d/site ;;\n\
                 'workitem search') cat $d/search.json ;;\n\
                 'workitem view') cat $d/view-$4.json ;;\n\
                 esac\n",
                f.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::write(f.join("site"), "home.jira.com\n").unwrap();
        std::fs::write(f.join("jira_config.yaml"), "profiles:\n    - site: s.atlassian.net\n      account_id: me1\n").unwrap();
        crate::jira::install_fake(&paths.data_dir, bin, f.join("jira_config.yaml"));
        let issue = |id: u32, key: &str, status: &str, assignee: &str| {
            format!(
                r#"{{"id":"{id}","key":"{key}","fields":{{"summary":"Broken","status":{{"statusCategory":{{"key":"{status}"}}}},
                   "assignee":{assignee},"reporter":{{"accountId":"pm","displayName":"PM"}}}}}}"#
            )
        };
        std::fs::write(f.join("search.json"), format!("[{}]", issue(101, "TKF-1", "new", r#"{"accountId":"me1"}"#))).unwrap();
        std::fs::write(f.join("view-TKF-1.json"), issue(101, "TKF-1", "indeterminate", r#"{"accountId":"me1"}"#)).unwrap();

        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let file = paths.repo_file(&repo);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let toml = "subscribed = true\n[[jira]]\nsite = \"s.atlassian.net\"\njql = \"project = TKF\"\npaths = [\"frontend/\"]\n";
        std::fs::write(&file, toml).unwrap();
        let cfg = crate::config::Config::parse("").unwrap();
        let clones = crate::repos::CloneMap::from([(repo.clone(), d.path().join("clone"))]);
        let ctx = crate::daemon::test_ctx(paths, cfg.clone(), clones);
        let calls = || std::fs::read_to_string(f.join("calls")).unwrap_or_default();
        let pending = || ctx.db(|d| d.events_with(Disposition::Pending)).unwrap();

        // Assigned to you: claimed without assigning, queued with the paths hint, said on Jira.
        assert_eq!(jira_pickup(&ctx, &cfg).await, None);
        let evs = pending();
        assert_eq!(evs.len(), 1);
        let e = &evs[0].0;
        assert_eq!((e.target, e.number, e.url.as_str()), (Target::Jira, JIRA_NUMBERS, "https://s.atlassian.net/browse/TKF-1"));
        assert!(e.body.contains("frontend/"), "{}", e.body);
        let c = ctx.db(|d| jira_claim(d, &repo, JIRA_NUMBERS)).unwrap().unwrap();
        assert_eq!((c.issue.key.as_str(), c.issue.paths.as_slice()), ("TKF-1", ["frontend/".to_string()].as_slice()));
        assert!(calls().contains("assignee = currentUser() AND statusCategory != Done AND (project = TKF)"), "{}", calls());
        assert!(calls().contains("comment create --key TKF-1") && !calls().contains("workitem assign"), "{}", calls());
        assert_eq!(std::fs::read_to_string(f.join("site")).unwrap().trim(), "home.jira.com", "acli back where it was");

        // Held: the same match isn't queued twice.
        assert_eq!(jira_pickup(&ctx, &cfg).await, None);
        assert_eq!(pending().len(), 1);

        // Done in Jira: let go for good, and not claimed again in the same pass, even if a search
        // still returns it.
        std::fs::write(f.join("view-TKF-1.json"), issue(101, "TKF-1", "done", r#"{"accountId":"me1"}"#)).unwrap();
        ctx.db(|d| d.move_event(&e.id, Disposition::Pending, Disposition::Dropped, "test")).unwrap();
        std::fs::write(f.join("search.json"), format!("[{}]", issue(101, "TKF-1", "done", r#"{"accountId":"me1"}"#))).unwrap();
        assert_eq!(jira_pickup(&ctx, &cfg).await, None);
        assert!(ctx.db(|d| jira_claim(d, &repo, JIRA_NUMBERS)).unwrap().unwrap().released);
        assert_eq!(pending().len(), 0);

        // `new`: unassigned issues created since the source was first seen, claimed by assigning.
        std::fs::write(&file, toml.replace("paths", "pickup = \"new\"\npaths")).unwrap();
        // Someone took TKF-3 between the search and the assignment: skipped for good, not queued.
        let found = [issue(102, "TKF-2", "new", "null"), issue(103, "TKF-3", "new", "null")].join(",");
        std::fs::write(f.join("search.json"), format!("[{found}]")).unwrap();
        std::fs::write(f.join("view-TKF-2.json"), issue(102, "TKF-2", "new", r#"{"accountId":"me1"}"#)).unwrap();
        std::fs::write(f.join("view-TKF-3.json"), issue(103, "TKF-3", "new", r#"{"accountId":"pat"}"#)).unwrap();
        assert_eq!(jira_pickup(&ctx, &cfg).await, None);
        assert!(calls().contains("assignee is EMPTY AND statusCategory != Done AND (project = TKF) AND created >= -1m"), "{}", calls());
        assert!(calls().contains("assign --jql key = TKF-2 AND assignee is EMPTY --yes --assignee @me"), "{}", calls());
        assert_eq!(ctx.db(|d| jira_claim(d, &repo, JIRA_NUMBERS + 1)).unwrap().unwrap().issue.key, "TKF-2");
        let taken = ctx.db(|d| jira_claim(d, &repo, JIRA_NUMBERS + 2)).unwrap().unwrap();
        assert!(taken.issue.key == "TKF-3" && taken.released);
        assert_eq!(pending().len(), 1, "only TKF-2 queued");
    }

    #[test]
    fn claims_settle_by_their_own_job() {
        let mut db = crate::state::Db::memory().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&repo, Target::Issue, 1, "maintainer", "draft", &[]).unwrap();
        let job = |status, pr: &str| {
            let mut j = db.job(id).unwrap().unwrap();
            (j.status, j.meta.pr_url) = (status, pr.into());
            j
        };
        assert_eq!(settle(false, None, false), Claim::Forget, "closed");
        assert_eq!(settle(true, None, false), Claim::Hold, "not dispatched yet");
        assert_eq!(settle(true, Some(&job(JobStatus::Failed, "")), true), Claim::Hold, "re-queued after a restart");
        assert_eq!(settle(true, Some(&job(JobStatus::Failed, "")), false), Claim::GiveBack);
        assert_eq!(settle(true, Some(&job(JobStatus::Dismissed, "")), false), Claim::GiveBack);
        assert_eq!(settle(true, Some(&job(JobStatus::Done, "https://x/pull/3")), false), Claim::Hold, "PR open: wait for it");
        assert_eq!(settle(true, Some(&job(JobStatus::Done, "")), false), Claim::Finished, "triaged, or nothing to change");
        assert_eq!(settle(true, Some(&job(JobStatus::AwaitingApproval, "")), false), Claim::Hold);
    }

    #[test]
    fn queues_once_per_item_and_verb() {
        let d = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::at(&d.path().join("c"), &d.path().join("d"));
        let ctx = crate::daemon::test_ctx(paths, crate::config::Config::parse("").unwrap(), crate::repos::CloneMap::default());
        let repo = RepoId::new(ForgeKind::GitLab, "git.drupalcode.org", "project/x");
        let it = |n| Listed { number: n, author: "a".into(), ..Default::default() };
        let items = [it(1), it(2)];
        assert_eq!(queue(&ctx, &repo, Target::Issue, Verb::Triage, "me", &items, "sweep").unwrap(), 2);
        assert_eq!(queue(&ctx, &repo, Target::Issue, Verb::Triage, "me", &items, "sweep").unwrap(), 0);
        assert_eq!(queue(&ctx, &repo, Target::Issue, Verb::Fix, "me", &items[..1], "sweep").unwrap(), 1);
        let pending = ctx.db(|d| d.events_with(Disposition::Pending)).unwrap();
        assert_eq!(pending.len(), 3);
        assert!(pending.iter().all(|(e, _)| is_sweep(e) && e.author == "me" && e.verb.is_some()));
    }

    #[test]
    fn watch_puts_others_ready_prs_in_the_inbox_once() {
        let d = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::at(&d.path().join("c"), &d.path().join("d"));
        let ctx = crate::daemon::test_ctx(paths, crate::config::Config::parse("").unwrap(), crate::repos::CloneMap::default());
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let it = |n, who: &str, draft| Listed { number: n, author: who.into(), draft, ..Default::default() };
        let items = [it(1, "ann", false), it(2, "ann", true), it(3, "Me", false)];
        assert_eq!(inbox_new(&ctx, &repo, "me", &items, false).unwrap(), 1, "drafts and your own PRs are skipped");
        assert_eq!(inbox_new(&ctx, &repo, "me", &items, false).unwrap(), 0);
        // Marked ready later: it shows up then.
        assert_eq!(inbox_new(&ctx, &repo, "me", &[it(2, "ann", false)], false).unwrap(), 1);
        let inbox = ctx.db(|d| d.events_with(Disposition::Inbox)).unwrap();
        assert!(inbox.iter().all(|(e, _)| is_sweep(e) && e.author == "ann" && e.verb == Some(Verb::Review)));
        // A `sweep --verb review` of the same PR doesn't queue it twice.
        assert_eq!(queue(&ctx, &repo, Target::Pr, Verb::Review, "me", &items[..1], "sweep").unwrap(), 0);
    }
}
