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
#[derive(Debug, PartialEq, Eq)]
enum Claim {
    Hold,
    /// The issue closed, or the work is done with nothing left to wait for.
    Forget,
    /// Done without a change: hand it back, saying so if it was meant to be fixed.
    Finished,
    /// The job failed or you put it away: hand it back and don't take it again.
    GiveBack,
}

/// `job`: the newest job on the issue since it was claimed. `pending`: its events are waiting
/// to run (again, e.g. after a restart).
fn settle(open: bool, job: Option<&crate::state::Job>, pending: bool) -> Claim {
    if !open {
        return Claim::Forget;
    }
    let Some(j) = job.filter(|_| !pending) else { return Claim::Hold };
    match j.status {
        JobStatus::Failed | JobStatus::Rejected | JobStatus::Dismissed => Claim::GiveBack,
        // A PR is open for it: held until the issue closes (the PR merging closes it).
        JobStatus::Done if !j.meta.pr_url.is_empty() => Claim::Hold,
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
                let why = match (&c, p.verb) {
                    (Claim::GiveBack, _) => Some("Night Porter couldn't finish this one, so it's free again."),
                    (_, Verb::Fix) => Some("Night Porter found nothing to change here, so it's free again."),
                    _ => None, // a triage or reproduction says what it found itself
                };
                if let Some(why) = why
                    && let Err(e) = forge.comment(repo, n, &sign(s, why)).await
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
        let what = match p.verb {
            Verb::Fix => "work on this and open a PR",
            Verb::Reproduce => "try to reproduce this",
            _ => "triage this",
        };
        if let Err(e) =
            forge.comment(repo, i.number, &sign(s, &format!("Night Porter picked this up from the sprint and will {what}."))).await
        {
            tracing::warn!("{repo}#{}: claimed, but couldn't say so: {e:#}", i.number);
        }
        tracing::info!("{repo}#{}: claimed", i.number);
    }
    Ok(())
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
