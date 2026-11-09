//! Thin JSON-over-HTTP helper shared by the forge clients, plus token lookup.

use crate::config::TokenSource;
use crate::forge::ForgeKind;
use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode, header};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt::Write as _;

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    pub base: String,
    token: SecretString,
    /// `Authorization: Bearer` (GitHub, Forgejo/Gitea) vs `PRIVATE-TOKEN` (GitLab).
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
            // Follow redirects only to the same scheme, host and port: GitLab's PRIVATE-TOKEN
            // header is not stripped on cross-host redirects the way `Authorization` is, and an
            // https→http hop would send it in the clear.
            .redirect(reqwest::redirect::Policy::custom(|a| {
                let same = a.previous().first().is_some_and(|p| same_origin(p, a.url()));
                if same && a.previous().len() < 5 { a.follow() } else { a.stop() }
            }))
            .build()?;
        Ok(Self { http, base: base.trim_end_matches('/').into(), token, gitlab })
    }

    pub fn is_ours(&self, url: &str) -> bool {
        match (reqwest::Url::parse(&self.base), reqwest::Url::parse(url)) {
            (Ok(b), Ok(u)) => same_origin(&b, &u),
            _ => false,
        }
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

    /// GET every page (follows `Link: <…>; rel="next"`, which GitHub and GitLab both send), up to
    /// `max_pages`. Errors on any page fail the whole call so callers don't advance cursors.
    pub async fn get_all<T: DeserializeOwned>(&self, path: &str, max_pages: usize) -> Result<Vec<T>> {
        self.pages(path, max_pages, false).await
    }

    /// Like [`Api::get_all`], but an error unless every page was read: for lists where a missing
    /// entry fails open (team members for `deny_teams`).
    pub async fn get_every<T: DeserializeOwned>(&self, path: &str, max_pages: usize) -> Result<Vec<T>> {
        self.pages(path, max_pages, true).await
    }

    async fn pages<T: DeserializeOwned>(&self, path: &str, max_pages: usize, complete: bool) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next = Some(path.to_string());
        for _ in 0..max_pages {
            let Some(p) = next.take() else { break };
            match self.get_cond::<Vec<T>>(&p, None).await? {
                Got::Fresh(v, h) => {
                    out.extend(v);
                    // Only follow `next` links back to our own API: they carry the token.
                    next = next_link(&h);
                    if let Some(u) = next.as_ref().filter(|u| !self.is_ours(u)) {
                        anyhow::ensure!(!complete, "{path}: next page is on another server ({u}); not following it");
                        next = None;
                    }
                }
                Got::NotModified => break,
            }
        }
        anyhow::ensure!(!complete || next.is_none(), "{path}: more than {max_pages} pages");
        Ok(out)
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

/// A client with no credentials, for notification webhooks: short timeout, few redirects.
pub fn plain() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("nightporter/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .unwrap_or_default()
    })
    .clone()
}

/// A forge API answered with an error status. Match on [`status`], never on the message: the
/// body is the server's text and can say anything.
#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    msg: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for HttpError {}

/// The HTTP status of a failed API call, if that's what failed.
pub fn status(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<HttpError>().map(|h| h.status)
}

async fn check(resp: reqwest::Response, path: &str) -> Result<reqwest::Response> {
    let s = resp.status();
    if s.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let body: String = body.chars().take(300).collect();
    Err(HttpError { status: s.as_u16(), msg: format!("{path}: HTTP {s}: {body}") }.into())
}

fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
}

/// The `rel="next"` URL from a `Link` header.
pub fn next_link(h: &header::HeaderMap) -> Option<String> {
    let link = h.get(header::LINK)?.to_str().ok()?;
    link.split(',').find(|p| p.contains("rel=\"next\"")).and_then(|p| Some(p.split_once('<')?.1.split_once('>')?.0.to_string()))
}

/// Get a token: from `gh`/`glab`/`tea` (they refresh it themselves) or from the OS keychain.
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
                // ponytail: blocking, but it runs once per account at daemon start.
                ForgeKind::Forgejo => return tea_token(host),
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

/// `tea` (the Gitea CLI, also works with Forgejo) has no "print token" command, but it is a git
/// credential helper: `tea login helper get` answers `password=<token>` for a host, refreshing
/// OAuth logins first. Its output is never logged; errors only say what to run.
pub fn tea_token(host: &str) -> Result<SecretString> {
    use std::io::Write;
    let mut child = std::process::Command::new("tea")
        .args(["login", "helper", "get"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("run `tea` (is it installed?)")?;
    if let Some(mut i) = child.stdin.take() {
        i.write_all(format!("protocol=https\nhost={host}\n\n").as_bytes())?;
    }
    let out = child.wait_with_output()?;
    let token = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("password="))
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if !out.status.success() || token.is_empty() {
        bail!(
            "`tea` has no login for {host}; run `tea login add --url https://{host}`, or store a token with `nightporter token set forgejo {host}` and set `token = \"keyring\"`"
        );
    }
    Ok(token.into())
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
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn redirects_and_next_links_stay_on_origin() {
        let u = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(super::same_origin(&u("https://gitlab.example.com/api/v4/x"), &u("https://gitlab.example.com/api/v4/y?page=2")));
        assert!(super::same_origin(&u("https://h.example/a"), &u("https://h.example:443/b")));
        assert!(!super::same_origin(&u("https://h.example/a"), &u("http://h.example/a")), "no https→http");
        assert!(!super::same_origin(&u("https://h.example/a"), &u("https://h.example:8443/a")), "no port change");
        assert!(!super::same_origin(&u("https://h.example/a"), &u("https://evil.example/a")));
    }

    #[test]
    fn link_next() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::LINK,
            r#"<https://api.github.com/x?page=2>; rel="next", <https://api.github.com/x?page=9>; rel="last""#.parse().unwrap(),
        );
        assert_eq!(super::next_link(&h).as_deref(), Some("https://api.github.com/x?page=2"));
        h.insert(reqwest::header::LINK, r#"<https://x?page=1>; rel="prev""#.parse().unwrap());
        assert_eq!(super::next_link(&h), None);
    }

    #[test]
    fn enc() {
        assert_eq!(super::enc("group/sub proj"), "group%2Fsub%20proj");
    }
}
