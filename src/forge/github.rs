//! GitHub: notifications polling, PR details, replies, thread resolution, the open-PR sweep.

use super::{Cursor, Event, PrInfo, PrState, RepoId, Role, Target, Trigger, Verb, parse_command};
use crate::forge::ForgeKind;
use crate::http::{Api, Got};
use anyhow::{Context, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

pub struct GitHub {
    pub host: String,
    pub me: String,
    pub api: Api,
    /// Maintainer label per repo (from config), to detect label requests.
    pub labels: HashMap<String, String>,
    /// Repos the user subscribed to (`owner/name`). Everything else is skipped before any
    /// per-thread API call, which keeps us well inside rate limits.
    pub subscribed: std::collections::HashSet<String>,
    pub sweep_every: std::time::Duration,
    team_cache: std::sync::Mutex<HashMap<String, (i64, Vec<String>)>>,
}

#[derive(Deserialize)]
struct User {
    login: String,
    #[serde(rename = "type", default)]
    kind: String,
}

impl User {
    fn is_bot(&self) -> bool {
        self.kind == "Bot" || self.login.ends_with("[bot]")
    }
}

#[derive(Deserialize)]
struct Thread {
    reason: String,
    updated_at: String,
    subject: Subject,
    repository: Repo,
}
#[derive(Deserialize)]
struct Subject {
    #[serde(rename = "type")]
    kind: String,
    url: Option<String>,
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
    author_association: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    html_url: String,
    created_at: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    in_reply_to_id: Option<u64>,
}

#[derive(Deserialize)]
struct Review {
    id: u64,
    user: Option<User>,
    #[serde(default)]
    author_association: String,
    #[serde(default)]
    body: String,
    state: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    submitted_at: Option<String>,
}

#[derive(Deserialize)]
struct IssueEvent {
    id: u64,
    event: String,
    actor: Option<User>,
    label: Option<Label>,
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
    mergeable_state: Option<String>,
    head: Head,
    base: Head,
}
#[derive(Deserialize)]
struct Head {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    repo: Option<HeadRepo>,
}
#[derive(Deserialize)]
struct HeadRepo {
    ssh_url: String,
    clone_url: String,
}

impl GitHub {
    pub fn new(host: &str, api: Api) -> Self {
        Self {
            host: host.into(),
            me: String::new(),
            api,
            labels: HashMap::new(),
            subscribed: Default::default(),
            sweep_every: std::time::Duration::from_secs(600),
            team_cache: Default::default(),
        }
    }

    pub fn api_base(host: &str) -> String {
        if host == "github.com" { "https://api.github.com".into() } else { format!("https://{host}/api/v3") }
    }

    fn repo(&self, full: &str) -> RepoId {
        RepoId::new(ForgeKind::GitHub, &self.host, full)
    }

    pub async fn login(&mut self) -> Result<()> {
        let u: User = self.api.get("/user").await.context("GitHub login")?;
        self.me = u.login;
        Ok(())
    }

    pub async fn poll(&mut self, cur: &mut Cursor) -> Result<Vec<Event>> {
        let since = cur.since.clone();
        let mut path = "/notifications?all=true&per_page=50".to_string();
        if let Some(s) = &since {
            path.push_str(&format!("&since={s}"));
        }
        let started = jiff::Timestamp::now().to_string();
        let mut out = Vec::new();
        match self.api.get_cond::<Vec<Thread>>(&path, cur.last_modified.as_deref()).await? {
            Got::NotModified => {}
            Got::Fresh(threads, headers) => {
                cur.last_modified = headers.get("last-modified").and_then(|v| v.to_str().ok()).map(String::from);
                cur.poll_interval_secs = headers.get("x-poll-interval").and_then(|v| v.to_str().ok()?.parse().ok());
                // ponytail: first page only (50 threads per poll interval); paginate if anyone gets more.
                for t in threads {
                    if !self.subscribed.contains(&t.repository.full_name) {
                        continue;
                    }
                    if let Err(e) = self.thread_events(&t, since.as_deref(), &mut out).await {
                        tracing::warn!("{}: {e:#}", t.repository.full_name);
                    }
                }
                cur.since = Some(started.clone());
            }
        }
        let due = cur.last_sweep.is_none_or(|t| crate::state::now() - t >= self.sweep_every.as_secs() as i64);
        if due {
            cur.last_sweep = Some(crate::state::now());
            out.extend(self.sweep(cur).await?);
        }
        Ok(out)
    }

    async fn thread_events(&self, t: &Thread, since: Option<&str>, out: &mut Vec<Event>) -> Result<()> {
        let Some(url) = &t.subject.url else { return Ok(()) };
        let number: u64 = url.rsplit('/').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if number == 0 {
            return Ok(());
        }
        let repo = self.repo(&t.repository.full_name);
        let r = &t.repository.full_name;
        let since_q = since.map(|s| format!("&since={s}")).unwrap_or_default();
        let after = |ts: &str| since.is_none_or(|s| ts > s);
        let (target, mine) = match t.subject.kind.as_str() {
            "PullRequest" => {
                let p: Pull = self.api.get(&format!("/repos/{r}/pulls/{number}")).await?;
                (Target::Pr, p.user.login.eq_ignore_ascii_case(&self.me))
            }
            "Issue" => (Target::Issue, false),
            _ => return Ok(()),
        };
        tracing::debug!("{r}#{number} reason={} updated={}", t.reason, t.updated_at);

        // Conversation comments (PRs and issues).
        let comments: Vec<Comment> = self.api.get(&format!("/repos/{r}/issues/{number}/comments?per_page=100{since_q}")).await?;
        for c in comments.into_iter().filter(|c| after(&c.created_at)) {
            let verb = parse_command(&c.body);
            if !mine && verb.is_none() {
                continue;
            }
            out.push(self.event(&repo, target, number, Trigger::Comment, format!("gh-ic-{}", c.id), c, verb));
        }

        if target == Target::Pr && mine {
            let rc: Vec<Comment> = self.api.get(&format!("/repos/{r}/pulls/{number}/comments?per_page=100{since_q}")).await?;
            for c in rc.into_iter().filter(|c| after(&c.created_at)) {
                let thread = Some(c.in_reply_to_id.unwrap_or(c.id).to_string());
                let mut e = self.event(&repo, target, number, Trigger::ReviewComment, format!("gh-rc-{}", c.id), c, None);
                e.thread = thread;
                out.push(e);
            }
            let reviews: Vec<Review> = self.api.get(&format!("/repos/{r}/pulls/{number}/reviews?per_page=100")).await?;
            for rv in reviews {
                let Some(at) = rv.submitted_at.clone() else { continue };
                // Approvals without text need no action; empty COMMENTED reviews are just
                // containers for line comments we already picked up.
                if !after(&at) || rv.state == "APPROVED" || rv.state == "PENDING" || (rv.state == "COMMENTED" && rv.body.trim().is_empty())
                {
                    continue;
                }
                let Some(user) = rv.user else { continue };
                let body =
                    if rv.body.trim().is_empty() { format!("(review: {})", rv.state.to_lowercase().replace('_', " ")) } else { rv.body };
                out.push(Event {
                    id: format!("gh-rv-{}", rv.id),
                    repo: repo.clone(),
                    target,
                    number,
                    trigger: Trigger::Review,
                    is_bot: user.is_bot(),
                    author: user.login,
                    role: Role::from_github(&rv.author_association),
                    body,
                    url: rv.html_url,
                    thread: None,
                    path: None,
                    line: None,
                    created_at: at,
                    verb: None,
                });
            }
        }

        // Maintainer label requests (on issues or others' PRs).
        if !mine && let Some(label) = self.labels.get(r) {
            let evs: Vec<IssueEvent> = self.api.get(&format!("/repos/{r}/issues/{number}/events?per_page=100")).await?;
            for ev in evs.into_iter().filter(|e| e.event == "labeled" && after(&e.created_at)) {
                let (Some(l), Some(actor)) = (&ev.label, &ev.actor) else { continue };
                if !l.name.eq_ignore_ascii_case(label) {
                    continue;
                }
                let role = self.role_of(r, &actor.login).await.unwrap_or(Role::None);
                out.push(Event {
                    id: format!("gh-label-{}", ev.id),
                    repo: repo.clone(),
                    target,
                    number,
                    trigger: Trigger::Comment,
                    author: actor.login.clone(),
                    role,
                    is_bot: actor.is_bot(),
                    body: format!("(label `{label}` added)"),
                    url: String::new(),
                    thread: None,
                    path: None,
                    line: None,
                    created_at: ev.created_at,
                    verb: Some(Verb::Fix),
                });
            }
        }
        Ok(())
    }

    fn event(&self, repo: &RepoId, target: Target, number: u64, trigger: Trigger, id: String, c: Comment, verb: Option<Verb>) -> Event {
        Event {
            id,
            repo: repo.clone(),
            target,
            number,
            trigger,
            is_bot: c.user.is_bot(),
            author: c.user.login,
            role: Role::from_github(&c.author_association),
            body: c.body,
            url: c.html_url,
            thread: None,
            path: c.path,
            line: c.line,
            created_at: c.created_at,
            verb,
        }
    }

    /// Collaborator permission → role (labels don't carry `author_association`).
    async fn role_of(&self, repo: &str, user: &str) -> Result<Role> {
        #[derive(Deserialize)]
        struct P {
            permission: String,
        }
        let p: P = self.api.get(&format!("/repos/{repo}/collaborators/{user}/permission")).await?;
        Ok(match p.permission.as_str() {
            "admin" => Role::Owner,
            "maintain" => Role::Maintainer,
            "write" => Role::Collaborator,
            _ => Role::None,
        })
    }

    /// Slow sweep of the user's open PRs: merge conflicts and failing CI.
    /// Events are keyed by head SHA so each state fires once per push.
    async fn sweep(&self, cur: &mut Cursor) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        for (repo, n) in self.my_open().await? {
            if !self.subscribed.contains(&repo.path) {
                continue;
            }
            let r = &repo.path;
            let p: Pull = self.api.get(&format!("/repos/{r}/pulls/{n}")).await?;
            let key = format!("{}#{n}", repo.key());
            let mut status = Vec::new();
            if p.mergeable_state.as_deref() == Some("dirty") {
                status.push("conflict".to_string());
                out.push(self.state_event(
                    &repo,
                    n,
                    Trigger::MergeConflict,
                    &p.head.sha,
                    format!("This PR has merge conflicts with `{}`.", p.base.name),
                ));
            }
            #[derive(Deserialize)]
            struct Runs {
                check_runs: Vec<Run>,
            }
            #[derive(Deserialize)]
            struct Run {
                name: String,
                conclusion: Option<String>,
                #[serde(default)]
                html_url: String,
                output: Option<Out>,
            }
            #[derive(Deserialize)]
            struct Out {
                title: Option<String>,
                summary: Option<String>,
            }
            let runs: Runs = self.api.get(&format!("/repos/{r}/commits/{}/check-runs?per_page=100", p.head.sha)).await?;
            let failed: Vec<&Run> =
                runs.check_runs.iter().filter(|c| matches!(c.conclusion.as_deref(), Some("failure" | "timed_out"))).collect();
            if failed.is_empty() {
                if runs.check_runs.iter().all(|c| c.conclusion.is_some()) && !runs.check_runs.is_empty() {
                    status.push("green".into());
                }
            } else {
                status.push("red".into());
                let mut body = String::from("CI failed:\n");
                for f in failed {
                    let o = f.output.as_ref();
                    body.push_str(&format!(
                        "- {} ({}) {}\n  {}\n",
                        f.name,
                        f.html_url,
                        o.and_then(|o| o.title.clone()).unwrap_or_default(),
                        o.and_then(|o| o.summary.clone()).unwrap_or_default().chars().take(800).collect::<String>()
                    ));
                }
                out.push(self.state_event(&repo, n, Trigger::CiFailure, &p.head.sha, body));
            }
            cur.seen_status.insert(key, status.join(","));
        }
        Ok(out)
    }

    fn state_event(&self, repo: &RepoId, n: u64, trigger: Trigger, sha: &str, body: String) -> Event {
        Event {
            id: format!("gh-{}-{}#{n}-{sha}", crate::state::enum_str(&trigger), repo.path),
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
            created_at: jiff::Timestamp::now().to_string(),
            verb: None,
        }
    }

    pub async fn my_open(&self) -> Result<Vec<(RepoId, u64)>> {
        #[derive(Deserialize)]
        struct S {
            items: Vec<I>,
        }
        #[derive(Deserialize)]
        struct I {
            number: u64,
            repository_url: String,
        }
        let s: S = self.api.get(&format!("/search/issues?q=is:pr+is:open+author:{}&per_page=100", self.me)).await?;
        Ok(s.items
            .into_iter()
            .filter_map(|i| {
                let full = i.repository_url.split("/repos/").nth(1)?;
                Some((self.repo(full), i.number))
            })
            .collect())
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

    pub async fn reply(&self, repo: &RepoId, _target: Target, n: u64, to: Option<&Event>, body: &str) -> Result<()> {
        let r = &repo.path;
        match to {
            Some(e) if e.trigger == Trigger::ReviewComment && e.thread.is_some() => {
                let id = e.thread.as_deref().unwrap_or_default();
                self.api.send(Method::POST, &format!("/repos/{r}/pulls/{n}/comments/{id}/replies"), &json!({ "body": body })).await?;
            }
            _ => {
                self.api.send(Method::POST, &format!("/repos/{r}/issues/{n}/comments"), &json!({ "body": body })).await?;
            }
        }
        Ok(())
    }

    /// Resolve the review thread containing comment `comment_id` (GraphQL only).
    pub async fn resolve(&self, repo: &RepoId, comment_id: &str) -> Result<()> {
        let gql = format!("{}/graphql", self.api.base.trim_end_matches("/v3"));
        let node: serde_json::Value = self.api.get(&format!("/repos/{}/pulls/comments/{comment_id}", repo.path)).await?;
        // Find the thread via the comment's node id.
        let q = json!({
            "query": "query($id:ID!){ node(id:$id){ ... on PullRequestReviewComment { pullRequestReview { pullRequest { reviewThreads(first:100){ nodes { id isResolved comments(first:1){ nodes { databaseId } } } } } } } } }",
            "variables": { "id": node.get("node_id").and_then(|v| v.as_str()).unwrap_or_default() }
        });
        let v = self.api.send(Method::POST, &gql, &q).await?;
        let want: u64 = comment_id.parse().unwrap_or(0);
        let threads = v
            .pointer("/data/node/pullRequestReview/pullRequest/reviewThreads/nodes")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        for t in threads {
            let first = t.pointer("/comments/nodes/0/databaseId").and_then(|d| d.as_u64());
            if first == Some(want) && t["isResolved"] == false {
                let m = json!({ "query": "mutation($id:ID!){ resolveReviewThread(input:{threadId:$id}){ thread { id } } }", "variables": { "id": t["id"] } });
                self.api.send(Method::POST, &gql, &m).await?;
            }
        }
        Ok(())
    }

    pub async fn open_draft(&self, repo: &RepoId, head: &str, base: &str, title: &str, body: &str) -> Result<String> {
        let v = self
            .api
            .send(
                Method::POST,
                &format!("/repos/{}/pulls", repo.path),
                &json!({ "title": title, "head": head, "base": base, "body": body, "draft": true }),
            )
            .await?;
        Ok(v["html_url"].as_str().unwrap_or_default().to_string())
    }

    /// `org/team` membership, cached for an hour.
    pub async fn in_team(&self, team: &str, user: &str) -> Result<bool> {
        let now = crate::state::now();
        if let Some((at, members)) = self.team_cache.lock().unwrap().get(team)
            && now - at < 3600
        {
            return Ok(members.iter().any(|m| m.eq_ignore_ascii_case(user)));
        }
        let (org, slug) = team.split_once('/').context("team must be org/team")?;
        let members: Vec<User> = self.api.get(&format!("/orgs/{org}/teams/{slug}/members?per_page=100")).await?;
        let members: Vec<String> = members.into_iter().map(|u| u.login).collect();
        let hit = members.iter().any(|m| m.eq_ignore_ascii_case(user));
        self.team_cache.lock().unwrap().insert(team.into(), (now, members));
        Ok(hit)
    }
}
