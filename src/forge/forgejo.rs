//! Forgejo and Gitea (codeberg.org, gitea.com, self-hosted): API v1. Same shape as GitHub:
//! notifications for comments and reviews, a slow sweep of my open PRs for conflicts and CI.
//!
//! Differences from GitHub, all forced by the API:
//! - Comments carry no author association, so every role comes from the collaborator
//!   permission endpoint (with a fallback when we're not a repo admin, see [`Forgejo::role_of`]).
//! - There is no "reply to review comment" endpoint. A reply is a one-comment review on the same
//!   file and line, which the web UI shows in the same conversation.
//! - Resolving a conversation has an API on Gitea (1.25+) but not on Forgejo; there it fails
//!   with a clear message in the job log.
//! - CI is commit statuses (Forgejo/Gitea Actions, Woodpecker and others all report there).

use super::{Cursor, Event, PrInfo, PrState, RepoId, Role, Target, Trigger, Verb, newer, now_iso, parse_command};
use crate::forge::ForgeKind;
use crate::http::Api;
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// Access token scopes Night Porter needs (Settings → Applications → Generate new token).
pub const SCOPES: &str = "read:user, read:notification, write:issue, write:repository, read:organization";

pub struct Forgejo {
    pub host: String,
    pub me: String,
    pub api: Api,
    /// Maintainer label per subscribed repo (`owner/name`) with maintainer mode on.
    pub labels: HashMap<String, String>,
    pub subscribed: HashSet<String>,
    pub sweep_every: std::time::Duration,
    team_cache: super::Ttl<String, Vec<String>>,
    perm_cache: super::Ttl<String, Role>,
}

#[derive(Deserialize)]
struct User {
    login: String,
}
impl User {
    /// No bot flag in the API: go by name (Actions' own user, `renovate-bot`, `x[bot]`, `ci_bot`).
    fn is_bot(&self) -> bool {
        let l = self.login.to_ascii_lowercase();
        l.ends_with("[bot]") || l.ends_with("-bot") || l.ends_with("_bot") || l == "gitea-actions" || l == "forgejo-actions"
    }
}

#[derive(Deserialize)]
struct Thread {
    subject: Subject,
    repository: Repo,
}
#[derive(Deserialize)]
struct Subject {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    url: String,
}
#[derive(Deserialize)]
struct Repo {
    full_name: String,
}

#[derive(Deserialize)]
struct Comment {
    id: u64,
    user: User,
    #[serde(default)]
    body: String,
    #[serde(default)]
    html_url: String,
    created_at: String,
}

#[derive(Deserialize)]
struct Review {
    id: u64,
    user: Option<User>,
    #[serde(default)]
    body: String,
    state: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    submitted_at: Option<String>,
    #[serde(default)]
    comments_count: u64,
}

#[derive(Deserialize)]
struct ReviewComment {
    id: u64,
    user: User,
    #[serde(default)]
    body: String,
    #[serde(default)]
    html_url: String,
    created_at: String,
    path: String,
    /// New-side line, 0 when the comment is on a removed line (then `original_position` is set).
    #[serde(default)]
    position: u64,
    #[serde(default)]
    original_position: u64,
}

#[derive(Deserialize)]
struct TimelineItem {
    id: u64,
    #[serde(rename = "type")]
    kind: String,
    user: Option<User>,
    label: Option<Label>,
    #[serde(default)]
    body: String,
    created_at: String,
}
#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Deserialize)]
struct Pull {
    number: u64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    user: User,
    state: String,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    mergeable: bool,
    head: Head,
    base: Head,
}
#[derive(Deserialize)]
struct Head {
    #[serde(rename = "ref")]
    name: String,
    #[serde(default)]
    sha: String,
    repo: Option<HeadRepo>,
}
#[derive(Deserialize)]
struct HeadRepo {
    ssh_url: String,
    clone_url: String,
}

/// Collaborator permission → role. `admin` is a repo admin, `owner` the repo/org owner.
fn role_from(permission: &str) -> Role {
    match permission {
        "owner" => Role::Owner,
        "admin" => Role::Maintainer,
        "write" => Role::Collaborator,
        "read" => Role::Contributor,
        _ => Role::None,
    }
}

impl Forgejo {
    pub fn new(host: &str, api: Api) -> Self {
        Self {
            host: host.into(),
            me: String::new(),
            api,
            labels: HashMap::new(),
            subscribed: HashSet::new(),
            sweep_every: std::time::Duration::from_secs(600),
            team_cache: super::Ttl::default(),
            perm_cache: super::Ttl::default(),
        }
    }

    pub fn api_base(host: &str) -> String {
        format!("https://{host}/api/v1")
    }

    fn repo(&self, full: &str) -> RepoId {
        RepoId::new(ForgeKind::Forgejo, &self.host, full)
    }

    pub async fn login(&mut self) -> Result<()> {
        let u: User = self.api.get("/user").await.context("Forgejo login")?;
        self.me = u.login;
        Ok(())
    }

    pub async fn poll(&mut self, cur: &mut Cursor) -> Result<Vec<Event>> {
        let since = cur.since.as_deref().map(super::overlap);
        let started = now_iso();
        let mut path = "/notifications?all=true&limit=50".to_string();
        if let Some(s) = &since {
            let _ = write!(path, "&since={s}");
        }
        let mut out = Vec::new();
        let threads: Vec<Thread> = self.api.get_all(&path, 10).await?;
        let mut failed = false;
        for t in threads.iter().filter(|t| self.subscribed.contains(&t.repository.full_name)) {
            if let Err(e) = self.thread_events(t, since.as_deref(), &mut out).await {
                tracing::warn!("{}: {e:#}", t.repository.full_name);
                failed = true;
            }
        }
        if !failed {
            cur.since = Some(started);
        }
        let due = cur.last_sweep.is_none_or(|t| crate::state::now() - t >= i64::try_from(self.sweep_every.as_secs()).unwrap_or(i64::MAX));
        if due {
            cur.last_sweep = Some(crate::state::now());
            match self.sweep(cur).await {
                Ok(evs) => out.extend(evs),
                Err(e) => tracing::warn!("sweep: {e:#}"),
            }
        }
        Ok(out)
    }

    async fn thread_events(&self, t: &Thread, since: Option<&str>, out: &mut Vec<Event>) -> Result<()> {
        let number: u64 = t.subject.url.rsplit('/').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if number == 0 {
            return Ok(());
        }
        let r = &t.repository.full_name;
        let repo = self.repo(r);
        let after = |ts: &str| newer(ts, since);
        let (target, mine) = match t.subject.kind.as_str() {
            "Pull" => {
                let p: Pull = self.api.get(&format!("/repos/{r}/pulls/{number}")).await?;
                (Target::Pr, p.user.login.eq_ignore_ascii_case(&self.me))
            }
            "Issue" => (Target::Issue, false),
            _ => return Ok(()),
        };
        let ev = |trigger, id: String, u: User, body: String, url: String, at: String, verb| Event {
            id,
            repo: repo.clone(),
            target,
            number,
            trigger,
            is_bot: u.is_bot(),
            author: u.login,
            role: Role::None,
            body,
            url,
            thread: None,
            path: None,
            line: None,
            created_at: at,
            verb,
        };
        let start = out.len();

        // Conversation comments (PRs and issues).
        let since_q = since.map(|s| format!("&since={s}")).unwrap_or_default();
        let comments: Vec<Comment> = self.api.get_all(&format!("/repos/{r}/issues/{number}/comments?limit=50{since_q}"), 10).await?;
        for c in comments.into_iter().filter(|c| after(&c.created_at)) {
            let verb = parse_command(&c.body);
            if !mine && verb.is_none() {
                continue;
            }
            out.push(ev(Trigger::Comment, format!("fj-{}-ic-{}", self.host, c.id), c.user, c.body, c.html_url, c.created_at, verb));
        }

        if target == Target::Pr && mine {
            let reviews: Vec<Review> = self.api.get_all(&format!("/repos/{r}/pulls/{number}/reviews?limit=50"), 20).await?;
            // A conversation is every line comment on the same file and line; Gitea resolves it
            // through its first comment.
            let mut first: HashMap<(String, u64, u64), u64> = HashMap::new();
            for c in self.review_comments(r, number, &reviews).await? {
                let root = *first.entry((c.path.clone(), c.position, c.original_position)).or_insert(c.id);
                if after(&c.created_at) {
                    let mut e =
                        ev(Trigger::ReviewComment, format!("fj-{}-rc-{}", self.host, c.id), c.user, c.body, c.html_url, c.created_at, None);
                    (e.thread, e.path, e.line) = (Some(root.to_string()), Some(c.path), (c.position > 0).then_some(c.position));
                    out.push(e);
                }
            }
            for rv in reviews {
                let Some(at) = rv.submitted_at.clone() else { continue };
                // Approvals need no action; empty COMMENT reviews are containers for line comments.
                if !after(&at)
                    || matches!(rv.state.as_str(), "APPROVED" | "PENDING" | "REQUEST_REVIEW")
                    || (rv.state == "COMMENT" && rv.body.trim().is_empty())
                {
                    continue;
                }
                let Some(user) = rv.user else { continue };
                let body =
                    if rv.body.trim().is_empty() { format!("(review: {})", rv.state.to_lowercase().replace('_', " ")) } else { rv.body };
                out.push(ev(Trigger::Review, format!("fj-{}-rv-{}", self.host, rv.id), user, body, rv.html_url, at, None));
            }
        }

        // Maintainer label requests (on issues or others' PRs). A label comment's body is "1"
        // when the label was added, empty when removed.
        if !mine && let Some(label) = self.labels.get(r) {
            let tl: Vec<TimelineItem> = self.api.get_all(&format!("/repos/{r}/issues/{number}/timeline?limit=50{since_q}"), 10).await?;
            for i in tl.into_iter().filter(|i| i.kind == "label" && i.body == "1" && after(&i.created_at)) {
                let (Some(l), Some(u)) = (i.label, i.user) else { continue };
                if l.name.eq_ignore_ascii_case(label) {
                    let body = format!("(label `{label}` added)");
                    out.push(ev(
                        Trigger::Comment,
                        format!("fj-{}-label-{}", self.host, i.id),
                        u,
                        body,
                        String::new(),
                        i.created_at,
                        Some(Verb::Fix),
                    ));
                }
            }
        }

        for e in out[start..].iter_mut().filter(|e| !e.is_bot && !e.author.eq_ignore_ascii_case(&self.me)) {
            e.role = self.role_of(r, &e.author).await;
        }
        Ok(())
    }

    /// Every line comment on the PR, oldest first. ponytail: one request per review with
    /// comments; fine for normal PRs, batch if a PR with hundreds of reviews ever shows up.
    async fn review_comments(&self, r: &str, number: u64, reviews: &[Review]) -> Result<Vec<ReviewComment>> {
        let mut rcs = Vec::new();
        for rv in reviews.iter().filter(|rv| rv.comments_count > 0) {
            let cs: Vec<ReviewComment> = self.api.get(&format!("/repos/{r}/pulls/{number}/reviews/{}/comments", rv.id)).await?;
            rcs.extend(cs);
        }
        rcs.sort_by_key(|c| c.id);
        Ok(rcs)
    }

    /// The user's role on the repo, cached for an hour. Only repo admins may read someone else's
    /// permission (403 otherwise); then fall back to what any reader can see: the repo owner's
    /// name, and whether the user is a collaborator. That fallback can't see org teams, so org
    /// members who aren't collaborators come out as `none` (fails closed).
    async fn role_of(&self, repo: &str, user: &str) -> Role {
        #[derive(Deserialize)]
        struct P {
            permission: String,
        }
        let key = format!("{repo}:{}", user.to_ascii_lowercase());
        if let Some(r) = self.perm_cache.get(&key) {
            return r;
        }
        let r = match self.api.get::<P>(&format!("/repos/{repo}/collaborators/{user}/permission")).await {
            Ok(p) => role_from(&p.permission),
            Err(e) if e.to_string().contains("HTTP 403") => {
                if repo.split('/').next().is_some_and(|o| o.eq_ignore_ascii_case(user)) {
                    Role::Owner
                } else {
                    match self.api.send(Method::GET, &format!("/repos/{repo}/collaborators/{user}"), &Value::Null).await {
                        Ok(_) => Role::Collaborator,
                        Err(e) if e.to_string().contains("HTTP 404") => Role::None,
                        // A hiccup: don't cache, or maintainers land in the inbox for an hour.
                        Err(_) => return Role::None,
                    }
                }
            }
            Err(e) if e.to_string().contains("HTTP 404") => Role::None,
            Err(_) => return Role::None,
        };
        self.perm_cache.put(key, r);
        r
    }

    /// Slow sweep of my open PRs: conflicts and failing CI, keyed by head SHA (see GitHub).
    async fn sweep(&self, cur: &mut Cursor) -> Result<Vec<Event>> {
        #[derive(Deserialize)]
        struct I {
            number: u64,
            repository: Repo,
        }
        let mine: Vec<I> = self.api.get_all("/repos/issues/search?type=pulls&state=open&created=true&limit=50", 10).await?;
        let mut out = Vec::new();
        for i in mine.iter().filter(|i| self.subscribed.contains(&i.repository.full_name)) {
            let repo = self.repo(&i.repository.full_name);
            let key = format!("{}#{}", repo.key(), i.number);
            let prev = cur.seen_status.get(&key).cloned().unwrap_or_default();
            match self.sweep_pr(&repo, i.number, &prev).await {
                Ok((evs, status)) => {
                    if cur.swept {
                        out.extend(evs);
                    }
                    cur.seen_status.insert(key, status);
                }
                Err(e) => tracing::warn!("sweep {repo}#{}: {e:#}", i.number),
            }
        }
        cur.swept = true;
        Ok(out)
    }

    /// `prev` is last sweep's status. `mergeable` is also false while the server is still
    /// checking a new push, so a conflict only fires when two sweeps in a row see it at the same
    /// head. ponytail: costs one sweep interval of delay on real conflicts.
    async fn sweep_pr(&self, repo: &RepoId, n: u64, prev: &str) -> Result<(Vec<Event>, String)> {
        #[derive(Deserialize)]
        struct Combined {
            #[serde(default)]
            statuses: Vec<CommitStatus>,
        }
        #[derive(Deserialize)]
        struct CommitStatus {
            #[serde(default)]
            context: String,
            #[serde(default)]
            status: String,
            #[serde(default)]
            target_url: String,
            #[serde(default)]
            description: String,
        }
        let r = &repo.path;
        let p: Pull = self.api.get(&format!("/repos/{r}/pulls/{n}")).await?;
        let sha = &p.head.sha;
        let mut out = Vec::new();
        let mut status = Vec::new();
        if !p.mergeable && !p.draft && p.state == "open" {
            let mark = format!("conflict@{sha}");
            if prev.split(',').any(|s| s == mark) {
                out.push(self.state_event(
                    repo,
                    n,
                    Trigger::MergeConflict,
                    sha,
                    format!("This PR has merge conflicts with `{}`.", p.base.name),
                ));
            }
            status.push(mark);
        }
        let c: Combined = self.api.get(&format!("/repos/{r}/commits/{sha}/status")).await?;
        let failed: Vec<&CommitStatus> = c.statuses.iter().filter(|s| matches!(s.status.as_str(), "failure" | "error")).collect();
        if failed.is_empty() {
            if !c.statuses.is_empty() && c.statuses.iter().all(|s| matches!(s.status.as_str(), "success" | "warning")) {
                status.push("green".into());
            }
        } else {
            status.push("red".into());
            let mut body = String::from("CI failed:\n");
            for f in failed {
                let _ = writeln!(body, "- {} ({}) {}", f.context, f.target_url, f.description.chars().take(800).collect::<String>());
            }
            out.push(self.state_event(repo, n, Trigger::CiFailure, sha, body));
        }
        Ok((out, status.join(",")))
    }

    fn state_event(&self, repo: &RepoId, n: u64, trigger: Trigger, sha: &str, body: String) -> Event {
        Event {
            id: format!("fj-{}-{}-{}#{n}-{sha}", self.host, crate::state::enum_str(&trigger), repo.path),
            repo: repo.clone(),
            target: Target::Pr,
            number: n,
            trigger,
            author: self.me.clone(),
            role: Role::Owner,
            is_bot: false,
            body,
            url: String::new(),
            thread: None,
            path: None,
            line: None,
            created_at: now_iso(),
            verb: None,
        }
    }

    pub async fn pr(&self, repo: &RepoId, n: u64) -> Result<PrInfo> {
        let p: Pull = self.api.get(&format!("/repos/{}/pulls/{n}", repo.path)).await?;
        let head_repo = p.head.repo.as_ref();
        Ok(PrInfo {
            number: p.number,
            title: p.title,
            author: p.user.login,
            state: match (p.state.as_str(), p.merged) {
                (_, true) => PrState::Merged,
                ("closed", _) => PrState::Closed,
                _ => PrState::Open,
            },
            head_ref: p.head.name,
            head_sha: p.head.sha,
            head_repo_ssh: head_repo.map(|r| r.ssh_url.clone()).unwrap_or_default(),
            head_repo_https: head_repo.map(|r| r.clone_url.clone()).unwrap_or_default(),
            base_ref: p.base.name,
            draft: p.draft,
            body: p.body.unwrap_or_default(),
        })
    }

    pub async fn issue(&self, repo: &RepoId, n: u64) -> Result<super::IssueInfo> {
        #[derive(Deserialize)]
        struct I {
            title: String,
            body: Option<String>,
            user: User,
        }
        let i: I = self.api.get(&format!("/repos/{}/issues/{n}", repo.path)).await?;
        Ok(super::IssueInfo { title: i.title, body: i.body.unwrap_or_default(), author: i.user.login })
    }

    pub async fn reply(&self, repo: &RepoId, n: u64, to: Option<&Event>, body: &str) -> Result<()> {
        let r = &repo.path;
        match to {
            // No reply endpoint: a one-comment review on the same file and line joins the thread.
            Some(e) if e.trigger == Trigger::ReviewComment && e.path.is_some() && e.line.is_some() => {
                let c = json!({ "path": e.path, "new_position": e.line, "body": body });
                self.api
                    .send(
                        Method::POST,
                        &format!("/repos/{r}/pulls/{n}/reviews"),
                        &json!({ "event": "COMMENT", "body": "", "comments": [c] }),
                    )
                    .await?;
            }
            _ => {
                self.api.send(Method::POST, &format!("/repos/{r}/issues/{n}/comments"), &json!({ "body": body })).await?;
            }
        }
        Ok(())
    }

    /// Resolve the conversation started by `comment_id`. Gitea 1.25+ only; Forgejo has no API.
    pub async fn resolve(&self, repo: &RepoId, comment_id: &str) -> Result<()> {
        match self.api.send(Method::POST, &format!("/repos/{}/pulls/comments/{comment_id}/resolve", repo.path), &json!({})).await {
            Err(e) if e.to_string().contains("HTTP 404") || e.to_string().contains("HTTP 405") => {
                bail!("this server has no API to resolve review conversations (Forgejo); resolve it in the web UI")
            }
            r => r.map(|_| ()),
        }
    }

    /// Draft = `WIP:` title prefix, which both Forgejo and Gitea treat as a draft PR.
    pub async fn open_draft(&self, repo: &RepoId, head: &str, base: &str, title: &str, body: &str) -> Result<String> {
        let v = self
            .api
            .send(
                Method::POST,
                &format!("/repos/{}/pulls", repo.path),
                &json!({ "title": format!("WIP: {title}"), "head": head, "base": base, "body": body }),
            )
            .await?;
        Ok(v["html_url"].as_str().unwrap_or_default().to_string())
    }

    /// `org/team` membership, cached for an hour.
    pub async fn in_team(&self, team: &str, user: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct Found {
            data: Vec<T>,
        }
        #[derive(Deserialize)]
        struct T {
            id: u64,
            name: String,
        }
        if let Some(m) = self.team_cache.get(&team.to_string()) {
            return Ok(m.iter().any(|x| x.eq_ignore_ascii_case(user)));
        }
        let (org, name) = team.split_once('/').context("team must be org/team")?;
        let found: Found = self.api.get(&format!("/orgs/{org}/teams/search?q={}", crate::http::enc(name))).await?;
        let t = found.data.into_iter().find(|t| t.name.eq_ignore_ascii_case(name)).with_context(|| format!("no team {team}"))?;
        // Every page: a member past the first page must still match (deny_teams fails open otherwise).
        let members: Vec<User> = self.api.get_all(&format!("/teams/{}/members?limit=50", t.id), 100).await?;
        let names: Vec<String> = members.into_iter().map(|u| u.login).collect();
        let hit = names.iter().any(|x| x.eq_ignore_ascii_case(user));
        self.team_cache.put(team.into(), names);
        Ok(hit)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn roles_and_bots() {
        use super::{Role, User, role_from};
        assert_eq!(role_from("owner"), Role::Owner);
        assert_eq!(role_from("admin"), Role::Maintainer);
        assert_eq!(role_from("write"), Role::Collaborator);
        assert_eq!(role_from("read"), Role::Contributor);
        assert_eq!(role_from("none"), Role::None);
        let bot = |l: &str| User { login: l.into() }.is_bot();
        assert!(bot("renovate-bot") && bot("forgejo-actions") && !bot("abbot"));
    }
}
