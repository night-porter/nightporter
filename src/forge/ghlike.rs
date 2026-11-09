//! What GitHub and Forgejo/Gitea share: their REST APIs agree on PRs, issues, comments and
//! reactions under `/repos/{owner}/{name}`.

use super::{IssueInfo, Listed, PrInfo, PrState, RepoId};
use crate::http::Api;
use anyhow::Result;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub(super) struct Login {
    pub login: String,
}

#[derive(Deserialize)]
pub(super) struct Pull {
    number: u64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    pub user: Login,
    pub state: String,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    pub draft: bool,
    /// GitHub: `dirty` means conflicts.
    #[serde(default)]
    pub mergeable_state: Option<String>,
    /// Forgejo/Gitea (GitHub sends it too, `null` while computing).
    #[serde(default)]
    pub mergeable: Option<bool>,
    pub head: Head,
    pub base: Head,
}
#[derive(Deserialize)]
pub(super) struct Head {
    #[serde(rename = "ref")]
    pub name: String,
    #[serde(default)]
    pub sha: String,
    repo: Option<HeadRepo>,
}
#[derive(Deserialize)]
struct HeadRepo {
    ssh_url: String,
    clone_url: String,
}

impl Pull {
    fn info(self) -> PrInfo {
        let head_repo = self.head.repo.as_ref();
        PrInfo {
            number: self.number,
            title: self.title,
            author: self.user.login,
            state: match (self.state.as_str(), self.merged) {
                (_, true) => PrState::Merged,
                ("closed", _) => PrState::Closed,
                _ => PrState::Open,
            },
            head_repo_ssh: head_repo.map(|r| r.ssh_url.clone()).unwrap_or_default(),
            head_repo_https: head_repo.map(|r| r.clone_url.clone()).unwrap_or_default(),
            head_ref: self.head.name,
            head_sha: self.head.sha,
            base_ref: self.base.name,
            draft: self.draft,
            body: self.body.unwrap_or_default(),
        }
    }
}

/// An issue or PR in a list.
#[derive(Deserialize)]
pub(super) struct ListItem {
    number: u64,
    title: String,
    user: Login,
    created_at: String,
    #[serde(default)]
    labels: Vec<Name>,
    /// GitHub lists PRs among issues, marked with this.
    #[serde(default)]
    pub pull_request: Option<Value>,
}
#[derive(Deserialize)]
pub(super) struct Name {
    pub name: String,
}

impl ListItem {
    pub fn listed(self) -> Listed {
        Listed {
            number: self.number,
            title: self.title,
            author: self.user.login,
            created_at: self.created_at,
            labels: self.labels.into_iter().map(|l| l.name).collect(),
        }
    }
}

pub(super) async fn pull(api: &Api, repo: &str, n: u64) -> Result<Pull> {
    api.get(&format!("/repos/{repo}/pulls/{n}")).await
}

pub(super) async fn pr(api: &Api, repo: &RepoId, n: u64) -> Result<PrInfo> {
    Ok(pull(api, &repo.path, n).await?.info())
}

pub(super) async fn issue(api: &Api, repo: &RepoId, n: u64) -> Result<IssueInfo> {
    #[derive(Deserialize)]
    struct I {
        title: String,
        body: Option<String>,
        user: Login,
    }
    let i: I = api.get(&format!("/repos/{}/issues/{n}", repo.path)).await?;
    Ok(IssueInfo { title: i.title, body: i.body.unwrap_or_default(), author: i.user.login })
}

/// A top-level comment on a PR or issue.
pub(super) async fn comment(api: &Api, repo: &RepoId, n: u64, body: &str) -> Result<()> {
    api.send(Method::POST, &format!("/repos/{}/issues/{n}/comments", repo.path), &json!({ "body": body })).await.map(|_| ())
}

/// Thumbs-up on a comment.
pub(super) async fn ack(api: &Api, repo: &RepoId, comment: &str) -> Result<()> {
    api.send(Method::POST, &format!("/repos/{}/issues/comments/{comment}/reactions", repo.path), &json!({ "content": "+1" }))
        .await
        .map(|_| ())
}

/// Open a PR from `fields` (title, head, base, body, …). Returns its URL.
pub(super) async fn open_pr(api: &Api, repo: &RepoId, fields: &Value) -> Result<String> {
    let v = api.send(Method::POST, &format!("/repos/{}/pulls", repo.path), fields).await?;
    Ok(v["html_url"].as_str().unwrap_or_default().to_string())
}
