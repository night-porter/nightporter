//! GitLab (gitlab.com and self-hosted). No notifications feed, so we poll the user's open MRs
//! (`updated_after`) and, for maintainer mode, recently updated issues/MRs in subscribed projects.

use super::{Cursor, Event, PrInfo, PrState, RepoId, Role, Target, Trigger, Verb, newer, now_iso, parse_command};
use crate::forge::ForgeKind;
use crate::http::{Api, enc};
use anyhow::{Context, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

pub struct GitLab {
    pub host: String,
    pub me: String,
    /// The token user's numeric id: commands on MRs are only taken from this id.
    pub me_id: u64,
    pub api: Api,
    /// Maintainer label per subscribed repo (`group/name`) with maintainer mode on.
    pub labels: HashMap<String, String>,
    pub subscribed: HashSet<String>,
    pub sweep_every: std::time::Duration,
    roles: super::Ttl<(String, u64), Role>,
    groups: super::Ttl<String, Vec<String>>,
}

#[derive(Deserialize, Clone)]
struct User {
    id: u64,
    username: String,
    #[serde(default)]
    bot: bool,
}
impl User {
    fn is_bot(&self) -> bool {
        self.bot || self.username.ends_with("_bot") || self.username.ends_with("-bot")
    }
}

#[derive(Deserialize)]
struct Mr {
    iid: u64,
    project_id: u64,
    title: String,
    #[serde(default)]
    description: Option<String>,
    author: User,
    state: String,
    #[serde(default)]
    draft: bool,
    source_branch: String,
    target_branch: String,
    source_project_id: u64,
    #[serde(default)]
    sha: Option<String>,
    references: Refs,
    #[serde(default)]
    has_conflicts: bool,
    #[serde(default)]
    head_pipeline: Option<Pipeline>,
}
#[derive(Deserialize)]
struct Refs {
    full: String,
}
#[derive(Deserialize)]
struct Pipeline {
    id: u64,
    status: String,
}
#[derive(Deserialize)]
struct Issue {
    iid: u64,
    references: Refs,
}

#[derive(Deserialize)]
struct Discussion {
    id: String,
    notes: Vec<Note>,
}
#[derive(Deserialize)]
struct Note {
    id: u64,
    author: User,
    body: String,
    #[serde(default)]
    system: bool,
    created_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    position: Option<Position>,
}
#[derive(Deserialize)]
struct Position {
    new_path: Option<String>,
    new_line: Option<u64>,
}

#[derive(Deserialize)]
struct LabelEvent {
    id: u64,
    user: User,
    label: Option<Name>,
    action: String,
    created_at: String,
}
#[derive(Deserialize)]
struct Name {
    name: String,
}

impl GitLab {
    pub fn new(host: &str, api: Api) -> Self {
        Self {
            host: host.into(),
            me: String::new(),
            me_id: 0,
            api,
            labels: HashMap::new(),
            subscribed: HashSet::new(),
            sweep_every: std::time::Duration::from_secs(600),
            roles: super::Ttl::default(),
            groups: super::Ttl::default(),
        }
    }

    pub fn api_base(host: &str) -> String {
        format!("https://{host}/api/v4")
    }

    fn repo(&self, full_ref: &str) -> RepoId {
        // `group/sub/name!12` or `group/name#3`
        let path = full_ref.split(['!', '#']).next().unwrap_or(full_ref);
        RepoId::new(ForgeKind::GitLab, &self.host, path)
    }

    pub async fn login(&mut self) -> Result<()> {
        let u: User = self.api.get("/user").await.context("GitLab login")?;
        self.me = u.username;
        self.me_id = u.id;
        Ok(())
    }

    pub async fn poll(&mut self, cur: &mut Cursor) -> Result<Vec<Event>> {
        // Overlapping window (late indexing, clock drift); repeats are de-duplicated downstream.
        let since = cur.since.as_deref().map(super::overlap);
        let started = now_iso();
        let upd = since.as_ref().map(|s| format!("&updated_after={s}")).unwrap_or_default();
        let after = |ts: &str| newer(ts, since.as_deref());
        let mut out = Vec::new();
        let mut failed = false;

        // 1. My open MRs: new discussion notes.
        let mrs: Vec<Mr> = self.api.get_all(&format!("/merge_requests?scope=created_by_me&state=opened&per_page=100{upd}"), 10).await?;
        for mr in &mrs {
            let repo = self.repo(&mr.references.full);
            if !self.subscribed.contains(&repo.path) {
                continue;
            }
            if let Err(e) = self.mr_notes(&repo, mr.project_id, mr.iid, true, &after, &mut out).await {
                tracing::warn!("{repo}!{}: {e:#}", mr.iid);
                failed = true;
            }
        }

        // 2. Maintainer mode: commands and labels on others' issues/MRs in subscribed projects.
        for (path, label) in self.labels.clone() {
            let p = enc(&path);
            let scan = async {
                let issues: Vec<Issue> = self.api.get_all(&format!("/projects/{p}/issues?state=opened&per_page=100{upd}"), 5).await?;
                for i in issues {
                    let repo = self.repo(&i.references.full);
                    self.issue_requests(&repo, &p, "issues", i.iid, Target::Issue, &label, &after, &mut out).await?;
                }
                let others: Vec<Mr> = self.api.get_all(&format!("/projects/{p}/merge_requests?state=opened&per_page=100{upd}"), 5).await?;
                for mr in others.into_iter().filter(|m| !m.author.username.eq_ignore_ascii_case(&self.me)) {
                    let repo = self.repo(&mr.references.full);
                    self.issue_requests(&repo, &p, "merge_requests", mr.iid, Target::Pr, &label, &after, &mut out).await?;
                }
                anyhow::Ok(())
            };
            if let Err(e) = scan.await {
                tracing::warn!("{path}: {e:#}");
                failed = true;
            }
        }
        if !failed {
            cur.since = Some(started);
        }

        if cur.sweep_due(self.sweep_every) {
            // All open MRs, not just the ones updated since the last poll: CI can go red without
            // the MR itself changing.
            match self.api.get_all::<Mr>("/merge_requests?scope=created_by_me&state=opened&per_page=100", 10).await {
                Ok(open) => {
                    for mr in open.iter().filter(|m| self.subscribed.contains(&self.repo(&m.references.full).path)) {
                        let repo = self.repo(&mr.references.full);
                        let r = self.sweep_one(&repo, mr.project_id, mr.iid).await;
                        cur.record(format!("{}#{}", repo.key(), mr.iid), r, &mut out);
                    }
                    // Only once the list was read: else the next sweep would fire for MRs that
                    // were already red.
                    cur.swept = true;
                }
                Err(e) => tracing::warn!("sweep: {e:#}"),
            }
        }
        Ok(out)
    }

    async fn mr_notes(
        &self,
        repo: &RepoId,
        pid: u64,
        iid: u64,
        mine: bool,
        after: &impl Fn(&str) -> bool,
        out: &mut Vec<Event>,
    ) -> Result<()> {
        let ds: Vec<Discussion> = self.api.get_all(&format!("/projects/{pid}/merge_requests/{iid}/discussions?per_page=100"), 20).await?;
        for d in ds {
            for n in d.notes.into_iter().filter(|n| !n.system && after(&n.created_at)) {
                let verb = parse_command(&n.body);
                if !mine && verb.is_none() {
                    continue;
                }
                let role = self.role(&pid.to_string(), &n.author).await;
                let (path, line) = n.position.map(|p| (p.new_path, p.new_line)).unwrap_or_default();
                let trigger = if path.is_some() { Trigger::ReviewComment } else { Trigger::Comment };
                out.push(Event {
                    is_bot: n.author.is_bot(),
                    author: n.author.username,
                    role,
                    body: n.body,
                    thread: Some(d.id.clone()),
                    path,
                    line,
                    created_at: n.created_at,
                    verb,
                    ..Event::new(format!("gl-{}-n{}", self.host, n.id), repo, Target::Pr, iid, trigger)
                });
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn issue_requests(
        &self,
        repo: &RepoId,
        p: &str,
        kind: &str,
        iid: u64,
        target: Target,
        label: &str,
        after: &impl Fn(&str) -> bool,
        out: &mut Vec<Event>,
    ) -> Result<()> {
        let notes: Vec<Note> = self.api.get_all(&format!("/projects/{p}/{kind}/{iid}/notes?per_page=100&sort=desc"), 3).await?;
        for n in notes.into_iter().filter(|n| !n.system && after(&n.created_at)) {
            let Some(verb) = parse_command(&n.body) else { continue };
            let role = self.role(p, &n.author).await;
            out.push(Self::request(repo, target, iid, format!("gl-{}-n{}", self.host, n.id), &n.author, role, n.body, n.created_at, verb));
        }
        let evs: Vec<LabelEvent> = self.api.get_all(&format!("/projects/{p}/{kind}/{iid}/resource_label_events?per_page=100"), 5).await?;
        for e in evs.into_iter().filter(|e| e.action == "add" && after(&e.created_at)) {
            if e.label.as_ref().is_some_and(|l| l.name.eq_ignore_ascii_case(label)) {
                let role = self.role(p, &e.user).await;
                out.push(Self::request(
                    repo,
                    target,
                    iid,
                    format!("gl-{}-l{}", self.host, e.id),
                    &e.user,
                    role,
                    format!("(label `{label}` added)"),
                    e.created_at,
                    Verb::Fix,
                ));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn request(repo: &RepoId, target: Target, n: u64, id: String, u: &User, role: Role, body: String, at: String, verb: Verb) -> Event {
        Event {
            author: u.username.clone(),
            role,
            is_bot: u.is_bot(),
            body,
            created_at: at,
            verb: Some(verb),
            ..Event::new(id, repo, target, n, Trigger::Comment)
        }
    }

    /// Project membership (including inherited) → role, cached for an hour.
    async fn role(&self, project: &str, u: &User) -> Role {
        #[derive(Deserialize)]
        struct M {
            access_level: u32,
        }
        let key = (project.to_string(), u.id);
        if let Some(r) = self.roles.get(&key) {
            return r;
        }
        match self.api.get::<M>(&format!("/projects/{project}/members/all/{}", u.id)).await {
            Ok(m) => {
                let r = Role::from_gitlab(m.access_level);
                self.roles.put(key, r);
                r
            }
            // Not a member: cache. Other errors (5xx, rate limit): don't, so a hiccup doesn't
            // send maintainers to the inbox for an hour.
            Err(e) if crate::http::status(&e) == Some(404) => {
                self.roles.put(key, Role::None);
                Role::None
            }
            Err(_) => Role::None,
        }
    }

    /// One MR's conflict/CI state. The list endpoint has no `head_pipeline`, so GET the MR.
    async fn sweep_one(&self, repo: &RepoId, pid: u64, iid: u64) -> Result<(Vec<Event>, String)> {
        #[derive(Deserialize)]
        struct Job {
            name: String,
            web_url: String,
            stage: String,
        }
        let mr: Mr = self.api.get(&format!("/projects/{pid}/merge_requests/{iid}")).await?;
        let mr = &mr;
        let sha = mr.sha.clone().unwrap_or_default();
        let mut out = Vec::new();
        let ev = |trigger: Trigger, body: String| {
            let id = format!("gl-{}-{}-{}!{}-{sha}", self.host, crate::state::enum_str(&trigger), repo.path, mr.iid);
            Event::state(id, repo, mr.iid, trigger, &self.me, body)
        };
        if mr.has_conflicts {
            out.push(ev(Trigger::MergeConflict, format!("This MR has merge conflicts with `{}`.", mr.target_branch)));
        }
        if let Some(p) = mr.head_pipeline.as_ref().filter(|p| p.status == "failed") {
            let jobs: Vec<Job> =
                self.api.get(&format!("/projects/{}/pipelines/{}/jobs?scope[]=failed&per_page=50", mr.project_id, p.id)).await?;
            let mut body = String::from("CI pipeline failed:\n");
            for j in jobs {
                let _ = writeln!(body, "- {} (stage {}) {}", j.name, j.stage, j.web_url);
            }
            out.push(ev(Trigger::CiFailure, body));
        }
        let status = match mr.head_pipeline.as_ref().map(|p| p.status.as_str()) {
            Some("failed") => "red",
            Some("success") => "green",
            _ => "",
        };
        let conflict = if mr.has_conflicts { "conflict," } else { "" };
        Ok((out, format!("{conflict}{status}")))
    }

    pub async fn pr(&self, repo: &RepoId, iid: u64) -> Result<PrInfo> {
        #[derive(Deserialize)]
        struct Proj {
            ssh_url_to_repo: String,
            http_url_to_repo: String,
        }
        let p = enc(&repo.path);
        let m: Mr = self.api.get(&format!("/projects/{p}/merge_requests/{iid}")).await?;
        let src: Proj = self.api.get(&format!("/projects/{}", m.source_project_id)).await?;
        Ok(PrInfo {
            number: m.iid,
            title: m.title,
            author: m.author.username,
            state: match m.state.as_str() {
                "merged" => PrState::Merged,
                "closed" | "locked" => PrState::Closed,
                _ => PrState::Open,
            },
            head_ref: m.source_branch,
            head_sha: m.sha.unwrap_or_default(),
            head_repo_ssh: src.ssh_url_to_repo,
            head_repo_https: src.http_url_to_repo,
            base_ref: m.target_branch,
            draft: m.draft,
            body: m.description.unwrap_or_default(),
        })
    }

    pub async fn open_items(&self, repo: &RepoId, target: Target) -> Result<Vec<super::Listed>> {
        #[derive(Deserialize)]
        struct Item {
            iid: u64,
            title: String,
            author: User,
            created_at: String,
            #[serde(default)]
            labels: Vec<String>,
        }
        let kind = if target == Target::Pr { "merge_requests" } else { "issues" };
        let path = format!("/projects/{}/{kind}?state=opened&per_page=100", enc(&repo.path));
        let items: Vec<Item> = self.api.get_all(&path, super::LIST_PAGES).await?;
        Ok(items
            .into_iter()
            .map(|i| super::Listed { number: i.iid, title: i.title, author: i.author.username, created_at: i.created_at, labels: i.labels })
            .collect())
    }

    pub async fn issue(&self, repo: &RepoId, iid: u64) -> Result<super::IssueInfo> {
        #[derive(Deserialize)]
        struct I {
            title: String,
            description: Option<String>,
            author: User,
        }
        let i: I = self.api.get(&format!("/projects/{}/issues/{iid}", enc(&repo.path))).await?;
        Ok(super::IssueInfo { title: i.title, body: i.description.unwrap_or_default(), author: i.author.username })
    }

    pub async fn reply(&self, repo: &RepoId, target: Target, n: u64, to: Option<&Event>, body: &str) -> Result<()> {
        let p = enc(&repo.path);
        let kind = if target == Target::Issue { "issues" } else { "merge_requests" };
        let path = match to.and_then(|e| e.thread.as_deref()) {
            Some(d) if target == Target::Pr => format!("/projects/{p}/merge_requests/{n}/discussions/{d}/notes"),
            _ => format!("/projects/{p}/{kind}/{n}/notes"),
        };
        self.api.send(Method::POST, &path, &json!({ "body": body })).await?;
        Ok(())
    }

    pub async fn resolve(&self, repo: &RepoId, n: u64, discussion: &str) -> Result<()> {
        let p = enc(&repo.path);
        self.api.send(Method::PUT, &format!("/projects/{p}/merge_requests/{n}/discussions/{discussion}?resolved=true"), &json!({})).await?;
        Ok(())
    }

    pub async fn open_draft(&self, repo: &RepoId, head: &str, base: &str, title: &str, body: &str) -> Result<String> {
        let p = enc(&repo.path);
        let v = self
            .api
            .send(
                Method::POST,
                &format!("/projects/{p}/merge_requests"),
                &json!({ "source_branch": head, "target_branch": base, "title": format!("Draft: {title}"), "description": body }),
            )
            .await?;
        Ok(v["web_url"].as_str().unwrap_or_default().to_string())
    }

    pub async fn controls(&self, repo: &RepoId, target: Target, iid: u64, since: &str) -> Result<Vec<super::ControlComment>> {
        let kind = if target == Target::Issue { "issues" } else { "merge_requests" };
        let notes: Vec<Note> =
            self.api.get_all(&format!("/projects/{}/{kind}/{iid}/notes?per_page=100&sort=desc", enc(&repo.path)), 3).await?;
        Ok(notes
            .into_iter()
            .rev()
            .filter(|n| !n.system)
            .filter_map(|n| {
                let control = super::control_of(self.me_id, n.author.id, false, &n.body, &n.created_at, &n.updated_at, since)?;
                Some(super::ControlComment { id: n.id.to_string(), control })
            })
            .collect())
    }

    pub async fn ack(&self, repo: &RepoId, target: Target, iid: u64, note: &str) -> Result<()> {
        let kind = if target == Target::Issue { "issues" } else { "merge_requests" };
        let path = format!("/projects/{}/{kind}/{iid}/notes/{note}/award_emoji?name=thumbsup", enc(&repo.path));
        self.api.send(Method::POST, &path, &json!({})).await?;
        Ok(())
    }

    pub async fn in_team(&self, group: &str, user: &str) -> Result<bool> {
        let fetch = async {
            let members: Vec<User> = self.api.get_every(&format!("/groups/{}/members/all?per_page=100", enc(group)), 100).await?;
            Ok(members.into_iter().map(|u| u.username).collect())
        };
        self.groups.member(group, user, fetch).await
    }
}
