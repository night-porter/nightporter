//! Thin JSON-over-HTTP helper shared by the forge clients, plus token lookup.

use crate::config::TokenSource;
use crate::forge::ForgeKind;
use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode, header};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    pub base: String,
    token: SecretString,
    /// `Authorization: Bearer` (GitHub) vs `PRIVATE-TOKEN` (GitLab).
    gitlab: bool,
}

pub enum Got<T> {
    Fresh(T, header::HeaderMap),
    NotModified,
}

impl Api {
    pub fn new(base: &str, token: SecretString, gitlab: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("nightporter/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            // Follow redirects only on the same host: GitLab's PRIVATE-TOKEN header is not
            // stripped on cross-host redirects the way `Authorization` is.
            .redirect(reqwest::redirect::Policy::custom(|a| {
                let same = a.previous().first().and_then(|u| u.host_str()) == a.url().host_str();
                if same && a.previous().len() < 5 { a.follow() } else { a.stop() }
            }))
            .build()?;
        Ok(Self { http, base: base.trim_end_matches('/').into(), token, gitlab })
    }

    fn req(&self, m: Method, path: &str) -> reqwest::RequestBuilder {
        let url = if path.starts_with("http") { path.to_string() } else { format!("{}{path}", self.base) };
        let r = self.http.request(m, url);
        if self.gitlab {
            r.header("PRIVATE-TOKEN", self.token.expose_secret())
        } else {
            r.bearer_auth(self.token.expose_secret())
                .header(header::ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        match self.get_cond(path, None).await? {
            Got::Fresh(v, _) => Ok(v),
            Got::NotModified => bail!("unexpected 304 for {path}"),
        }
    }

    /// GET with `If-Modified-Since`. 304s don't count against GitHub's rate limit.
    pub async fn get_cond<T: DeserializeOwned>(&self, path: &str, since: Option<&str>) -> Result<Got<T>> {
        let mut r = self.req(Method::GET, path);
        if let Some(s) = since {
            r = r.header(header::IF_MODIFIED_SINCE, s);
        }
        let resp = r.send().await.with_context(|| format!("GET {path}"))?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(Got::NotModified);
        }
        let headers = resp.headers().clone();
        let v = check(resp, path).await?.json().await.with_context(|| format!("decode {path}"))?;
        Ok(Got::Fresh(v, headers))
    }

    pub async fn send(&self, m: Method, path: &str, body: &Value) -> Result<Value> {
        let resp = self.req(m.clone(), path).json(body).send().await.with_context(|| format!("{m} {path}"))?;
        let resp = check(resp, path).await?;
        let text = resp.text().await?;
        Ok(if text.is_empty() { Value::Null } else { serde_json::from_str(&text)? })
    }
}

async fn check(resp: reqwest::Response, path: &str) -> Result<reqwest::Response> {
    let s = resp.status();
    if s.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let body: String = body.chars().take(300).collect();
    bail!("{path}: HTTP {s}: {body}")
}

/// Get a token: from `gh`/`glab` (they refresh it themselves) or from the OS keychain.
/// Never read from config files or environment of the agent.
pub async fn token(forge: ForgeKind, host: &str, source: TokenSource) -> Result<SecretString> {
    if let Some(t) = std::env::var("NIGHTPORTER_TEST_TOKEN").ok().filter(|_| cfg!(debug_assertions)) {
        return Ok(t.into());
    }
    match source {
        TokenSource::Keyring => {
            let e = keyring::Entry::new("nightporter", &format!("{}:{host}", forge.as_str()))?;
            Ok(e.get_password()
                .with_context(|| format!("no token in keychain for {host}; run `nightporter token set {} {host}`", forge.as_str()))?
                .into())
        }
        TokenSource::Cli => {
            let (bin, args): (&str, Vec<&str>) = match forge {
                ForgeKind::GitHub => ("gh", vec!["auth", "token", "--hostname", host]),
                ForgeKind::GitLab => ("glab", vec!["config", "get", "token", "--host", host]),
            };
            let out =
                tokio::process::Command::new(bin).args(&args).output().await.with_context(|| format!("run `{bin}` (is it installed?)"))?;
            let t = String::from_utf8(out.stdout)?.trim().to_string();
            if !out.status.success() || t.is_empty() {
                bail!("`{bin} {}` gave no token; log in with `{bin} auth login --hostname {host}`", args.join(" "));
            }
            Ok(t.into())
        }
    }
}

pub fn store_token(forge: ForgeKind, host: &str, token: &str) -> Result<()> {
    keyring::Entry::new("nightporter", &format!("{}:{host}", forge.as_str()))?.set_password(token)?;
    Ok(())
}

/// Percent-encode a path segment (GitLab project ids are `group%2Fname`).
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn enc() {
        assert_eq!(super::enc("group/sub proj"), "group%2Fsub%20proj");
    }
}
