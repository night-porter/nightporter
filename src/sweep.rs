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
    let queued = if dry_run { 0 } else { queue(ctx, repo, target, verb, &me, &picked)? };
    Ok(Report { picked, queued })
}

/// `@current` becomes the open milestone due soonest among `items`.
fn resolve_milestone(pick: &mut Pick, items: &[Listed]) -> Result<()> {
    if pick.milestone.as_deref() == Some("@current") {
        let today = jiff::Zoned::now().date().to_string();
        pick.milestone =
            Some(crate::forge::current_milestone(items, &today).context("no open issue is in a milestone with a due date from today on")?);
    }
    Ok(())
}

/// `[maintainer.pickup]`, every `sweep_interval`: release issues whose job failed or was put
/// away, forget closed ones, then claim (assign to you, with a comment) and queue unassigned
/// matching issues until `max_claimed` are held. An issue it released isn't claimed again.
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
        ctx.db(|d| d.set(&kv, &crate::state::now().to_string()))?;
        let forge = ctx.forge(&repo)?.lock().await;
        let me = forge.me().to_string();
        let items = forge.open_items(&repo, Target::Issue).await?;
        let claim_key = |n: u64| format!("claim:{}#{n}", repo.key());
        let mut held = Vec::new();
        for n in ctx.db(|d| d.prefixed(&format!("claim:{}#", repo.key())))?.keys().filter_map(|n| n.parse::<u64>().ok()) {
            if !items.iter().any(|i| i.number == n) {
                ctx.db(|d| d.del(&claim_key(n)))?; // closed: done
                continue;
            }
            let job = ctx.db(|d| d.latest_job(&repo, n))?;
            if job.is_some_and(|j| matches!(j.status, JobStatus::Failed | JobStatus::Rejected | JobStatus::Dismissed)) {
                forge.assign(&repo, n, false).await?;
                forge.comment(&repo, n, &sign(&s, "Night Porter couldn't finish this one, so it's free again.")).await?;
                ctx.db(|d| d.del(&claim_key(n)))?;
                ctx.db(|d| d.set(&format!("released:{}#{n}", repo.key()), "1"))?;
                tracing::info!("{repo}#{n}: released");
                continue;
            }
            held.push(n);
        }
        let mut pick =
            Pick { labels: p.label.iter().cloned().collect(), milestone: p.milestone.clone(), unassigned: true, ..Pick::default() };
        if let Err(e) = resolve_milestone(&mut pick, &items) {
            ctx.warn_every(&kv, 86400, &format!("{repo}: pickup: {e:#}"));
            continue;
        }
        let released = ctx.db(|d| d.prefixed(&format!("released:{}#", repo.key())))?;
        // Not ones it holds or let go, even if the list hasn't caught up with the assignment yet.
        let free = crate::forge::pick(items, &pick)
            .into_iter()
            .filter(|i| !held.contains(&i.number) && !released.contains_key(&i.number.to_string()));
        for i in free.take(p.max_claimed.saturating_sub(held.len())) {
            forge.assign(&repo, i.number, true).await?;
            let what = match p.verb {
                Verb::Fix => "work on this and open a draft PR",
                Verb::Reproduce => "try to reproduce this",
                _ => "triage this",
            };
            forge.comment(&repo, i.number, &sign(&s, &format!("Night Porter picked this up from the sprint and will {what}."))).await?;
            ctx.db(|d| d.set(&claim_key(i.number), "1"))?;
            queue(ctx, &repo, Target::Issue, p.verb, &me, std::slice::from_ref(&i))?;
            tracing::info!("{repo}#{}: claimed", i.number);
        }
    }
    Ok(())
}

/// A post as you, marked as Night Porter's per the repo's `reply` settings.
fn sign(s: &crate::config::RepoSettings, body: &str) -> String {
    crate::prompts::sign(body, s.reply.attribution(), s.agent)
}

/// One pending, trusted (it's you) maintainer event per item. Ids are stable, so running the
/// same sweep again doesn't queue anything twice.
pub fn queue(ctx: &Ctx, repo: &RepoId, target: Target, verb: Verb, me: &str, items: &[Listed]) -> Result<usize> {
    let mut n = 0;
    for i in items {
        let e = Event {
            body: format!("(queued by `nightporter sweep`: {})", crate::state::enum_str(&verb)),
            ..event(repo, target, verb, i.number, me)
        };
        n += usize::from(ctx.db(|d| d.add_event(&e, Disposition::Pending, "sweep"))?);
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
    fn queues_once_per_item_and_verb() {
        let d = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::at(&d.path().join("c"), &d.path().join("d"));
        let ctx = crate::daemon::test_ctx(paths, crate::config::Config::parse("").unwrap(), crate::repos::CloneMap::default());
        let repo = RepoId::new(ForgeKind::GitLab, "git.drupalcode.org", "project/x");
        let it = |n| Listed { number: n, author: "a".into(), ..Default::default() };
        let items = [it(1), it(2)];
        assert_eq!(queue(&ctx, &repo, Target::Issue, Verb::Triage, "me", &items).unwrap(), 2);
        assert_eq!(queue(&ctx, &repo, Target::Issue, Verb::Triage, "me", &items).unwrap(), 0);
        assert_eq!(queue(&ctx, &repo, Target::Issue, Verb::Fix, "me", &items[..1]).unwrap(), 1);
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
        assert_eq!(queue(&ctx, &repo, Target::Pr, Verb::Review, "me", &items[..1]).unwrap(), 0);
    }
}
