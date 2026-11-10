//! GitHub: notifications polling, PR details, replies, thread resolution, the open-PR sweep.

use super::ghlike::{self, ListItem};
use super::{Cursor, Event, PrInfo, RepoId, Role, Target, Trigger, Verb, newer, now_iso, parse_command};
use crate::forge::ForgeKind;
use crate::http::{Api, Got};
use anyhow::{Context, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

pub struct GitHub {
    pub host: String,
    pub me: String,
    /// The token user's numeric id: commands on PRs are only taken from this id.
    pub me_id: u64,
    pub api: Api,
    /// Maintainer label per repo (from config), to detect label requests.
    pub labels: HashMap<String, String>,
    /// Repos the user subscribed to (`owner/name`). Everything else is skipped before any
    /// per-thread API call, which keeps us well inside rate limits.
    pub subscribed: std::collections::HashSet<String>,
    pub sweep_every: std::time::Duration,
    team_cache: super::Ttl<String, Vec<String>>,
    perm_cache: super::Ttl<String, Role>,
}

#[derive(Deserialize)]
struct User {
    login: String,
    #[serde(default)]
    id: u64,
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
    updated_at: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    in_reply_to_id: Option<u64>,
    /// Set when a GitHub App posted this on the user's behalf.
    #[serde(default)]
    performed_via_github_app: Option<serde_json::Value>,
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

impl GitHub {
    pub fn new(host: &str, api: Api) -> Self {
        Self {
            host: host.into(),
            me: String::new(),
            me_id: 0,
            api,
            labels: HashMap::new(),
            subscribed: HashSet::default(),
            sweep_every: std::time::Duration::from_secs(600),
            team_cache: super::Ttl::default(),
            perm_cache: super::Ttl::default(),
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
        self.me_id = u.id;
        Ok(())
    }

    pub async fn poll(&mut self, cur: &mut Cursor) -> Result<Vec<Event>> {
        // Overlapping window: forges index late and clocks drift. Repeats are de-duplicated.
        let since = cur.since.as_deref().map(super::overlap);
        let mut path = "/notifications?all=true&per_page=50".to_string();
        if let Some(s) = &since {
            let _ = write!(path, "&since={s}");
        }
        let started = now_iso();
        let mut out = Vec::new();
        match self.api.get_cond::<Vec<Thread>>(&path, cur.last_modified.as_deref()).await? {
            Got::NotModified => {}
            Got::Fresh(mut threads, headers) => {
                cur.poll_interval_secs = headers.get("x-poll-interval").and_then(|v| v.to_str().ok()?.parse().ok());
                let mut next = crate::http::next_link(&headers).filter(|u| self.api.is_ours(u));
                for _ in 0..10 {
                    let Some(p) = next.take() else { break };
                    if let Got::Fresh(more, h) = self.api.get_cond::<Vec<Thread>>(&p, None).await? {
                        threads.extend(more);
                        next = crate::http::next_link(&h).filter(|u| self.api.is_ours(u));
                    }
                }
                let mut failed = false;
                for t in threads.iter().filter(|t| self.subscribed.contains(&t.repository.full_name)) {
                    if let Err(e) = self.thread_events(t, since.as_deref(), &mut out).await {
                        tracing::warn!("{}: {e:#}", t.repository.full_name);
                        failed = true;
                    }
                }
                // Only move the window forward when everything was read; otherwise retry it.
                if !failed {
                    cur.last_modified = headers.get("last-modified").and_then(|v| v.to_str().ok()).map(String::from);
                    cur.since = Some(started);
                }
            }
        }
        let catch_up = super::catch_up_since(cur.last_sweep, cur.swept);
        if cur.sweep_due(self.sweep_every) {
            match self.sweep(cur, catch_up.as_deref()).await {
                Ok(evs) => out.extend(evs),
                Err(e) => tracing::warn!("sweep: {e:#}"),
            }
        }
        Ok(out)
    }

    async fn thread_events(&self, t: &Thread, since: Option<&str>, out: &mut Vec<Event>) -> Result<()> {
        let Some(url) = &t.subject.url else { return Ok(()) };
        let number: u64 = url.rsplit('/').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if number == 0 {
            return Ok(());
        }
        let start = out.len();
        let repo = self.repo(&t.repository.full_name);
        let r = &t.repository.full_name;
        let since_q = since.map(|s| format!("&since={s}")).unwrap_or_default();
        let after = |ts: &str| newer(ts, since);
        let (target, mine) = match t.subject.kind.as_str() {
            "PullRequest" => {
                let p = ghlike::pull(&self.api, r, number).await?;
                let mine = p.user.login.eq_ignore_ascii_case(&self.me);
                // Still waiting for your review (you haven't submitted one since it was asked for).
                if t.reason == "review_requested"
                    && !mine
                    && p.state == "open"
                    && p.requested_reviewers.iter().any(|u| u.login.eq_ignore_ascii_case(&self.me))
                {
                    out.push(Event::review_request(&repo, number, &p.head.sha, &self.me, self.me_id));
                }
                (Target::Pr, mine)
            }
            "Issue" => (Target::Issue, false),
            _ => return Ok(()),
        };
        tracing::debug!("{r}#{number} reason={} updated={}", t.reason, t.updated_at);

        // Conversation comments (PRs and issues).
        let comments: Vec<Comment> = self.api.get_all(&format!("/repos/{r}/issues/{number}/comments?per_page=100{since_q}"), 10).await?;
        for c in comments.into_iter().filter(|c| after(&c.created_at)) {
            let verb = parse_command(&c.body);
            if !mine && verb.is_none() {
                continue;
            }
            out.push(Self::event(&repo, target, number, Trigger::Comment, format!("gh-{}-ic-{}", self.host, c.id), c, verb));
        }

        if target == Target::Pr && mine {
            let rc: Vec<Comment> = self.api.get_all(&format!("/repos/{r}/pulls/{number}/comments?per_page=100{since_q}"), 10).await?;
            for c in rc.into_iter().filter(|c| after(&c.created_at)) {
                let thread = Some(c.in_reply_to_id.unwrap_or(c.id).to_string());
                let mut e = Self::event(&repo, target, number, Trigger::ReviewComment, format!("gh-{}-rc-{}", self.host, c.id), c, None);
                e.thread = thread;
                out.push(e);
            }
            let reviews: Vec<Review> = self.api.get_all(&format!("/repos/{r}/pulls/{number}/reviews?per_page=100"), 20).await?;
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
                    is_bot: user.is_bot(),
                    author_id: user.id,
                    author: user.login,
                    role: Role::from_github(&rv.author_association),
                    body,
                    url: rv.html_url,
                    created_at: at,
                    ..Event::new(format!("gh-{}-rv-{}", self.host, rv.id), &repo, target, number, Trigger::Review)
                });
            }
        }

        // Maintainer label requests (on issues or others' PRs).
        if !mine && let Some(label) = self.labels.get(r) {
            let evs: Vec<IssueEvent> = self.api.get_all(&format!("/repos/{r}/issues/{number}/events?per_page=100"), 10).await?;
            for ev in evs.into_iter().filter(|e| e.event == "labeled" && after(&e.created_at)) {
                let (Some(l), Some(actor)) = (&ev.label, &ev.actor) else { continue };
                if !l.name.eq_ignore_ascii_case(label) {
                    continue;
                }
                let role = self.role_of(r, &actor.login).await.unwrap_or(Role::None);
                out.push(Event {
                    author: actor.login.clone(),
                    author_id: actor.id,
                    role,
                    is_bot: actor.is_bot(),
                    body: format!("(label `{label}` added)"),
                    created_at: ev.created_at,
                    verb: Some(Verb::Fix),
                    ..Event::new(format!("gh-{}-label-{}", self.host, ev.id), &repo, target, number, Trigger::Comment)
                });
            }
        }
        self.verify_roles(&t.repository.full_name, &mut out[start..]).await;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn event(repo: &RepoId, target: Target, number: u64, trigger: Trigger, id: String, c: Comment, verb: Option<Verb>) -> Event {
        Event {
            is_bot: c.user.is_bot(),
            author_id: c.user.id,
            author: c.user.login,
            role: Role::from_github(&c.author_association),
            body: c.body,
            url: c.html_url,
            path: c.path,
            line: c.line,
            created_at: c.created_at,
            verb,
            ..Event::new(id, repo, target, number, trigger)
        }
    }

    /// Collaborator permission → role, cached for an hour. Used for labels (no association) and
    /// to check MEMBER/COLLABORATOR associations, which GitHub also gives read-only users.
    async fn role_of(&self, repo: &str, user: &str) -> Result<Role> {
        #[derive(Deserialize)]
        struct P {
            permission: String,
        }
        let key = format!("{repo}:{}", user.to_ascii_lowercase());
        if let Some(r) = self.perm_cache.get(&key) {
            return Ok(r);
        }
        let p: P = self.api.get(&format!("/repos/{repo}/collaborators/{user}/permission")).await?;
        let r = match p.permission.as_str() {
            "admin" => Role::Owner,
            "maintain" => Role::Maintainer,
            "write" => Role::Collaborator,
            _ => Role::Contributor,
        };
        self.perm_cache.put(key, r);
        Ok(r)
    }

    /// Replace association-based MEMBER/COLLABORATOR with the user's real permission.
    async fn verify_roles(&self, repo: &str, events: &mut [Event]) {
        for e in events.iter_mut().filter(|e| matches!(e.role, Role::Member | Role::Collaborator) && !e.is_bot) {
            e.role = self.role_of(repo, &e.author).await.unwrap_or(Role::Contributor);
        }
    }

    /// Slow sweep of the user's open PRs: merge conflicts and failing CI.
    /// Events are keyed by head SHA so each state fires once per push.
    /// The first sweep after install only records state, so PRs that were already red or
    /// conflicted don't all start jobs at once.
    /// `catch_up`: also re-read reviews and comments since then, in case a notification never
    /// came (GitHub sometimes delivers them late, after the poll window has moved on).
    async fn sweep(&self, cur: &mut Cursor, catch_up: Option<&str>) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        for (repo, n) in self.my_open().await? {
            if !self.subscribed.contains(&repo.path) {
                continue;
            }
            let r = self.sweep_pr(&repo, n).await;
            cur.record(format!("{}#{n}", repo.key()), r, &mut out);
            if let Some(since) = catch_up
                && let Err(e) = self.pr_events(&repo, n, since, &mut out).await
            {
                tracing::warn!("catch up {repo}#{n}: {e:#}");
            }
        }
        cur.swept = true;
        Ok(out)
    }

    /// Your PR `n`'s reviews and comments since `since`, as if its notification had come in.
    async fn pr_events(&self, repo: &RepoId, n: u64, since: &str, out: &mut Vec<Event>) -> Result<()> {
        let t = Thread {
            reason: "author".into(),
            updated_at: String::new(),
            subject: Subject { kind: "PullRequest".into(), url: Some(format!("/repos/{}/pulls/{n}", repo.path)) },
            repository: Repo { full_name: repo.path.clone() },
        };
        self.thread_events(&t, Some(since), out).await
    }

    pub async fn rescan(&self, repo: &RepoId, since: &str) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        for (r, n) in self.my_open().await?.into_iter().filter(|(r, _)| r.path.eq_ignore_ascii_case(&repo.path)) {
            self.pr_events(&r, n, since, &mut out).await?;
        }
        Ok(out)
    }

    async fn sweep_pr(&self, repo: &RepoId, n: u64) -> Result<(Vec<Event>, String)> {
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
        let mut out = Vec::new();
        {
            let r = &repo.path;
            let repo = repo.clone();
            let p = ghlike::pull(&self.api, r, n).await?;
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
                    let _ = writeln!(
                        body,
                        "- {} ({}) {}\n  {}",
                        f.name,
                        f.html_url,
                        o.and_then(|o| o.title.clone()).unwrap_or_default(),
                        o.and_then(|o| o.summary.clone()).unwrap_or_default().chars().take(800).collect::<String>()
                    );
                }
                out.push(self.state_event(&repo, n, Trigger::CiFailure, &p.head.sha, body));
            }
            Ok((out, status.join(",")))
        }
    }

    fn state_event(&self, repo: &RepoId, n: u64, trigger: Trigger, sha: &str, body: String) -> Event {
        let id = format!("gh-{}-{}-{}#{n}-{sha}", self.host, crate::state::enum_str(&trigger), repo.path);
        Event::state(id, repo, n, trigger, &self.me, body)
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
        ghlike::pr(&self.api, repo, n).await
    }

    pub async fn open_items(&self, repo: &RepoId, target: Target) -> Result<Vec<super::Listed>> {
        // `/issues` lists PRs too (with a `pull_request` key); `/pulls` only PRs.
        let path = match target {
            Target::Issue => format!("/repos/{}/issues?state=open&per_page=100", repo.path),
            Target::Pr => format!("/repos/{}/pulls?state=open&per_page=100", repo.path),
        };
        let items: Vec<ListItem> = self.api.get_all(&path, super::LIST_PAGES).await?;
        Ok(items.into_iter().filter(|i| target == Target::Pr || i.pull_request.is_none()).map(ListItem::listed).collect())
    }

    pub async fn issue(&self, repo: &RepoId, n: u64) -> Result<super::IssueInfo> {
        ghlike::issue(&self.api, repo, n).await
    }

    pub async fn reply(&self, repo: &RepoId, _target: Target, n: u64, to: Option<&Event>, body: &str) -> Result<()> {
        let r = &repo.path;
        match to {
            Some(e) if e.trigger == Trigger::ReviewComment && e.thread.is_some() => {
                let id = e.thread.as_deref().unwrap_or_default();
                self.api.send(Method::POST, &format!("/repos/{r}/pulls/{n}/comments/{id}/replies"), &json!({ "body": body })).await?;
            }
            _ => ghlike::comment(&self.api, repo, n, body).await?,
        }
        Ok(())
    }

    /// Resolve the review thread containing comment `comment_id` (GraphQL only).
    pub async fn resolve(&self, repo: &RepoId, _n: u64, comment_id: &str) -> Result<()> {
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
            let first = t.pointer("/comments/nodes/0/databaseId").and_then(serde_json::Value::as_u64);
            if first == Some(want) && t["isResolved"] == false {
                let m = json!({ "query": "mutation($id:ID!){ resolveReviewThread(input:{threadId:$id}){ thread { id } } }", "variables": { "id": t["id"] } });
                self.api.send(Method::POST, &gql, &m).await?;
            }
        }
        Ok(())
    }

    /// PR `n`'s review threads that are unresolved and not yet answered by you, as the event of
    /// each thread's first comment (same id as when it arrives by notification). GraphQL only:
    /// REST doesn't say whether a thread is resolved.
    // ponytail: first 100 threads; page when a PR has more open ones.
    pub async fn open_threads(&self, repo: &RepoId, n: u64) -> Result<Vec<Event>> {
        let (owner, name) = repo.path.split_once('/').context("repo must be owner/name")?;
        let query = json!({
            "query": "query($o:String!,$r:String!,$n:Int!){ repository(owner:$o,name:$r){ pullRequest(number:$n){ reviewThreads(first:100){ nodes { isResolved first:comments(first:1){ nodes { databaseId author { login __typename } body path line createdAt url } } last:comments(last:1){ nodes { author { login } } } } } } } }",
            "variables": { "o": owner, "r": name, "n": n }
        });
        let v = self.api.send(Method::POST, &format!("{}/graphql", self.api.base.trim_end_matches("/v3")), &query).await?;
        let threads = v.pointer("/data/repository/pullRequest/reviewThreads/nodes").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        let text = |v: &serde_json::Value, p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let mut out = Vec::new();
        for t in threads {
            let c = &t["first"]["nodes"][0];
            let Some(id) = c["databaseId"].as_u64() else { continue };
            if t["isResolved"] != false || text(&t, "/last/nodes/0/author/login").eq_ignore_ascii_case(&self.me) {
                continue;
            }
            // GraphQL gives an app's login without the `[bot]` that REST (and `allow_bots`) use.
            let is_bot = text(c, "/author/__typename") == "Bot";
            let login = text(c, "/author/login");
            out.push(Event {
                is_bot,
                author: if is_bot && !login.ends_with("[bot]") { format!("{login}[bot]") } else { login },
                body: text(c, "/body"),
                url: text(c, "/url"),
                thread: Some(id.to_string()),
                path: c["path"].as_str().map(String::from),
                line: c["line"].as_u64(),
                created_at: text(c, "/createdAt"),
                ..Event::new(format!("gh-{}-rc-{id}", self.host), repo, Target::Pr, n, Trigger::ReviewComment)
            });
        }
        Ok(out)
    }

    pub async fn open_draft(&self, repo: &RepoId, head: &str, base: &str, title: &str, body: &str) -> Result<String> {
        ghlike::open_pr(&self.api, repo, &json!({ "title": title, "head": head, "base": base, "body": body, "draft": true })).await
    }

    pub async fn controls(&self, repo: &RepoId, _target: Target, n: u64, since: &str) -> Result<Vec<super::ControlComment>> {
        let cs: Vec<Comment> =
            self.api.get_all(&format!("/repos/{}/issues/{n}/comments?per_page=100&since={since}", repo.path), 10).await?;
        Ok(cs
            .into_iter()
            .filter_map(|c| {
                let via_app = c.performed_via_github_app.as_ref().is_some_and(|v| !v.is_null());
                let control = super::control_of(self.me_id, c.user.id, via_app, &c.body, &c.created_at, &c.updated_at, since)?;
                Some(super::ControlComment { id: c.id.to_string(), control })
            })
            .collect())
    }

    pub async fn ack(&self, repo: &RepoId, _target: Target, _n: u64, comment: &str) -> Result<()> {
        ghlike::ack(&self.api, repo, comment).await
    }

    /// `org/team` membership, cached for an hour.
    pub async fn in_team(&self, team: &str, user: &str) -> Result<bool> {
        let (org, slug) = team.split_once('/').context("team must be org/team")?;
        let fetch = async {
            let members: Vec<User> = self.api.get_every(&format!("/orgs/{org}/teams/{slug}/members?per_page=100"), 100).await?;
            Ok(members.into_iter().map(|u| u.login).collect())
        };
        self.team_cache.member(team, user, fetch).await
    }
}
