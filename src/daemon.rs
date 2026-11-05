//! The daemon: poll forges, classify events, batch them into jobs, run jobs, deliver approvals.

use crate::config::{Config, Level};
use crate::forge::{Cursor, Event, Forge, ForgeKind, Mode, RepoId, Trigger, github::GitHub, gitlab::GitLab};
use crate::http::{self, Api};
use crate::notify::{self, Kind};
use crate::paths::Paths;
use crate::repos::{self, CloneMap};
use crate::state::{Db, Disposition, JobMeta, JobStatus};
use crate::{job, schedule, trust};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

pub struct Ctx {
    pub paths: Paths,
    cfg: RwLock<Arc<Config>>,
    db: Mutex<Db>,
    pub forges: HashMap<(ForgeKind, String), tokio::sync::Mutex<Forge>>,
    clones: RwLock<Arc<CloneMap>>,
    /// Jobs with a task in flight, and their repo (for per-repo limits).
    running: Mutex<HashMap<i64, RepoId>>,
    /// Last time a recurring warning was logged, so the 5s loop doesn't spam.
    warned: Mutex<HashMap<String, i64>>,
}

impl Ctx {
    pub fn cfg(&self) -> Arc<Config> {
        self.cfg.read().unwrap().clone()
    }
    /// Short, synchronous DB access. Never hold across an `.await`.
    pub fn db<T>(&self, f: impl FnOnce(&mut Db) -> Result<T>) -> Result<T> {
        f(&mut self.db.lock().unwrap())
    }
    pub fn forge(&self, repo: &RepoId) -> Result<&tokio::sync::Mutex<Forge>> {
        self.forges.get(&(repo.forge, repo.host.clone())).with_context(|| format!("no {} account for {}", repo.forge.as_str(), repo.host))
    }
    pub fn clone_for(&self, repo: &RepoId) -> Option<std::path::PathBuf> {
        self.clones.read().unwrap().get(repo).cloned()
    }
    pub fn clones(&self) -> Arc<CloneMap> {
        self.clones.read().unwrap().clone()
    }
    /// Log a warning at most once per `secs` for the same key.
    pub fn warn_every(&self, key: &str, secs: i64, msg: &str) {
        let now = crate::state::now();
        let mut w = self.warned.lock().unwrap();
        if w.get(key).is_none_or(|t| now - t >= secs) {
            w.insert(key.into(), now);
            tracing::warn!("{msg}");
        }
    }
    pub fn notify(&self, kind: Kind, title: &str, body: &str) {
        notify::send(&self.cfg().notifications, kind, title, body);
    }
    fn subscribed(&self, cfg: &Config) -> Vec<(RepoId, crate::config::RepoSettings)> {
        self.clones()
            .keys()
            .filter_map(|r| cfg.settings_for(&self.paths, r).ok().filter(|s| s.subscribed).map(|s| (r.clone(), s)))
            .collect()
    }
}

/// Build forge clients for every configured account and log in.
pub async fn connect(cfg: &Config) -> Result<HashMap<(ForgeKind, String), tokio::sync::Mutex<Forge>>> {
    let mut out = HashMap::new();
    for a in &cfg.accounts {
        let host = a.host().to_string();
        let token = http::token(a.forge, &host, a.token).await?;
        let base = std::env::var("NIGHTPORTER_API_BASE").ok().filter(|_| cfg!(debug_assertions));
        let mut f = match a.forge {
            ForgeKind::GitHub => {
                Forge::GitHub(GitHub::new(&host, Api::new(&base.unwrap_or_else(|| GitHub::api_base(&host)), token, false)?))
            }
            ForgeKind::GitLab => {
                Forge::GitLab(GitLab::new(&host, Api::new(&base.unwrap_or_else(|| GitLab::api_base(&host)), token, true)?))
            }
        };
        f.login().await.with_context(|| format!("log in to {host}"))?;
        tracing::info!("logged in to {host} as {}", f.me());
        out.insert((a.forge, host), tokio::sync::Mutex::new(f));
    }
    Ok(out)
}

pub async fn run(paths: Paths) -> Result<()> {
    paths.ensure()?;
    // Single instance: the lock is held for the life of the process.
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(paths.lock())?;
    lock.try_lock().map_err(|_| anyhow::anyhow!("another nightporter daemon is already running ({})", paths.lock().display()))?;

    let cfg = Config::load(&paths)?;
    let db = Db::open(&paths.db())?;
    let n = db.recover()?;
    if n > 0 {
        tracing::warn!("{n} job(s) were interrupted by a restart; their requests are queued again");
        notify::send(&cfg.notifications, Kind::Failed, "restarted", &format!("{n} interrupted job(s) re-queued"));
    }
    let forges = connect(&cfg).await?;
    let clones = repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts);
    tracing::info!("found {} repo remotes under {}", clones.len(), cfg.code_dir().display());
    let ctx = Arc::new(Ctx {
        paths: paths.clone(),
        cfg: RwLock::new(Arc::new(cfg)),
        db: Mutex::new(db),
        forges,
        clones: RwLock::new(Arc::new(clones)),
        running: Mutex::new(HashMap::new()),
        warned: Mutex::new(HashMap::new()),
    });

    // Reload config when files change.
    let dirty = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _watcher = watch(&paths, dirty.clone());

    let mut next_poll: HashMap<(ForgeKind, String), Instant> = HashMap::new();
    let mut last_scan = Instant::now();
    let mut last_housekeeping = Instant::now() - Duration::from_secs(3600);
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = tokio::signal::ctrl_c() => { tracing::info!("shutting down"); return Ok(()); }
        }
        if dirty.swap(false, std::sync::atomic::Ordering::Relaxed) || last_scan.elapsed() > Duration::from_secs(900) {
            reload(&ctx);
            last_scan = Instant::now();
        }
        let cfg = ctx.cfg();
        for key in ctx.forges.keys() {
            if next_poll.get(key).is_some_and(|t| Instant::now() < *t) {
                continue;
            }
            let wait = match poll(&ctx, key).await {
                Ok(server_min) => cfg.poll_interval.max(Duration::from_secs(server_min)),
                Err(e) => {
                    tracing::warn!("poll {}: {e:#}", key.1);
                    cfg.poll_interval * 2
                }
            };
            next_poll.insert(key.clone(), Instant::now() + wait);
        }
        if let Err(e) = dispatch(&ctx) {
            tracing::error!("dispatch: {e:#}");
        }
        if let Err(e) = start_jobs(&ctx) {
            tracing::error!("start jobs: {e:#}");
        }
        if last_housekeeping.elapsed() > Duration::from_secs(3600) {
            last_housekeeping = Instant::now();
            let c = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = job::housekeeping(&c).await {
                    tracing::warn!("housekeeping: {e:#}");
                }
            });
        }
    }
}

fn reload(ctx: &Ctx) {
    match Config::load(&ctx.paths) {
        Ok(c) => {
            let clones = repos::scan(&c.code_dir(), c.scan_depth, &c.accounts);
            *ctx.clones.write().unwrap() = Arc::new(clones);
            *ctx.cfg.write().unwrap() = Arc::new(c);
            tracing::info!("config reloaded");
        }
        Err(e) => tracing::error!("config not reloaded, keeping the previous one: {e:#}"),
    }
}

fn watch(paths: &Paths, dirty: Arc<std::sync::atomic::AtomicBool>) -> Option<::notify::RecommendedWatcher> {
    use ::notify::Watcher;
    let mut w = ::notify::recommended_watcher(move |r: ::notify::Result<::notify::Event>| {
        if r.is_ok_and(|e| e.paths.iter().any(|p| p.extension().is_some_and(|x| x == "toml" || x == "lock"))) {
            dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
    .ok()?;
    w.watch(&paths.config_dir, ::notify::RecursiveMode::Recursive).ok()?;
    Some(w)
}

/// Poll one account. Returns the server's minimum poll interval in seconds.
pub(crate) async fn poll(ctx: &Ctx, key: &(ForgeKind, String)) -> Result<u64> {
    let cfg = ctx.cfg();
    let subs = ctx.subscribed(&cfg);
    let mut forge = ctx.forges[key].lock().await;
    let mine: Vec<_> = subs.iter().filter(|(r, _)| r.forge == key.0 && r.host == key.1).collect();
    let set: HashSet<String> = mine.iter().map(|(r, _)| r.path.clone()).collect();
    let labels: HashMap<String, String> =
        mine.iter().filter(|(_, s)| s.maintainer.enabled).map(|(r, s)| (r.path.clone(), s.maintainer.label.clone())).collect();
    match &mut *forge {
        Forge::GitHub(g) => {
            g.subscribed = set;
            g.labels = labels;
            g.sweep_every = cfg.sweep_interval;
        }
        Forge::GitLab(g) => {
            g.subscribed = set;
            g.labels = labels;
            g.sweep_every = cfg.sweep_interval;
        }
    }
    if mine.is_empty() {
        return Ok(0);
    }
    let ckey = format!("cursor:{}:{}", key.0.as_str(), key.1);
    let mut cursor: Cursor = ctx.db(|d| d.get_json(&ckey))?;
    let events = forge.poll(&mut cursor).await?;
    let me = forge.me().to_string();
    // CI went green → reset CI-fix attempt counters.
    for (k, st) in &cursor.seen_status {
        if st.contains("green")
            && let Some((repo, n)) = k.rsplit_once('#')
            && let (Some(r), Ok(n)) = (RepoId::parse_key(repo), n.parse())
        {
            ctx.db(|d| d.set_ci_attempts(&r, n, 0))?;
        }
    }
    drop(forge);
    // Intake first, cursor second: a crash in between re-reads (de-duplicated) instead of losing.
    for e in events {
        if let Err(err) = intake(ctx, &cfg, &me, e.clone()).await {
            // Keep it visible rather than dropping it (e.g. a broken repo file).
            tracing::warn!("intake {}: {err:#}", e.id);
            ctx.db(|d| d.add_event(&e, Disposition::Inbox, &format!("could not classify: {err:#}")).map(|_| ()))?;
        }
    }
    ctx.db(|d| d.set_json(&ckey, &cursor))?;
    let min = cursor.poll_interval_secs.unwrap_or(0);
    Ok(min)
}

/// Classify one event: pending (trusted), inbox (unknown author) or dropped.
pub async fn intake(ctx: &Ctx, cfg: &Config, me: &str, e: Event) -> Result<()> {
    let s = cfg.settings_for(&ctx.paths, &e.repo)?;
    let own = e.author.eq_ignore_ascii_case(me);
    let drop = |reason: &str| ctx.db(|d| d.add_event(&e, Disposition::Dropped, reason).map(|_| ()));
    if !s.subscribed {
        return drop("not subscribed");
    }
    // Night Porter's own replies are posted as you; never treat them as new requests.
    if own && e.body.contains(crate::prompts::FOOTER_MARK) {
        return drop("night porter reply");
    }
    match e.mode() {
        Mode::MyPrs => {
            if !s.triggers.contains(&e.trigger) {
                return drop("trigger disabled");
            }
            let state_event = matches!(e.trigger, Trigger::CiFailure | Trigger::MergeConflict);
            if own && !state_event {
                return drop("own comment");
            }
        }
        Mode::Maintainer => {
            if !s.maintainer.enabled {
                return drop("maintainer mode off");
            }
        }
    }
    // Team membership needs the network; resolve only the teams this repo's rules mention.
    let mut teams = HashSet::new();
    if !own && !(s.trust.allow_teams.is_empty() && s.trust.deny_teams.is_empty()) {
        let forge = ctx.forge(&e.repo)?.lock().await;
        for t in s.trust.allow_teams.iter().chain(&s.trust.deny_teams) {
            match forge.in_team(t, &e.author).await {
                Ok(true) => {
                    teams.insert(t.clone());
                }
                Ok(false) => {}
                Err(err) => tracing::warn!("team {t}: {err:#}"),
            }
        }
    }
    let decision =
        if own { trust::Decision::Trusted("self") } else { trust::decide(&s.trust, &e.author, e.role, e.is_bot, |t| teams.contains(t)) };
    let (disp, reason) = match decision {
        trust::Decision::Trusted(why) => (Disposition::Pending, why.to_string()),
        trust::Decision::Untrusted => (Disposition::Inbox, "unknown author".into()),
        trust::Decision::Denied => (Disposition::Dropped, "denied".into()),
    };
    let new = ctx.db(|d| d.add_event(&e, disp, &reason))?;
    if new && disp == Disposition::Inbox {
        ctx.notify(
            Kind::NewAuthor,
            &format!("@{} on {}#{}", e.author, e.repo, e.number),
            "New author waiting in the inbox: allow or ignore?",
        );
    }
    Ok(())
}

/// Batch pending events into jobs: debounce, schedule, one job per PR, caps.
pub fn dispatch(ctx: &Ctx) -> Result<()> {
    if ctx.db(|d| Ok(crate::actions::paused(d)))? {
        return Ok(());
    }
    let cfg = ctx.cfg();
    let pending = ctx.db(|d| d.events_with(Disposition::Pending))?;
    let mut groups: BTreeMap<(RepoId, u64), Vec<(Event, i64)>> = BTreeMap::new();
    for (e, seen) in pending {
        groups.entry((e.repo.clone(), e.number)).or_default().push((e, seen));
    }
    let now = crate::state::now();
    let tz_now = jiff::Zoned::now();
    for ((repo, n), evs) in groups {
        let s = match cfg.settings_for(&ctx.paths, &repo) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("{repo}: {e:#}");
                continue;
            }
        };
        if !schedule::is_active(&s.schedule, &tz_now).unwrap_or(true) {
            continue;
        }
        let last = evs.iter().map(|(_, t)| *t).max().unwrap_or(now);
        if now - last < s.debounce.as_secs() as i64 {
            continue;
        }
        if ctx.db(|d| d.pr_busy(&repo, n))? {
            continue;
        }
        if ctx.db(|d| d.jobs_since(now - 86400))? >= cfg.daily_job_cap {
            ctx.warn_every("cap", 3600, &format!("daily job cap ({}) reached; waiting", cfg.daily_job_cap));
            return Ok(());
        }
        let verb = evs.iter().find_map(|(e, _)| e.verb);
        let mode = if verb.is_some() { Mode::Maintainer } else { Mode::MyPrs };
        let ids: Vec<String> = evs.iter().map(|(e, _)| e.id.clone()).collect();
        // CI-fix loop guard.
        if evs.iter().any(|(e, _)| e.trigger == Trigger::CiFailure) {
            let tries = ctx.db(|d| d.ci_attempts(&repo, n))?;
            if tries >= s.max_ci_attempts {
                for id in &ids {
                    ctx.db(|d| d.set_disposition(id, Disposition::Dropped, "CI-fix attempts exhausted"))?;
                }
                ctx.notify(Kind::Failed, &format!("{repo}#{n}"), &format!("CI still failing after {tries} attempts; stopped retrying"));
                continue;
            }
            ctx.db(|d| d.set_ci_attempts(&repo, n, tries + 1))?;
        }
        let level: Level = match mode {
            Mode::MyPrs => s.level,
            Mode::Maintainer => s.maintainer.level,
        };
        let target = evs[0].0.target;
        let mode_s = crate::state::enum_str(&mode);
        let id = ctx.db(|d| d.create_job(&repo, target, n, &mode_s, level.as_str(), &ids))?;
        ctx.db(|d| d.set_meta(id, &JobMeta { verb, ..Default::default() }))?;
        tracing::info!("job {id}: {repo}#{n} ({} event(s), {})", ids.len(), level.as_str());
    }
    Ok(())
}

/// Start queued and approved jobs within concurrency limits.
fn start_jobs(ctx: &Arc<Ctx>) -> Result<()> {
    let cfg = ctx.cfg();
    let queued = ctx.db(|d| d.jobs_with(JobStatus::Queued))?;
    let approved = ctx.db(|d| d.jobs_with(JobStatus::Approved))?;
    for j in approved.into_iter().chain(queued) {
        let running = ctx.running.lock().unwrap().len();
        if running >= cfg.max_concurrent_jobs {
            break;
        }
        if ctx.running.lock().unwrap().contains_key(&j.id) {
            continue;
        }
        let s = match cfg.settings_for(&ctx.paths, &j.repo) {
            Ok(s) => s,
            Err(e) => {
                ctx.warn_every(&format!("settings:{}", j.repo.key()), 3600, &format!("job {} waits: {e:#}", j.id));
                continue;
            }
        };
        let same_repo = ctx.running.lock().unwrap().values().filter(|r| **r == j.repo).count();
        if j.status == JobStatus::Queued && same_repo >= s.concurrency() {
            continue;
        }
        if j.status == JobStatus::Queued {
            let free = crate::git::free_gb(&ctx.paths.worktrees_dir()).unwrap_or(u64::MAX);
            if free < cfg.min_free_gb {
                ctx.warn_every("disk", 3600, &format!("only {free} GB free (min {}); jobs wait", cfg.min_free_gb));
                continue;
            }
        }
        ctx.running.lock().unwrap().insert(j.id, j.repo.clone());
        let c = ctx.clone();
        tokio::spawn(async move {
            let id = j.id;
            let res = if j.status == JobStatus::Approved { job::deliver(&c, id).await } else { job::run(&c, id).await };
            if let Err(e) = res {
                tracing::error!("job {id}: {e:#}");
                let _ = c.db(|d| d.fail(id, &format!("{e:#}")));
                c.notify(Kind::Failed, &format!("job {id} failed"), &format!("{e:#}"));
            }
            c.running.lock().unwrap().remove(&id);
        });
    }
    Ok(())
}

/// A context for one-off CLI commands (no poll loop). Forge login failures are tolerated.
pub async fn offline_ctx(paths: &Paths, cfg: Config) -> Result<Ctx> {
    let forges = connect(&cfg).await.unwrap_or_else(|e| {
        tracing::warn!("offline: {e:#}");
        HashMap::new()
    });
    let clones = repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts);
    Ok(Ctx {
        paths: paths.clone(),
        cfg: RwLock::new(Arc::new(cfg)),
        db: Mutex::new(Db::open(&paths.db())?),
        forges,
        clones: RwLock::new(Arc::new(clones)),
        running: Mutex::new(HashMap::new()),
        warned: Mutex::new(HashMap::new()),
    })
}

#[cfg(test)]
pub fn test_ctx(paths: Paths, cfg: Config, clones: CloneMap) -> Ctx {
    Ctx {
        paths,
        cfg: RwLock::new(Arc::new(cfg)),
        db: Mutex::new(Db::memory().unwrap()),
        forges: HashMap::new(),
        clones: RwLock::new(Arc::new(clones)),
        running: Mutex::new(HashMap::new()),
        warned: Mutex::new(HashMap::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{Role, Target};

    fn setup(repo_toml: &str) -> (tempfile::TempDir, Ctx, RepoId) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        std::fs::create_dir_all(paths.repo_file(&repo).parent().unwrap()).unwrap();
        std::fs::write(paths.repo_file(&repo), repo_toml).unwrap();
        let cfg = Config::parse("[defaults]\ndebounce = \"0s\"").unwrap();
        let mut clones = CloneMap::new();
        clones.insert(repo.clone(), d.path().join("clone"));
        let ctx = test_ctx(paths, cfg, clones);
        (d, ctx, repo)
    }

    fn ev(repo: &RepoId, id: &str, author: &str, role: Role, trigger: Trigger) -> Event {
        Event {
            id: id.into(),
            repo: repo.clone(),
            target: Target::Pr,
            number: 7,
            trigger,
            author: author.into(),
            role,
            is_bot: false,
            body: "please fix".into(),
            url: String::new(),
            thread: None,
            path: None,
            line: None,
            created_at: String::new(),
            verb: None,
        }
    }

    #[tokio::test]
    async fn intake_classifies() {
        let (_d, ctx, repo) = setup("subscribed = true\ntriggers = [\"comment\", \"ci_failure\"]");
        let cfg = ctx.cfg();
        intake(&ctx, &cfg, "me", ev(&repo, "1", "alice", Role::Member, Trigger::Comment)).await.unwrap();
        intake(&ctx, &cfg, "me", ev(&repo, "2", "rando", Role::None, Trigger::Comment)).await.unwrap();
        intake(&ctx, &cfg, "me", ev(&repo, "3", "me", Role::Owner, Trigger::Comment)).await.unwrap();
        intake(&ctx, &cfg, "me", ev(&repo, "4", "me", Role::Owner, Trigger::CiFailure)).await.unwrap();
        intake(&ctx, &cfg, "me", ev(&repo, "5", "alice", Role::Member, Trigger::ReviewComment)).await.unwrap();
        let ids = |d: Disposition| ctx.db(|db| db.events_with(d)).unwrap().into_iter().map(|(e, _)| e.id).collect::<Vec<_>>();
        assert_eq!(ids(Disposition::Pending), vec!["1", "4"]);
        assert_eq!(ids(Disposition::Inbox), vec!["2"]);
        assert_eq!(ids(Disposition::Dropped), vec!["3", "5"]);
    }

    #[tokio::test]
    async fn unsubscribed_repos_are_dropped() {
        let (_d, ctx, repo) = setup("");
        let cfg = ctx.cfg();
        intake(&ctx, &cfg, "me", ev(&repo, "1", "alice", Role::Member, Trigger::Comment)).await.unwrap();
        assert!(ctx.db(|d| d.events_with(Disposition::Pending)).unwrap().is_empty());
    }

    #[tokio::test]
    async fn dispatch_batches_one_job_per_pr_and_caps_ci() {
        let (_d, ctx, repo) = setup("subscribed = true\nmax_ci_attempts = 1");
        let cfg = ctx.cfg();
        intake(&ctx, &cfg, "me", ev(&repo, "1", "alice", Role::Member, Trigger::Comment)).await.unwrap();
        intake(&ctx, &cfg, "me", ev(&repo, "2", "bob", Role::Owner, Trigger::Review)).await.unwrap();
        dispatch(&ctx).unwrap();
        let jobs = ctx.db(|d| d.jobs_with(JobStatus::Queued)).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(ctx.db(|d| d.job_events(jobs[0].id)).unwrap().len(), 2);
        // Busy PR: new events wait.
        intake(&ctx, &cfg, "me", ev(&repo, "3", "alice", Role::Member, Trigger::Comment)).await.unwrap();
        dispatch(&ctx).unwrap();
        assert_eq!(ctx.db(|d| d.jobs_with(JobStatus::Queued)).unwrap().len(), 1);
        // CI attempts: first allowed, second dropped.
        ctx.db(|d| d.set_status(jobs[0].id, JobStatus::Done)).unwrap();
        intake(&ctx, &cfg, "me", ev(&repo, "4", "me", Role::Owner, Trigger::CiFailure)).await.unwrap();
        dispatch(&ctx).unwrap();
        let j2 = ctx.db(|d| d.jobs_with(JobStatus::Queued)).unwrap();
        assert_eq!(j2.len(), 1);
        ctx.db(|d| d.set_status(j2[0].id, JobStatus::Done)).unwrap();
        intake(&ctx, &cfg, "me", ev(&repo, "5", "me", Role::Owner, Trigger::CiFailure)).await.unwrap();
        dispatch(&ctx).unwrap();
        assert!(ctx.db(|d| d.jobs_with(JobStatus::Queued)).unwrap().is_empty());
    }

    #[tokio::test]
    async fn paused_dispatches_nothing() {
        let (_d, ctx, repo) = setup("subscribed = true");
        let cfg = ctx.cfg();
        ctx.db(|d| d.set("paused", "1")).unwrap();
        intake(&ctx, &cfg, "me", ev(&repo, "1", "alice", Role::Member, Trigger::Comment)).await.unwrap();
        dispatch(&ctx).unwrap();
        assert!(ctx.db(|d| d.jobs_with(JobStatus::Queued)).unwrap().is_empty());
    }
}
