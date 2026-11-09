//! Forge-neutral types plus enum dispatch over GitHub, GitLab and Forgejo/Gitea.

pub mod forgejo;
mod ghlike;
pub mod github;
pub mod gitlab;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    GitHub,
    GitLab,
    /// Forgejo and Gitea (one API). `gitea` is accepted in config.
    #[serde(alias = "gitea")]
    Forgejo,
}

impl ForgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "github",
            ForgeKind::GitLab => "gitlab",
            ForgeKind::Forgejo => "forgejo",
        }
    }
    pub fn default_host(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "github.com",
            ForgeKind::GitLab => "gitlab.com",
            ForgeKind::Forgejo => "codeberg.org",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "github" => Some(Self::GitHub),
            "gitlab" => Some(Self::GitLab),
            "forgejo" | "gitea" => Some(Self::Forgejo),
            _ => None,
        }
    }
}

/// A repository on a forge. `path` is `owner/name` (GitLab may nest: `group/sub/name`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RepoId {
    pub forge: ForgeKind,
    pub host: String,
    pub path: String,
}

impl RepoId {
    pub fn new(forge: ForgeKind, host: &str, path: &str) -> Self {
        Self { forge, host: host.to_ascii_lowercase(), path: path.trim_matches('/').trim_end_matches(".git").to_string() }
    }
    /// Stable key used in the database: `github:github.com/owner/name`.
    pub fn key(&self) -> String {
        format!("{}:{}/{}", self.forge.as_str(), self.host, self.path)
    }
    pub fn parse_key(s: &str) -> Option<Self> {
        let (forge, rest) = s.split_once(':')?;
        let (host, path) = rest.split_once('/')?;
        Some(Self::new(ForgeKind::parse(forge)?, host, path))
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.path)
    }
}

/// Normalised author role. Ordered from least to most privileged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    None,
    Contributor,
    Collaborator,
    Member,
    Maintainer,
    Owner,
}

impl Role {
    /// GitHub `author_association`.
    pub fn from_github(assoc: &str) -> Self {
        match assoc {
            "OWNER" => Role::Owner,
            "MEMBER" => Role::Member,
            "COLLABORATOR" => Role::Collaborator,
            "CONTRIBUTOR" | "FIRST_TIME_CONTRIBUTOR" | "FIRST_TIMER" => Role::Contributor,
            _ => Role::None,
        }
    }
    /// GitLab access level (10 guest, 20 reporter, 30 developer, 40 maintainer, 50 owner).
    pub fn from_gitlab(level: u32) -> Self {
        match level {
            50.. => Role::Owner,
            40..=49 => Role::Maintainer,
            30..=39 => Role::Collaborator,
            1..=29 => Role::Contributor,
            0 => Role::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    ReviewComment,
    Review,
    Comment,
    CiFailure,
    MergeConflict,
}

/// What a trusted maintainer asked for in maintainer mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verb {
    Triage,
    Reproduce,
    Fix,
    Review,
}

impl Verb {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "triage" => Some(Self::Triage),
            "reproduce" => Some(Self::Reproduce),
            "fix" => Some(Self::Fix),
            "review" => Some(Self::Review),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Activity on a PR/MR the user authored.
    MyPrs,
    /// A trusted user asked Night Porter to work on someone else's issue/MR.
    Maintainer,
}

/// Kind of thing the event targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Pr,
    Issue,
}

/// One piece of activity, after polling. Bodies are untrusted until `trust::decide` says otherwise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Forge-unique id used for de-duplication, e.g. `gh-review-comment-123`.
    pub id: String,
    pub repo: RepoId,
    pub target: Target,
    pub number: u64,
    pub trigger: Trigger,
    pub author: String,
    pub role: Role,
    pub is_bot: bool,
    pub body: String,
    pub url: String,
    /// Review thread / discussion id to reply to, when there is one.
    pub thread: Option<String>,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub created_at: String,
    /// Set when the event is a maintainer-mode request (label or `/nightporter <verb>`).
    pub verb: Option<Verb>,
}

impl Event {
    /// An event with no author, text or place yet: set what applies.
    pub fn new(id: String, repo: &RepoId, target: Target, number: u64, trigger: Trigger) -> Self {
        Event {
            id,
            repo: repo.clone(),
            target,
            number,
            trigger,
            author: String::new(),
            role: Role::None,
            is_bot: false,
            body: String::new(),
            url: String::new(),
            thread: None,
            path: None,
            line: None,
            created_at: now_iso(),
            verb: None,
        }
    }

    /// A conflict or CI failure the sweep found on your own PR. `id` carries the head SHA, so
    /// each state fires once per push.
    pub fn state(id: String, repo: &RepoId, number: u64, trigger: Trigger, me: &str, body: String) -> Self {
        Event { author: me.into(), role: Role::Owner, body, ..Self::new(id, repo, Target::Pr, number, trigger) }
    }

    pub fn mode(&self) -> Mode {
        if self.verb.is_some() { Mode::Maintainer } else { Mode::MyPrs }
    }
}

/// Current time as `YYYY-MM-DDTHH:MM:SSZ` (what both forge APIs accept in queries).
pub fn now_iso() -> String {
    jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// How far back each poll re-reads, to cover late indexing and clock drift.
pub const OVERLAP_SECS: i64 = 300;

/// `since` moved back by [`OVERLAP_SECS`].
pub fn overlap(since: &str) -> String {
    match since.parse::<jiff::Timestamp>() {
        Ok(t) => (t - jiff::SignedDuration::from_secs(OVERLAP_SECS)).strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
        Err(_) => since.to_string(),
    }
}

/// `ts` is strictly after `since`. Parses timestamps (forges mix `Z`, offsets and fractions);
/// falls back to string comparison if either doesn't parse.
pub fn newer(ts: &str, since: Option<&str>) -> bool {
    let Some(since) = since else { return true };
    match (ts.parse::<jiff::Timestamp>(), since.parse::<jiff::Timestamp>()) {
        (Ok(a), Ok(b)) => a > b,
        _ => ts > since,
    }
}

/// Parse a `/nightporter <verb>` command from the first line that starts with it.
pub fn parse_command(body: &str) -> Option<Verb> {
    body.lines().find_map(|l| {
        let mut it = l.trim().strip_prefix("/nightporter")?.split_whitespace();
        Verb::parse(it.next()?)
    })
}

/// A job command you post on the PR/MR yourself (e.g. from the forge's mobile app). Acts on Night
/// Porter's newest job for that PR, never on the PR itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Approve,
    Dismiss,
    Retry,
    Status,
}

/// Parse `/nightporter approve|dismiss|retry|status` from the first line that starts with it.
pub fn parse_control(body: &str) -> Option<Control> {
    body.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        if it.next()? != "/nightporter" {
            return None;
        }
        match it.next()? {
            "approve" => Some(Control::Approve),
            "dismiss" => Some(Control::Dismiss),
            "retry" => Some(Control::Retry),
            "status" => Some(Control::Status),
            _ => None,
        }
    })
}

/// A [`Control`] comment written by the token's own user (checked by user id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlComment {
    /// The forge's comment/note id (for the reaction and the processed-once record).
    pub id: String,
    pub control: Control,
}

/// The command in a comment, if the token's own user posted it after `since`.
/// `me_id == 0` (unknown) never matches. Night Porter's own posts never count, nor do edited
/// comments: people with write access can edit yours, so the text may not be your own.
pub fn control_of(
    me_id: u64,
    author_id: u64,
    via_app: bool,
    body: &str,
    created_at: &str,
    updated_at: &str,
    since: &str,
) -> Option<Control> {
    if me_id == 0
        || author_id != me_id
        || via_app
        || body.contains(crate::prompts::FOOTER_MARK)
        || !newer(created_at, Some(since))
        || edited(created_at, updated_at)
    {
        return None;
    }
    parse_control(body)
}

/// Changed more than a second after it was posted (an unknown time counts as edited).
fn edited(created_at: &str, updated_at: &str) -> bool {
    if updated_at.is_empty() || updated_at == created_at {
        return false;
    }
    match (created_at.parse::<jiff::Timestamp>(), updated_at.parse::<jiff::Timestamp>()) {
        (Ok(c), Ok(u)) => u.duration_since(c) > jiff::SignedDuration::from_secs(1),
        _ => true,
    }
}

/// What we need to check out and push a PR/MR.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub state: PrState,
    pub head_ref: String,
    pub head_sha: String,
    /// Clone URLs of the repo holding the head branch (may be a fork).
    pub head_repo_ssh: String,
    pub head_repo_https: String,
    pub base_ref: String,
    pub draft: bool,
    pub body: String,
}

/// An issue's text, for maintainer mode. Always untrusted.
#[derive(Clone, Debug, Default)]
pub struct IssueInfo {
    pub title: String,
    pub body: String,
    pub author: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    #[default]
    Open,
    Closed,
    Merged,
}

/// One reply the agent wants posted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    /// Event id this answers (thread replies), or `None` for a top-level comment.
    #[serde(default)]
    pub event: Option<String>,
    pub body: String,
}

/// One open issue or PR/MR, as listed for `nightporter sweep`. Text is untrusted.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub created_at: String,
    pub labels: Vec<String>,
}

/// Which listed items a sweep takes: all set conditions must hold.
#[derive(Clone, Debug, Default)]
pub struct Pick {
    /// Every one of these labels (case-insensitive).
    pub labels: Vec<String>,
    pub created_before: Option<jiff::Timestamp>,
    pub created_after: Option<jiff::Timestamp>,
    /// Leave out this author's items (your own PRs are already watched).
    pub skip_author: Option<String>,
    pub limit: Option<usize>,
}

/// Oldest first, so a capped sweep works through the backlog in order. Done in one place (not
/// with each forge's query parameters, which differ in label and date semantics).
pub fn pick(mut items: Vec<Listed>, p: &Pick) -> Vec<Listed> {
    let at = |i: &Listed| i.created_at.parse::<jiff::Timestamp>().ok();
    items.retain(|i| {
        p.labels.iter().all(|l| i.labels.iter().any(|x| x.eq_ignore_ascii_case(l)))
            && p.skip_author.as_ref().is_none_or(|a| !i.author.eq_ignore_ascii_case(a))
            && p.created_before.is_none_or(|t| at(i).is_some_and(|c| c < t))
            && p.created_after.is_none_or(|t| at(i).is_some_and(|c| c > t))
    });
    items.sort_by(|a, b| at(a).cmp(&at(b)).then(a.number.cmp(&b.number)));
    items.truncate(p.limit.unwrap_or(usize::MAX));
    items
}

/// Pages fetched when listing open issues or PRs (100 per page; 50 on Forgejo/Gitea).
pub const LIST_PAGES: usize = 50;

/// A forge client. Enum dispatch (three forges, no trait objects needed).
pub enum Forge {
    GitHub(github::GitHub),
    GitLab(gitlab::GitLab),
    Forgejo(forgejo::Forgejo),
}

/// The same expression on whichever forge this is; every forge has the same method names.
macro_rules! each {
    ($s:expr, $g:ident => $e:expr) => {
        match $s {
            Forge::GitHub($g) => $e,
            Forge::GitLab($g) => $e,
            Forge::Forgejo($g) => $e,
        }
    };
}

impl Forge {
    pub fn new(kind: ForgeKind, host: &str, api: crate::http::Api) -> Self {
        match kind {
            ForgeKind::GitHub => Forge::GitHub(github::GitHub::new(host, api)),
            ForgeKind::GitLab => Forge::GitLab(gitlab::GitLab::new(host, api)),
            ForgeKind::Forgejo => Forge::Forgejo(forgejo::Forgejo::new(host, api)),
        }
    }

    pub fn api_base(kind: ForgeKind, host: &str) -> String {
        match kind {
            ForgeKind::GitHub => github::GitHub::api_base(host),
            ForgeKind::GitLab => gitlab::GitLab::api_base(host),
            ForgeKind::Forgejo => forgejo::Forgejo::api_base(host),
        }
    }

    /// What to watch: subscribed repo paths, the maintainer label per repo with maintainer mode
    /// on, and how often to sweep your open PRs.
    pub fn watch(
        &mut self,
        subscribed: std::collections::HashSet<String>,
        labels: std::collections::HashMap<String, String>,
        sweep_every: std::time::Duration,
    ) {
        each!(self, g => {
            g.subscribed = subscribed;
            g.labels = labels;
            g.sweep_every = sweep_every;
        });
    }

    /// Comments on `number` posted after `since` by the token's own user that are job commands,
    /// oldest first.
    pub async fn controls(&self, repo: &RepoId, target: Target, number: u64, since: &str) -> Result<Vec<ControlComment>> {
        each!(self, g => g.controls(repo, target, number, since).await)
    }

    /// Acknowledge a command comment with a thumbs-up.
    pub async fn ack(&self, repo: &RepoId, target: Target, number: u64, comment: &str) -> Result<()> {
        each!(self, g => g.ack(repo, target, number, comment).await)
    }

    pub fn me(&self) -> &str {
        each!(self, g => &g.me)
    }

    pub async fn login(&mut self) -> Result<()> {
        each!(self, g => g.login().await)
    }

    /// New events since the last poll. `cursor` is opaque, persisted by the caller.
    pub async fn poll(&mut self, cursor: &mut Cursor) -> Result<Vec<Event>> {
        if cursor.since.is_none() {
            // First run: start from now. Never replay history (old comments, CI already red).
            cursor.since = Some(now_iso());
            cursor.last_sweep = Some(crate::state::now());
            return Ok(vec![]);
        }
        each!(self, g => g.poll(cursor).await)
    }

    pub async fn pr(&self, repo: &RepoId, number: u64) -> Result<PrInfo> {
        each!(self, g => g.pr(repo, number).await)
    }

    /// Every open issue (`Target::Issue`) or PR/MR (`Target::Pr`) in `repo`.
    pub async fn open_items(&self, repo: &RepoId, target: Target) -> Result<Vec<Listed>> {
        each!(self, g => g.open_items(repo, target).await)
    }

    pub async fn issue(&self, repo: &RepoId, number: u64) -> Result<IssueInfo> {
        each!(self, g => g.issue(repo, number).await)
    }

    /// Reply in the event's thread when it has one, else a top-level comment on the PR/issue.
    pub async fn reply(&self, repo: &RepoId, target: Target, number: u64, to: Option<&Event>, body: &str) -> Result<()> {
        each!(self, g => g.reply(repo, target, number, to, body).await)
    }

    pub async fn resolve(&self, repo: &RepoId, number: u64, thread: &str) -> Result<()> {
        each!(self, g => g.resolve(repo, number, thread).await)
    }

    /// Open a draft PR/MR (maintainer `fix`). Returns its URL.
    pub async fn open_draft(&self, repo: &RepoId, head: &str, base: &str, title: &str, body: &str) -> Result<String> {
        each!(self, g => g.open_draft(repo, head, base, title, body).await)
    }

    /// Team (`org/team`) or GitLab group membership, cached for an hour.
    pub async fn in_team(&self, team: &str, user: &str) -> Result<bool> {
        each!(self, g => g.in_team(team, user).await)
    }
}

/// A small map whose entries expire after an hour (role and team lookups).
pub struct Ttl<K, V>(std::sync::Mutex<std::collections::HashMap<K, (i64, V)>>);

impl<K: std::hash::Hash + Eq, V: Clone> Default for Ttl<K, V> {
    fn default() -> Self {
        Self(std::sync::Mutex::default())
    }
}

impl<K: std::hash::Hash + Eq, V: Clone> Ttl<K, V> {
    pub fn get(&self, k: &K) -> Option<V> {
        let m = self.0.lock().unwrap();
        m.get(k).filter(|(at, _)| crate::state::now() - at < 3600).map(|(_, v)| v.clone())
    }
    pub fn put(&self, k: K, v: V) {
        self.0.lock().unwrap().insert(k, (crate::state::now(), v));
    }
}

impl Ttl<String, Vec<String>> {
    /// Whether `user` is in `team`, fetching (and caching) the member list on a miss. `fetch`
    /// must read every page: a member past the first must still match, or `deny_teams` fails open.
    pub async fn member(&self, team: &str, user: &str, fetch: impl Future<Output = Result<Vec<String>>>) -> Result<bool> {
        let members = if let Some(m) = self.get(&team.to_string()) {
            m
        } else {
            let m = fetch.await?;
            self.put(team.into(), m.clone());
            m
        };
        Ok(members.iter().any(|m| m.eq_ignore_ascii_case(user)))
    }
}

/// Opaque per-account poll cursor, stored as JSON in the state db.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cursor {
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
    /// Last time the slow "my open PRs" sweep ran (unix secs).
    #[serde(default)]
    pub last_sweep: Option<i64>,
    /// Mergeable / CI status seen last sweep, per `repo#n`, so we only fire on change.
    #[serde(default)]
    pub seen_status: std::collections::BTreeMap<String, String>,
    /// A first (record-only) sweep has happened.
    #[serde(default)]
    pub swept: bool,
}

impl Cursor {
    /// Time for the slow sweep of your open PRs; marks it as run.
    pub fn sweep_due(&mut self, every: std::time::Duration) -> bool {
        let now = crate::state::now();
        let due = self.last_sweep.is_none_or(|t| now - t >= i64::try_from(every.as_secs()).unwrap_or(i64::MAX));
        if due {
            self.last_sweep = Some(now);
        }
        due
    }

    /// One PR's sweep result: its events go out, its status is remembered under `key`. The first
    /// sweep after install only records, so PRs that were already red or conflicted don't all
    /// start jobs at once (set `swept` once a sweep has read the PR list).
    pub fn record(&mut self, key: String, r: Result<(Vec<Event>, String)>, out: &mut Vec<Event>) {
        match r {
            Ok((evs, status)) => {
                if self.swept {
                    out.extend(evs);
                }
                self.seen_status.insert(key, status);
            }
            Err(e) => tracing::warn!("sweep {key}: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_bookkeeping_records_first_then_fires() {
        let mut c = Cursor::default();
        let every = std::time::Duration::from_secs(600);
        assert!(c.sweep_due(every));
        assert!(!c.sweep_due(every), "just ran");
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let red = || Ok((vec![Event::state("x".into(), &repo, 1, Trigger::CiFailure, "me", "CI failed".into())], "red".into()));
        let mut out = vec![];
        // First sweep: state only, no events (the PR may have been red for days).
        c.record("k".into(), red(), &mut out);
        assert_eq!((out.len(), c.seen_status["k"].as_str()), (0, "red"));
        c.swept = true;
        c.record("k".into(), red(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].role, out[0].author.as_str(), out[0].target), (Role::Owner, "me", Target::Pr));
        // A failed read keeps the last status.
        c.record("k".into(), Err(anyhow::anyhow!("boom")), &mut out);
        assert_eq!(c.seen_status["k"], "red");
    }

    #[test]
    fn repo_key_roundtrip() {
        let r = RepoId::new(ForgeKind::GitLab, "Git.DrupalCode.org", "project/token.git");
        assert_eq!(r.key(), "gitlab:git.drupalcode.org/project/token");
        assert_eq!(RepoId::parse_key(&r.key()), Some(r));
    }

    #[test]
    fn timestamps() {
        assert!(newer("2026-11-05T10:00:01Z", Some("2026-11-05T10:00:00Z")));
        assert!(!newer("2026-11-05T10:00:00Z", Some("2026-11-05T10:00:00Z")));
        assert!(newer("2026-11-05T11:30:00.000+01:00", Some("2026-11-05T10:00:00Z")));
        assert!(!newer("2026-11-05T10:30:00.000+01:00", Some("2026-11-05T10:00:00Z")));
        assert!(newer("x", None));
        assert_eq!(overlap("2026-11-05T10:05:00Z"), "2026-11-05T10:00:00Z");
    }

    #[test]
    fn commands() {
        assert_eq!(parse_command("thanks!\n/nightporter fix please"), Some(Verb::Fix));
        assert_eq!(parse_command("  /nightporter triage"), Some(Verb::Triage));
        assert_eq!(parse_command("/nightporter merge"), None);
        assert_eq!(parse_command("see /nightporter fix"), None);
    }

    #[test]
    fn controls() {
        assert_eq!(parse_control("/nightporter approve"), Some(Control::Approve));
        assert_eq!(parse_control("lgtm\n  /nightporter retry now"), Some(Control::Retry));
        assert_eq!(parse_control("/nightporter dismiss"), Some(Control::Dismiss));
        assert_eq!(parse_control("/nightporter status"), Some(Control::Status));
        assert_eq!(parse_control("/nightporter merge"), None);
        assert_eq!(parse_control("/nightporter fix"), None);
        assert_eq!(parse_control("please /nightporter approve"), None);
        assert_eq!(parse_control("/nightporterapprove"), None);
        let at = "2026-11-05T10:00:01Z";
        let since = "2026-11-05T10:00:00Z";
        let ok = |me, author, app, body: &str, updated| control_of(me, author, app, body, at, updated, since);
        assert_eq!(ok(7, 7, false, "/nightporter approve", at), Some(Control::Approve));
        assert_eq!(ok(7, 7, false, "/nightporter approve", ""), Some(Control::Approve), "no updated_at");
        assert_eq!(ok(7, 7, false, "/nightporter approve", "2026-11-05T10:00:01.500Z"), Some(Control::Approve), "same second");
        assert_eq!(ok(7, 7, false, "/nightporter approve", "2026-11-05T10:05:00Z"), None, "edited later, maybe by someone else");
        assert_eq!(ok(7, 8, false, "/nightporter approve", at), None, "someone else");
        assert_eq!(ok(0, 0, false, "/nightporter approve", at), None, "unknown id");
        assert_eq!(ok(7, 7, true, "/nightporter approve", at), None, "an app acting as me");
        assert_eq!(control_of(7, 7, false, "/nightporter approve", since, since, since), None, "older than the job");
        let ours = format!("/nightporter approve\n{}", crate::prompts::FOOTER_MARK);
        assert_eq!(ok(7, 7, false, &ours, at), None, "our own post");
    }

    #[test]
    fn roles() {
        assert_eq!(Role::from_github("MEMBER"), Role::Member);
        assert_eq!(Role::from_github("NONE"), Role::None);
        assert_eq!(Role::from_gitlab(40), Role::Maintainer);
        assert_eq!(Role::from_gitlab(30), Role::Collaborator);
        assert!(Role::Owner > Role::Contributor);
    }

    #[test]
    fn picks_by_label_age_and_author() {
        let it = |n, at: &str, who: &str, labels: &[&str]| Listed {
            number: n,
            title: format!("#{n}"),
            author: who.into(),
            created_at: at.into(),
            labels: labels.iter().map(ToString::to_string).collect(),
        };
        let items = vec![
            it(3, "2026-10-01T00:00:00Z", "ann", &["Bug"]),
            it(1, "2026-01-01T00:00:00Z", "bob", &["bug", "needs review"]),
            it(2, "2026-06-01T00:00:00Z", "me", &[]),
            it(4, "not a date", "ann", &["bug"]),
        ];
        let nums = |p: &Pick| pick(items.clone(), p).iter().map(|i| i.number).collect::<Vec<_>>();
        assert_eq!(nums(&Pick::default()), [4, 1, 2, 3]); // undated first, then oldest
        assert_eq!(nums(&Pick { labels: vec!["BUG".into()], ..Default::default() }), [4, 1, 3]);
        assert_eq!(nums(&Pick { labels: vec!["bug".into(), "needs review".into()], ..Default::default() }), [1]);
        let t = |s: &str| Some(s.parse().unwrap());
        assert_eq!(nums(&Pick { created_before: t("2026-07-01T00:00:00Z"), ..Default::default() }), [1, 2]);
        assert_eq!(nums(&Pick { created_after: t("2026-05-01T00:00:00Z"), ..Default::default() }), [2, 3]);
        assert_eq!(nums(&Pick { skip_author: Some("ME".into()), limit: Some(2), ..Default::default() }), [4, 1]);
    }

    #[test]
    fn forgejo_kind() {
        assert_eq!(ForgeKind::parse("gitea"), Some(ForgeKind::Forgejo));
        assert_eq!(toml::from_str::<toml::Table>("f='gitea'").unwrap()["f"].clone().try_into::<ForgeKind>().unwrap(), ForgeKind::Forgejo);
        let r = RepoId::new(ForgeKind::Forgejo, "Codeberg.org", "o/r");
        assert_eq!(RepoId::parse_key(&r.key()), Some(r));
    }
}
