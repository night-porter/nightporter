//! `nightporter sweep`: queue maintainer jobs for a repo's backlog of open issues or PRs/MRs,
//! picked by label and age. The jobs go through the normal queue (debounce, schedule, caps) and
//! stop at maintainer mode's level (at most draft), so you review each one.

use crate::daemon::Ctx;
use crate::forge::{Event, Listed, Pick, RepoId, Role, Target, Trigger, Verb};
use crate::state::Disposition;
use anyhow::{Result, bail};

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
    let picked = crate::forge::pick(items, &pick);
    let queued = if dry_run { 0 } else { queue(ctx, repo, target, verb, &me, &picked)? };
    Ok(Report { picked, queued })
}

/// One pending, trusted (it's you) maintainer event per item. Ids are stable, so running the
/// same sweep again doesn't queue anything twice.
pub fn queue(ctx: &Ctx, repo: &RepoId, target: Target, verb: Verb, me: &str, items: &[Listed]) -> Result<usize> {
    let verb_s = crate::state::enum_str(&verb);
    let mut n = 0;
    for i in items {
        let e = Event {
            id: format!("{PREFIX}{verb_s}-{}-{}-{}", repo.key(), crate::state::enum_str(&target), i.number),
            repo: repo.clone(),
            target,
            number: i.number,
            trigger: Trigger::Comment,
            author: me.to_string(),
            author_id: 0,
            role: Role::Owner,
            is_bot: false,
            body: format!("(queued by `nightporter sweep`: {verb_s})"),
            url: String::new(),
            thread: None,
            path: None,
            line: None,
            created_at: crate::forge::now_iso(),
            verb: Some(verb),
        };
        n += usize::from(ctx.db(|d| d.add_event(&e, Disposition::Pending, "sweep"))?);
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
        let it = |n| Listed { number: n, title: String::new(), author: "a".into(), created_at: String::new(), labels: vec![] };
        let items = [it(1), it(2)];
        assert_eq!(queue(&ctx, &repo, Target::Issue, Verb::Triage, "me", &items).unwrap(), 2);
        assert_eq!(queue(&ctx, &repo, Target::Issue, Verb::Triage, "me", &items).unwrap(), 0);
        assert_eq!(queue(&ctx, &repo, Target::Issue, Verb::Fix, "me", &items[..1]).unwrap(), 1);
        let pending = ctx.db(|d| d.events_with(Disposition::Pending)).unwrap();
        assert_eq!(pending.len(), 3);
        assert!(pending.iter().all(|(e, _)| is_sweep(e) && e.author == "me" && e.verb.is_some()));
    }
}
