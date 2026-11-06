//! Forge-neutral types plus enum dispatch over GitHub and GitLab.

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
}

impl ForgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "github",
            ForgeKind::GitLab => "gitlab",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "github" => Some(Self::GitHub),
            "gitlab" => Some(Self::GitLab),
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

/// A forge client. Enum dispatch (two forges, no trait objects needed).
pub enum Forge {
    GitHub(github::GitHub),
    GitLab(gitlab::GitLab),
}

impl Forge {
    pub fn me(&self) -> &str {
        match self {
            Forge::GitHub(g) => &g.me,
            Forge::GitLab(g) => &g.me,
        }
    }
    pub async fn login(&mut self) -> Result<()> {
        match self {
            Forge::GitHub(g) => g.login().await,
            Forge::GitLab(g) => g.login().await,
        }
    }
    /// New events since the last poll. `cursor` is opaque, persisted by the caller.
    pub async fn poll(&mut self, cursor: &mut Cursor) -> Result<Vec<Event>> {
        if cursor.since.is_none() {
            // First run: start from now. Never replay history (old comments, CI already red).
            cursor.since = Some(now_iso());
            cursor.last_sweep = Some(crate::state::now());
            return Ok(vec![]);
        }
        match self {
            Forge::GitHub(g) => g.poll(cursor).await,
            Forge::GitLab(g) => g.poll(cursor).await,
        }
    }
    pub async fn pr(&self, repo: &RepoId, number: u64) -> Result<PrInfo> {
        match self {
            Forge::GitHub(g) => g.pr(repo, number).await,
            Forge::GitLab(g) => g.mr(repo, number).await,
        }
    }
    pub async fn issue(&self, repo: &RepoId, number: u64) -> Result<IssueInfo> {
        match self {
            Forge::GitHub(g) => g.issue(repo, number).await,
            Forge::GitLab(g) => g.issue(repo, number).await,
        }
    }
    /// Reply in the event's thread when it has one, else a top-level comment on the PR/issue.
    pub async fn reply(&self, repo: &RepoId, target: Target, number: u64, to: Option<&Event>, body: &str) -> Result<()> {
        match self {
            Forge::GitHub(g) => g.reply(repo, target, number, to, body).await,
            Forge::GitLab(g) => g.reply(repo, target, number, to, body).await,
        }
    }
    pub async fn resolve(&self, repo: &RepoId, number: u64, thread: &str) -> Result<()> {
        match self {
            Forge::GitHub(g) => g.resolve(repo, thread).await,
            Forge::GitLab(g) => g.resolve(repo, number, thread).await,
        }
    }
    /// Open a draft PR/MR (maintainer `fix`). Returns its URL.
    pub async fn open_draft(&self, repo: &RepoId, head: &str, base: &str, title: &str, body: &str) -> Result<String> {
        match self {
            Forge::GitHub(g) => g.open_draft(repo, head, base, title, body).await,
            Forge::GitLab(g) => g.open_draft(repo, head, base, title, body).await,
        }
    }
    pub async fn in_team(&self, team: &str, user: &str) -> Result<bool> {
        match self {
            Forge::GitHub(g) => g.in_team(team, user).await,
            Forge::GitLab(g) => g.in_group(team, user).await,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn roles() {
        assert_eq!(Role::from_github("MEMBER"), Role::Member);
        assert_eq!(Role::from_github("NONE"), Role::None);
        assert_eq!(Role::from_gitlab(40), Role::Maintainer);
        assert_eq!(Role::from_gitlab(30), Role::Collaborator);
        assert!(Role::Owner > Role::Contributor);
    }
}
