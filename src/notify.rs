//! Notifications: the desktop popup (native notification center) plus the webhook and command
//! targets in `[[notifications.targets]]`. Best effort: failures are only logged.

use crate::config::{Notifications, NotifyTarget, TargetType, WebhookFormat};
use crate::forge::{ForgeKind, RepoId, Target};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt::Write as _;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    NeedsApproval,
    Failed,
    Done,
    NewAuthor,
}

/// What happened. `json` webhooks and commands get exactly this, as JSON.
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub kind: Kind,
    /// Short headline: `owner/repo#7`, or e.g. `restarted`.
    pub title: String,
    pub summary: String,
    pub repo: Option<String>,
    pub number: Option<u64>,
    /// The PR/MR or issue title (untrusted text).
    pub pr_title: Option<String>,
    pub url: Option<String>,
    pub job: Option<i64>,
}

impl Event {
    pub fn new(kind: Kind, title: &str, summary: &str) -> Self {
        Self { kind, title: title.into(), summary: summary.into(), repo: None, number: None, pr_title: None, url: None, job: None }
    }
    /// About one PR/MR or issue, with a link to it.
    pub fn about(kind: Kind, repo: &RepoId, target: Target, number: u64, summary: &str) -> Self {
        Self {
            repo: Some(repo.path.clone()),
            number: Some(number),
            url: Some(web_url(repo, target, number)),
            ..Self::new(kind, &format!("{repo}#{number}"), summary)
        }
    }
}

/// The browser URL of a PR/MR or issue.
pub fn web_url(repo: &RepoId, target: Target, n: u64) -> String {
    let what = match (repo.forge, target) {
        (ForgeKind::GitHub, Target::Pr) => "pull",
        (ForgeKind::GitLab, Target::Pr) => "-/merge_requests",
        (ForgeKind::GitLab, Target::Issue) => "-/issues",
        (ForgeKind::Forgejo, Target::Pr) => "pulls",
        (ForgeKind::GitHub | ForgeKind::Forgejo, Target::Issue) => "issues",
    };
    format!("https://{}/{}/{what}/{n}", repo.host, repo.path)
}

fn desktop_on(n: &Notifications, kind: Kind) -> bool {
    match kind {
        Kind::NeedsApproval => n.needs_approval,
        Kind::Failed => n.failed,
        Kind::Done => n.done,
        Kind::NewAuthor => n.new_author,
    }
}

/// Does `t` want events of this kind? (No `kinds` = all.)
pub fn wants(t: &NotifyTarget, kind: Kind) -> bool {
    t.kinds.is_empty() || t.kinds.contains(&kind)
}

/// Whether the desktop gets `kind`, and which targets do. `only` is the repo's (or the active
/// window's) `notify` list; `None` = everywhere.
pub fn route<'a>(n: &'a Notifications, only: Option<&[String]>, kind: Kind) -> (bool, Vec<&'a NotifyTarget>) {
    let picked = |name: &str| only.is_none_or(|o| o.iter().any(|x| x == name));
    (picked("desktop") && desktop_on(n, kind), n.targets.iter().filter(|t| picked(&t.name) && wants(t, kind)).collect())
}

/// Send `ev` to the desktop and the targets [`route`] picks. Never blocks: the popup runs on
/// its own thread, targets as tasks on the current tokio runtime.
pub fn send(n: &Notifications, only: Option<&[String]>, ev: &Event) {
    if std::env::var_os("NIGHTPORTER_NO_NOTIFY").is_some() {
        return;
    }
    let (desk, targets) = route(n, only, ev.kind);
    if desk {
        // notify-rust can block (D-Bus / NSUserNotification); keep it off the async runtime.
        let ev = ev.clone();
        std::thread::spawn(move || desktop(&ev));
    }
    let targets: Vec<NotifyTarget> = targets.into_iter().cloned().collect();
    if targets.is_empty() {
        return;
    }
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        tracing::debug!("no async runtime; notification targets skipped");
        return;
    };
    for t in targets {
        let ev = ev.clone();
        rt.spawn(async move {
            if let Err(e) = deliver(&t, &ev).await {
                tracing::warn!("notification target {:?}: {e:#}", t.name);
            }
        });
    }
}

fn popup(ev: &Event) -> notify_rust::Notification {
    let body = match ev.pr_title.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => format!("{t}\n{}", ev.summary),
        None => ev.summary.clone(),
    };
    let mut n = notify_rust::Notification::new();
    n.summary(&format!("Night Porter: {}", ev.title)).body(&body).appname("Night Porter");
    n
}

/// Show the popup. On Linux, clicking it opens the PR in the browser. macOS can't do that here:
/// notify-rust only reports clicks while the main run loop runs, which the daemon doesn't do.
fn desktop(ev: &Event) {
    #[allow(unused_mut, reason = "only mutated on Linux")]
    let mut n = popup(ev);
    #[cfg(all(unix, not(target_os = "macos")))]
    if ev.url.is_some() {
        n.action("default", "Open");
    }
    match n.show() {
        Err(e) => tracing::debug!("notification failed: {e}"),
        // ponytail: this thread waits until the popup is clicked or closed; servers that keep
        // popups forever keep the thread too. Fine at a few notifications a day.
        #[cfg(all(unix, not(target_os = "macos")))]
        Ok(h) => {
            if let Some(url) = ev.url.clone() {
                h.wait_for_action(|a| {
                    if a == "default" {
                        let _ = std::process::Command::new("xdg-open").arg(&url).status();
                    }
                });
            }
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        Ok(_) => {}
    }
}

/// Send one event to one target and wait for it (10s limit).
pub async fn deliver(t: &NotifyTarget, ev: &Event) -> Result<()> {
    let limit = Duration::from_secs(10);
    match t.kind {
        TargetType::Webhook => {
            let url = match (&t.url, &t.url_env) {
                (Some(u), _) => u.clone(),
                (None, Some(var)) => std::env::var(var).with_context(|| format!("environment variable {var} is not set"))?,
                (None, None) => bail!("no url"),
            };
            let (headers, body, ctype) = payload(t.format, ev);
            let mut r = crate::http::plain().post(url).header(reqwest::header::CONTENT_TYPE, ctype).body(body);
            for (k, v) in headers {
                r = r.header(k, v);
            }
            // `without_url`: the URL is often the secret.
            let resp = r.send().await.map_err(|e| anyhow::Error::new(e.without_url()))?;
            if !resp.status().is_success() {
                bail!("HTTP {}", resp.status());
            }
        }
        TargetType::Command => {
            use std::process::Stdio;
            use tokio::io::AsyncWriteExt;
            let (bin, args) = t.command.split_first().context("empty command")?;
            let mut child = tokio::process::Command::new(bin)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .with_context(|| format!("run {bin}"))?;
            let mut stdin = child.stdin.take().context("no stdin")?;
            let input = serde_json::to_vec(ev)?;
            let out = tokio::time::timeout(limit, async move {
                // A command that doesn't read stdin is fine; its exit status is what counts.
                let _ = stdin.write_all(&input).await;
                drop(stdin);
                child.wait_with_output().await
            })
            .await
            .context("timed out after 10s")??;
            if !out.status.success() {
                let err: String = String::from_utf8_lossy(&out.stderr).chars().take(200).collect();
                bail!("{bin} exited with {}: {}", out.status, err.trim());
            }
        }
    }
    Ok(())
}

/// Extra headers, body and content type of a webhook request in `format`.
pub fn payload(format: WebhookFormat, ev: &Event) -> (Vec<(&'static str, String)>, String, &'static str) {
    const JSON: &str = "application/json";
    let head = format!("Night Porter: {}", ev.title);
    let pr = ev.pr_title.as_deref().filter(|t| !t.is_empty());
    match format {
        WebhookFormat::Json => (vec![], serde_json::to_string(ev).unwrap_or_default(), JSON),
        WebhookFormat::Slack => {
            // Slack mrkdwn: escape &, <, > so a PR title can't inject links or @channel.
            let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            let mut text = format!("*{}*\n{}", esc(&head), esc(&ev.summary));
            if let Some(u) = &ev.url {
                let label = pr.map_or_else(|| ev.title.clone(), |t| format!("{}: {t}", ev.title));
                let _ = write!(text, "\n<{u}|{}>", esc(&label));
            }
            (vec![], json!({ "text": text }).to_string(), JSON)
        }
        WebhookFormat::Discord => {
            // Escape Markdown, so a PR title can't render as a masked link or fake formatting.
            let esc = |s: &str| {
                s.chars().fold(String::new(), |mut o, c| {
                    if "\\*_~`|[]()<>#-".contains(c) {
                        o.push('\\');
                    }
                    o.push(c);
                    o
                })
            };
            let mut text = format!("**{}**\n{}", esc(&head), esc(&ev.summary));
            if let Some(t) = pr {
                text.push('\n');
                text.push_str(&esc(t));
            }
            if let Some(u) = &ev.url {
                text.push('\n');
                text.push_str(u);
            }
            // Discord's limit is 2000 characters.
            let text: String = text.chars().take(1900).collect();
            // No pings, whatever the text says.
            (vec![], json!({ "content": text, "allowed_mentions": { "parse": [] } }).to_string(), JSON)
        }
        WebhookFormat::Ntfy => {
            let tag = match ev.kind {
                Kind::NeedsApproval => "bell",
                Kind::Failed => "warning",
                Kind::Done => "white_check_mark",
                Kind::NewAuthor => "bust_in_silhouette",
            };
            // Header values must be ASCII; the full text goes in the body.
            let ascii = |s: &str| s.chars().map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '?' }).collect::<String>();
            let mut h = vec![("Title", ascii(&head)), ("Tags", tag.to_string())];
            if let Some(u) = &ev.url {
                h.push(("Click", ascii(u)));
            }
            let body = pr.map_or_else(|| ev.summary.clone(), |t| format!("{t}\n{}", ev.summary));
            (h, body, "text/plain; charset=utf-8")
        }
    }
}

/// `nightporter notify test [target]`: send a sample event to one target (`desktop` for the
/// popup) or to all of them, ignoring `kinds`, and report each result.
pub async fn test(n: &Notifications, name: Option<&str>) -> Result<String> {
    let ev = Event {
        pr_title: Some("Example pull request".into()),
        url: Some("https://github.com/night-porter/nightporter".into()),
        ..Event::about(
            Kind::NeedsApproval,
            &RepoId::new(ForgeKind::GitHub, "github.com", "night-porter/nightporter"),
            Target::Pr,
            1,
            "Test notification from `nightporter notify test`",
        )
    };
    let targets: Vec<&NotifyTarget> = n.targets.iter().filter(|t| name.is_none_or(|x| x == t.name)).collect();
    if let Some(x) = name
        && x != "desktop"
        && targets.is_empty()
    {
        let names: Vec<&str> = std::iter::once("desktop").chain(n.targets.iter().map(|t| t.name.as_str())).collect();
        bail!("no notification target {x:?}; there are: {}", names.join(", "));
    }
    let (mut out, mut failed) = (String::new(), 0);
    if name.is_none_or(|x| x == "desktop") {
        match popup(&ev).show() {
            Ok(_) => out.push_str("desktop: sent (check your notification center)\n"),
            Err(e) => {
                failed += 1;
                let _ = writeln!(out, "desktop: failed: {e}");
            }
        }
    }
    for t in targets {
        match deliver(t, &ev).await {
            Ok(()) => {
                let _ = writeln!(out, "{}: sent", t.name);
            }
            Err(e) => {
                failed += 1;
                let _ = writeln!(out, "{}: failed: {e:#}", t.name);
            }
        }
    }
    if failed > 0 {
        bail!("{out}{failed} failed");
    }
    Ok(out.trim_end().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev() -> Event {
        Event {
            pr_title: Some("Fix <!channel> & @everyone".into()),
            job: Some(3),
            ..Event::about(Kind::Failed, &RepoId::new(ForgeKind::GitLab, "gitlab.com", "grp/proj"), Target::Pr, 7, "checks need you")
        }
    }

    fn target(kinds: &[Kind]) -> NotifyTarget {
        let mut t: NotifyTarget = toml::from_str("name = \"x\"\ntype = \"command\"\ncommand = [\"true\"]").unwrap();
        t.kinds = kinds.to_vec();
        t
    }

    #[test]
    fn urls() {
        let gh = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        assert_eq!(web_url(&gh, Target::Pr, 7), "https://github.com/o/r/pull/7");
        assert_eq!(web_url(&gh, Target::Issue, 7), "https://github.com/o/r/issues/7");
        assert_eq!(ev().url.as_deref(), Some("https://gitlab.com/grp/proj/-/merge_requests/7"));
    }

    #[test]
    fn kinds_filter() {
        assert!(wants(&target(&[]), Kind::Done), "no kinds = all");
        assert!(wants(&target(&[Kind::Failed, Kind::NeedsApproval]), Kind::Failed));
        assert!(!wants(&target(&[Kind::Failed]), Kind::Done));
    }

    #[test]
    fn routing_follows_notify_lists() {
        fn names((d, ts): (bool, Vec<&NotifyTarget>)) -> (bool, Vec<&str>) {
            (d, ts.into_iter().map(|t| t.name.as_str()).collect())
        }
        let mut n = Notifications::default();
        for (name, kinds) in [("phone", vec![Kind::Failed]), ("team", vec![])] {
            let mut t = target(&kinds);
            t.name = name.into();
            n.targets.push(t);
        }
        assert_eq!(names(route(&n, None, Kind::Failed)), (true, vec!["phone", "team"]));
        assert_eq!(names(route(&n, None, Kind::Done)), (true, vec!["team"]), "phone only wants failed");
        let only = ["phone".to_string()];
        assert_eq!(names(route(&n, Some(&only), Kind::Failed)), (false, vec!["phone"]));
        assert_eq!(names(route(&n, Some(&["desktop".into()]), Kind::Done)), (true, vec![]));
        n.done = false;
        assert_eq!(names(route(&n, Some(&["desktop".into()]), Kind::Done)), (false, vec![]), "the toggle still applies");
    }

    #[test]
    fn json_payload() {
        let (h, body, ct) = payload(WebhookFormat::Json, &ev());
        assert!(h.is_empty() && ct == "application/json");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["kind"], "failed");
        assert_eq!(v["repo"], "grp/proj");
        assert_eq!(v["number"], 7);
        assert_eq!(v["job"], 3);
        assert_eq!(v["url"], "https://gitlab.com/grp/proj/-/merge_requests/7");
    }

    #[test]
    fn slack_payload_escapes_and_links() {
        let (_, body, _) = payload(WebhookFormat::Slack, &ev());
        let text = serde_json::from_str::<serde_json::Value>(&body).unwrap()["text"].as_str().unwrap().to_string();
        assert!(text.starts_with("*Night Porter: grp/proj#7*\nchecks need you\n"));
        assert!(text.contains("<https://gitlab.com/grp/proj/-/merge_requests/7|grp/proj#7: Fix &lt;!channel&gt; &amp; @everyone>"));
    }

    #[test]
    fn discord_payload_never_pings() {
        let (_, body, _) = payload(WebhookFormat::Discord, &ev());
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["allowed_mentions"]["parse"], json!([]));
        let c = v["content"].as_str().unwrap();
        assert!(c.starts_with("**Night Porter: grp/proj\\#7**") && c.ends_with("/-/merge_requests/7"), "{c}");
        let mut e = ev();
        e.pr_title = Some("[Approve job](https://evil.example)".into());
        let (_, body, _) = payload(WebhookFormat::Discord, &e);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["content"].as_str().unwrap().contains("\\[Approve job\\]\\(https://evil.example\\)"));
    }

    #[test]
    fn ntfy_payload_headers() {
        let mut e = ev();
        e.title = "grp/proj#7 ✓".into();
        let (h, body, ct) = payload(WebhookFormat::Ntfy, &e);
        assert_eq!(ct, "text/plain; charset=utf-8");
        assert!(h.contains(&("Title", "Night Porter: grp/proj#7 ?".into())), "headers stay ASCII");
        assert!(h.contains(&("Click", "https://gitlab.com/grp/proj/-/merge_requests/7".into())));
        assert!(h.contains(&("Tags", "warning".into())));
        assert_eq!(body, "Fix <!channel> & @everyone\nchecks need you");
    }

    #[tokio::test]
    async fn command_gets_json_on_stdin() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("ev.json");
        let mut t = target(&[]);
        t.command = vec!["sh".into(), "-c".into(), format!("cat > '{}'", out.display())];
        deliver(&t, &ev()).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(v["summary"], "checks need you");
        t.command = vec!["false".into()];
        assert!(deliver(&t, &ev()).await.is_err());
    }

    #[test]
    fn targets_are_validated() {
        let ok =
            "[[notifications.targets]]\nname = \"phone\"\ntype = \"webhook\"\nurl_env = \"NTFY\"\nformat = \"ntfy\"\nkinds = [\"failed\"]";
        crate::config::Config::parse(ok).unwrap();
        for bad in [
            "[[notifications.targets]]\nname = \"desktop\"\ntype = \"command\"\ncommand = [\"x\"]",
            "[[notifications.targets]]\nname = \"a\"\ntype = \"webhook\"",
            "[[notifications.targets]]\nname = \"a\"\ntype = \"webhook\"\nurl = \"ftp://x\"",
            "[[notifications.targets]]\nname = \"a\"\ntype = \"command\"",
            "[[notifications.targets]]\nname = \"a\"\ntype = \"command\"\ncommand = [\"x\"]\nkinds = [\"sometimes\"]",
        ] {
            assert!(crate::config::Config::parse(bad).is_err(), "{bad}");
        }
    }
}
