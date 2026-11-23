//! Jira Cloud through Atlassian's `acli`. `acli` has one current site for the whole machine (your
//! terminal's too), so every call switches to its source's site and back, under a file lock that
//! the daemon and the CLI share.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A claimed Jira issue as a job knows it (`JobMeta::jira`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JiraRef {
    pub site: String,
    pub key: String,
    /// The source's `paths`: the part of the repo the work belongs in.
    #[serde(default)]
    pub paths: Vec<String>,
}

impl JiraRef {
    pub fn url(&self) -> String {
        format!("https://{}/browse/{}", self.site, self.key)
    }

    /// `nightporter/jira/acme/ABC-123`: the key links it in Jira (its GitHub/GitLab app finds it
    /// anywhere in the name), and the site keeps two sites' ABC-123 on one repo apart.
    pub fn branch(&self) -> String {
        let site = self.site.split('.').next().unwrap_or_default().to_lowercase();
        format!("nightporter/jira/{site}/{}", self.key)
    }
}

/// One issue from a search or a view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Issue {
    pub id: u64,
    pub key: String,
    pub summary: String,
    /// Its status is in the Done category.
    pub done: bool,
    /// Account ids; empty when unassigned.
    pub assignee: String,
    /// Who created it: unlike the reporter, nobody can change that afterwards.
    pub creator: String,
    pub creator_name: String,
    /// Who wrote its comments (a view only), by account id.
    pub commenters: Vec<String>,
    /// Description, then the comments (a view only).
    pub text: String,
}

impl Issue {
    fn parse(v: &Value) -> Result<Self> {
        let f = &v["fields"];
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let mut text = adf_text(&f["description"]);
        let mut commenters = Vec::new();
        for c in f["comment"]["comments"].as_array().into_iter().flatten() {
            let _ = write!(text, "\n\n--- comment by {} ---\n{}", s(&c["author"]["displayName"]), adf_text(&c["body"]));
            commenters.push(s(&c["author"]["accountId"]));
        }
        Ok(Self {
            id: s(&v["id"]).parse().with_context(|| format!("jira issue without a numeric id: {v}"))?,
            key: s(&v["key"]),
            summary: s(&f["summary"]),
            done: f["status"]["statusCategory"]["key"] == "done",
            assignee: s(&f["assignee"]["accountId"]),
            creator: s(&f["creator"]["accountId"]),
            creator_name: s(&f["creator"]["displayName"]),
            commenters,
            text: text.trim().to_string(),
        })
    }
}

#[derive(Clone)]
pub struct Acli {
    bin: PathBuf,
    lock: PathBuf,
    /// `acli`'s own config, where its logins are.
    config: PathBuf,
}

/// Tests: a fake `acli` per data dir.
#[cfg(test)]
pub static FAKES: std::sync::Mutex<Vec<(PathBuf, Acli)>> = std::sync::Mutex::new(Vec::new());

/// Tests: use the `acli` at `bin`, logged in per `config`, for jobs with this data dir.
#[cfg(test)]
pub fn install_fake(data_dir: &Path, bin: PathBuf, config: PathBuf) {
    let a = Acli { bin, lock: data_dir.join("acli.lock"), config };
    FAKES.lock().unwrap().push((data_dir.to_path_buf(), a));
}

impl Acli {
    pub fn new(paths: &crate::paths::Paths) -> Self {
        #[cfg(test)]
        if let Some((_, a)) = FAKES.lock().unwrap().iter().find(|(d, _)| *d == paths.data_dir) {
            return a.clone();
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let dir = home.join(".config/acli");
        // Next to acli's own config, not in the data dir: Night Porters with other homes
        // (`NIGHTPORTER_HOME`) share one acli, so they share the lock too. `paths` only picks a test fake.
        let _ = paths;
        Self { bin: "acli".into(), lock: dir.join("nightporter.lock"), config: dir.join("jira_config.yaml") }
    }

    /// Run `f` with this client off the async runtime (`acli` calls block, and wait on the lock).
    pub async fn call<T: Send + 'static>(&self, f: impl FnOnce(&Self) -> Result<T> + Send + 'static) -> Result<T> {
        let a = self.clone();
        tokio::task::spawn_blocking(move || f(&a)).await?
    }

    /// The sites `acli` is logged in to.
    pub fn sites(&self) -> Vec<String> {
        profiles_in(&self.config).into_iter().map(|(site, _)| site).collect()
    }

    /// Your account id on `site`: who `assigned` issues belong to, and whose text isn't a stranger's.
    pub fn account(&self, site: &str) -> Option<String> {
        profiles_in(&self.config).into_iter().find(|(s, _)| s == site).map(|(_, a)| a).filter(|a| !a.is_empty())
    }

    /// `acli jira <args>` on `site`, then back to the site that was current before. A command that
    /// went through is never reported as failed: a problem switching back is only logged, so a
    /// caller doesn't redo (or forget) what it did.
    pub fn run(&self, site: &str, args: &[&str]) -> Result<String> {
        if let Some(d) = self.lock.parent() {
            std::fs::create_dir_all(d)?;
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock)
            .with_context(|| format!("open {}", self.lock.display()))?;
        lock.lock().with_context(|| format!("lock {}", self.lock.display()))?;
        let before = self.current_site()?;
        if before != site {
            self.acli(&["auth", "switch", "--site", site])
                .with_context(|| format!("acli: can't switch to {site}; log in to it: acli jira auth login --site {site}"))?;
            // ponytail: something outside the lock (your terminal) can still switch in between; an
            // acli option per command, if it gets one, would close that.
            let now = self.current_site()?;
            if now != site {
                bail!("acli is on {now} right after switching to {site}; did something else switch it?");
            }
        }
        let out = self.acli(args);
        if before != site {
            let back = self.acli(&["auth", "switch", "--site", &before]).and_then(|_| self.current_site());
            let problem = match back {
                Ok(now) if now == before => None,
                Ok(now) => Some(format!("acli's site changed to {now} while Night Porter used it")),
                Err(e) => Some(format!("{e:#}")),
            };
            if let Some(p) = problem {
                let msg = format!("{p}; your terminal's acli may not be on {before}: acli jira auth switch --site {before}");
                match &out {
                    Ok(_) => tracing::warn!("{msg}"),
                    Err(e) => bail!("{e:#}; also {msg}"),
                }
            }
        }
        out
    }

    /// Up to `limit` issues matching `jql`, oldest first.
    pub fn search(&self, site: &str, jql: &str, limit: usize) -> Result<Vec<Issue>> {
        let jql = format!("{jql} ORDER BY created ASC");
        let out = self.run(
            site,
            &["workitem", "search", "--jql", &jql, "--fields", "summary,status,assignee,creator", "--limit", &limit.to_string(), "--json"],
        )?;
        let v: Value = serde_json::from_str(&out).with_context(|| format!("acli search on {site}: not JSON"))?;
        v.as_array().into_iter().flatten().map(Issue::parse).collect()
    }

    /// One issue with its description and comments.
    pub fn view(&self, site: &str, key: &str) -> Result<Issue> {
        let out =
            self.run(site, &["workitem", "view", key, "--fields", "summary,description,comment,status,assignee,creator", "--json"])?;
        Issue::parse(&serde_json::from_str(&out).with_context(|| format!("acli view {key}: not JSON"))?)
    }

    /// A comment written in Markdown, sent as Atlassian Document Format so it keeps its layout.
    /// Jira shows HTML as text, so `sign`'s hidden marker is dropped and its footer kept as words.
    pub fn comment(&self, site: &str, key: &str, markdown: &str) -> Result<()> {
        let md = markdown.replace(crate::prompts::FOOTER_MARK, "").replace("<sub>", "").replace("</sub>", "");
        self.run(site, &["workitem", "comment", "create", "--key", key, "--body", &md_to_adf(md.trim_end()).to_string()]).map(|_| ())
    }

    /// Assign to you (`on`) if it's still unassigned, or unassign it if it's still yours: never
    /// over someone who took it meanwhile. Check the issue afterwards to see which happened.
    pub fn assign(&self, site: &str, key: &str, on: bool) -> Result<()> {
        // It goes into JQL: only a real issue key.
        anyhow::ensure!(is_key(key), "not a Jira issue key: {key:?}");
        let (jql, who) = if on {
            (format!("key = {key} AND assignee is EMPTY"), ["--assignee", "@me"].as_slice())
        } else {
            (format!("key = {key} AND assignee = currentUser()"), ["--remove-assignee"].as_slice())
        };
        self.run(site, &[&["workitem", "assign", "--jql", jql.as_str(), "--yes"], who].concat()).map(|_| ())
    }

    /// The issues with these keys, for settling: one search when it can, else one view each (a
    /// key Jira no longer shows fails the whole search). An issue it can't read is left out.
    pub fn issues(&self, site: &str, keys: &[String]) -> Vec<Result<Issue>> {
        if keys.iter().all(|k| is_key(k)) {
            let jql = format!("key in ({})", keys.join(", "));
            if let Ok(found) = self.search(site, &jql, keys.len()) {
                return keys
                    .iter()
                    .map(|k| found.iter().find(|i| i.key == *k).cloned().with_context(|| format!("{k} isn't in Jira's answer")))
                    .collect();
            }
        }
        keys.iter().map(|k| self.view(site, k)).collect()
    }

    fn current_site(&self) -> Result<String> {
        let out =
            self.acli(&["auth", "status"]).context("acli isn't logged in to any Jira site: acli jira auth login --site <your-site>")?;
        out.lines()
            .find_map(|l| l.trim().strip_prefix("Site:").map(|s| s.trim().to_string()))
            .context("acli jira auth status: no Site line")
    }

    /// One `acli jira` command, killed after two minutes: a hung one holds the lock for everyone.
    fn acli(&self, args: &[&str]) -> Result<String> {
        let mut child = Command::new(&self.bin)
            .arg("jira")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("run {}", self.bin.display()))?;
        let what = args[..args.len().min(2)].join(" ");
        // Read both pipes while it runs, or a big reply (a view with its comments) fills one and hangs.
        let read = |p: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = p {
                    let _ = p.read_to_end(&mut buf);
                }
                buf
            })
        };
        let out = read(child.stdout.take().map(|p| Box::new(p) as _));
        let err = read(child.stderr.take().map(|p| Box::new(p) as _));
        let deadline = std::time::Instant::now() + ACLI_TIMEOUT;
        let status = loop {
            if let Some(st) = child.try_wait()? {
                break st;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("acli jira {what}: no answer in {}s", ACLI_TIMEOUT.as_secs());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let join = |h: std::thread::JoinHandle<Vec<u8>>| h.join().unwrap_or_default();
        let o = std::process::Output { status, stdout: join(out), stderr: join(err) };
        if !o.status.success() {
            // acli prints some errors (`✗ Error: ...`) on stdout.
            let err = [&o.stderr, &o.stdout].map(|b| String::from_utf8_lossy(b).trim().to_string()).join(" ");
            bail!("acli jira {what}: {}", err.trim().trim_start_matches('✗').trim());
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

const ACLI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// `ABC-123`: a project key, a dash, a number.
fn is_key(k: &str) -> bool {
    k.split_once('-').is_some_and(|(p, n)| {
        p.starts_with(|c: char| c.is_ascii_alphabetic())
            && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
    })
}

/// `(site, account_id)` per profile in `acli`'s config (`- site: x` starts one).
fn profiles_in(file: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for l in std::fs::read_to_string(file).unwrap_or_default().lines() {
        let l = l.trim();
        if let Some(site) = l.strip_prefix("- site:") {
            out.push((site.trim().to_string(), String::new()));
        } else if let (Some(a), Some(last)) = (l.strip_prefix("account_id:"), out.last_mut()) {
            last.1 = a.trim().to_string();
        }
    }
    out
}

/// Plain text of an Atlassian Document Format value, for the agent to read.
fn adf_text(v: &Value) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v["type"].as_str().unwrap_or_default() {
            "text" => {
                out.push_str(v["text"].as_str().unwrap_or_default());
                // A link's words alone would lose where it goes.
                let marks = v["marks"].as_array().into_iter().flatten();
                if let Some(href) = marks.filter(|m| m["type"] == "link").find_map(|m| m["attrs"]["href"].as_str()) {
                    let _ = write!(out, " ({href})");
                }
            }
            "inlineCard" | "blockCard" | "embedCard" => out.push_str(v["attrs"]["url"].as_str().unwrap_or_default()),
            "mention" => out.push_str(v["attrs"]["text"].as_str().unwrap_or_default()),
            "hardBreak" => out.push('\n'),
            "listItem" => out.push_str("- "),
            "codeBlock" => out.push_str("```\n"),
            _ => {}
        }
        for c in v["content"].as_array().into_iter().flatten() {
            walk(c, out);
        }
        match v["type"].as_str().unwrap_or_default() {
            "codeBlock" => out.push_str("\n```\n"),
            // A list item's paragraph ends its line.
            "paragraph" | "heading" => out.push('\n'),
            _ => {}
        }
    }
    let mut out = String::new();
    // Older issues may hold plain text.
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    walk(v, &mut out);
    out
}

/// Markdown (what the agent writes) as Atlassian Document Format: headings, `-`/`1.` lists,
/// fenced code, paragraphs with line breaks, and inline links, `code` and **bold**.
// ponytail: one list level, no tables or quotes; extend when replies need them.
pub fn md_to_adf(md: &str) -> Value {
    let mut blocks: Vec<Value> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut lines = md.lines().peekable();
    let flush = |para: &mut Vec<&str>, blocks: &mut Vec<Value>| {
        if !para.is_empty() {
            blocks.push(json!({ "type": "paragraph", "content": lines_inline(para) }));
            para.clear();
        }
    };
    while let Some(l) = lines.next() {
        let t = l.trim_start();
        if let Some(lang) = t.strip_prefix("```") {
            flush(&mut para, &mut blocks);
            let mut code = Vec::new();
            for c in lines.by_ref() {
                if c.trim_start().starts_with("```") {
                    break;
                }
                code.push(c);
            }
            let mut b = json!({ "type": "codeBlock", "content": [{ "type": "text", "text": code.join("\n") }] });
            // ADF refuses an empty text node.
            if code.iter().all(|l| l.is_empty()) {
                b["content"] = json!([]);
            }
            if !lang.trim().is_empty() {
                b["attrs"] = json!({ "language": lang.trim() });
            }
            blocks.push(b);
        } else if let Some((level, text)) = heading(t) {
            flush(&mut para, &mut blocks);
            blocks.push(json!({ "type": "heading", "attrs": { "level": level }, "content": inline(text) }));
        } else if let Some((ordered, _)) = list_item(t) {
            flush(&mut para, &mut blocks);
            let mut items = Vec::new();
            while let Some(text) = lines_peek_item(t, &items, &mut lines, ordered) {
                items.push(json!({ "type": "listItem", "content": [{ "type": "paragraph", "content": inline(&text) }] }));
            }
            blocks.push(json!({ "type": if ordered { "orderedList" } else { "bulletList" }, "content": items }));
        } else if t.is_empty() {
            flush(&mut para, &mut blocks);
        } else {
            para.push(l);
        }
    }
    flush(&mut para, &mut blocks);
    json!({ "type": "doc", "version": 1, "content": blocks })
}

/// The first item is `first` (already taken from `lines`); later ones come off `lines` while they
/// are items of the same kind.
fn lines_peek_item<'a>(
    first: &'a str,
    items: &[Value],
    lines: &mut std::iter::Peekable<std::str::Lines<'a>>,
    ordered: bool,
) -> Option<String> {
    if items.is_empty() {
        return list_item(first).map(|(_, t)| t.to_string());
    }
    let next = lines.peek()?.trim_start();
    match list_item(next) {
        Some((o, t)) if o == ordered => {
            let t = t.to_string();
            lines.next();
            Some(t)
        }
        _ => None,
    }
}

fn heading(t: &str) -> Option<(usize, &str)> {
    let level = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&level).then(|| t[level..].strip_prefix(' ')).flatten().map(|rest| (level, rest.trim()))
}

/// `(ordered, text)` for `- x`, `* x` or `1. x`.
fn list_item(t: &str) -> Option<(bool, &str)> {
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        return Some((false, rest));
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    (digits > 0).then(|| t[digits..].strip_prefix(". ")).flatten().map(|rest| (true, rest))
}

/// A paragraph's lines, joined by hard breaks.
fn lines_inline(lines: &[&str]) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push(json!({ "type": "hardBreak" }));
        }
        out.extend(inline(l.trim()));
    }
    out
}

/// Text with `[t](url)`, bare `https://` URLs, `code` and **bold** as marks.
fn inline(s: &str) -> Vec<Value> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\[([^\]]+)\]\((https?://[^)\s]+)\)|`([^`]+)`|\*\*([^*]+)\*\*|(https?://[^\s)>]+)").expect("regex")
    });
    let text = |t: &str, marks: Value| {
        let mut v = json!({ "type": "text", "text": t });
        if marks.as_array().is_some_and(|m| !m.is_empty()) {
            v["marks"] = marks;
        }
        v
    };
    let link = |href: &str| json!([{ "type": "link", "attrs": { "href": href } }]);
    let mut out = Vec::new();
    let mut at = 0;
    for c in RE.captures_iter(s) {
        let m = c.get(0).expect("match");
        if m.start() > at {
            out.push(text(&s[at..m.start()], json!([])));
        }
        out.push(match (c.get(1), c.get(2), c.get(3), c.get(4), c.get(5)) {
            (Some(t), Some(u), ..) => text(t.as_str(), link(u.as_str())),
            (_, _, Some(code), ..) => text(code.as_str(), json!([{ "type": "code" }])),
            (_, _, _, Some(b), _) => text(b.as_str(), json!([{ "type": "strong" }])),
            (.., Some(u)) => {
                // "see https://x.io/1." links to …/1, and the full stop stays text.
                let url = u.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']);
                out.push(text(url, link(url)));
                at = u.start() + url.len();
                continue;
            }
            _ => continue,
        });
        at = m.end();
    }
    if at < s.len() {
        out.push(text(&s[at..], json!([])));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `acli` keeping its current site in a file and logging every call.
    fn fake(dir: &Path) -> Acli {
        let bin = dir.join("acli");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nd={d}\necho \"$*\" >> $d/calls\n\
                 case \"$2 $3\" in\n\
                   'auth status') echo '✓ Authenticated'; echo \"  Site: $(cat $d/site)\" ;;\n\
                   'auth switch') [ \"$5\" = nowhere ] && exit 1; echo \"$5\" > $d/site ;;\n\
                   *) echo \"ran on $(cat $d/site)\" ;;\n\
                 esac\n",
                d = dir.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("site"), "home.atlassian.net\n").unwrap();
        Acli { bin, lock: dir.join("data/lock"), config: dir.join("jira_config.yaml") }
    }

    #[test]
    fn runs_on_the_site_then_switches_back() {
        let d = tempfile::tempdir().unwrap();
        let a = fake(d.path());
        assert_eq!(a.run("other.atlassian.net", &["workitem", "search"]).unwrap().trim(), "ran on other.atlassian.net");
        assert_eq!(std::fs::read_to_string(d.path().join("site")).unwrap().trim(), "home.atlassian.net");
        // Already there: no switching.
        let calls = || std::fs::read_to_string(d.path().join("calls")).unwrap().lines().count();
        let n = calls();
        a.run("home.atlassian.net", &["workitem", "search"]).unwrap();
        assert_eq!(calls() - n, 2, "status + the command");
        let e = a.run("nowhere", &["workitem", "search"]).unwrap_err();
        assert!(format!("{e:#}").contains("switch to nowhere"), "{e:#}");
        // A command that went through isn't reported as failed when the switch back fails.
        std::fs::write(d.path().join("site"), "nowhere\n").unwrap();
        assert_eq!(a.run("other.atlassian.net", &["workitem", "search"]).unwrap().trim(), "ran on other.atlassian.net");
    }

    #[test]
    fn reads_logged_in_sites() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("jira_config.yaml");
        std::fs::write(
            &f,
            "version: 1\ncurrent_profile: x\nprofiles:\n    - site: a.jira.com\n      account_id: 7:a\n      email: x\n    - site: b.atlassian.net\n",
        )
        .unwrap();
        assert_eq!(profiles_in(&f), [("a.jira.com".into(), "7:a".into()), ("b.atlassian.net".into(), String::new())]);
    }

    #[test]
    fn markdown_becomes_adf() {
        let doc = md_to_adf(
            "## Fix\nOpened [the PR](https://x.io/1), see `src/a.rs`\nand **this**.\n\n- one\n- two\n1. first\n\n```rust\nfn a() {}\n```",
        );
        let types: Vec<&str> = doc["content"].as_array().unwrap().iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(types, ["heading", "paragraph", "bulletList", "orderedList", "codeBlock"]);
        let p = &doc["content"][1]["content"];
        assert_eq!(p[1]["marks"][0]["attrs"]["href"], "https://x.io/1");
        assert_eq!(p[3]["marks"][0]["type"], "code");
        assert_eq!(p[4]["type"], "hardBreak", "a single newline is a line break");
        assert_eq!(p[6]["marks"][0]["type"], "strong");
        assert_eq!(doc["content"][2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(doc["content"][4]["attrs"]["language"], "rust");
        assert_eq!(md_to_adf("see https://a.io/x")["content"][0]["content"][1]["marks"][0]["attrs"]["href"], "https://a.io/x");
        let end = &md_to_adf("see https://a.io/x.")["content"][0]["content"];
        assert_eq!((end[1]["text"].as_str(), end[2]["text"].as_str()), (Some("https://a.io/x"), Some(".")));
        assert_eq!(md_to_adf("```\n\n```")["content"][0]["content"], json!([]), "no empty text node");
    }

    #[test]
    fn keys_and_branches() {
        assert!(is_key("ABC-123") && is_key("A_1-9"));
        assert!(!is_key("ABC") && !is_key("1BC-2") && !is_key("ABC-1 OR x") && !is_key("ABC-"));
        let r = JiraRef { site: "Acme.atlassian.net".into(), key: "ABC-12".into(), paths: vec![] };
        assert_eq!(r.branch(), "nightporter/jira/acme/ABC-12");
    }

    #[test]
    fn adf_reads_as_text() {
        let v: Value = serde_json::from_str(
            r#"{"id":"759125","key":"TKF-559","fields":{"summary":"S","status":{"statusCategory":{"key":"done"}},
               "creator":{"accountId":"r1","displayName":"Ann"},"assignee":null,
               "description":{"type":"doc","content":[{"type":"heading","content":[{"type":"text","text":"Problem"}]},
                 {"type":"bulletList","content":[{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"a"}]}]}]},
                 {"type":"paragraph","content":[{"type":"text","text":"see "},{"type":"text","text":"log","marks":[{"type":"link","attrs":{"href":"https://l.io"}}]},
                   {"type":"text","text":" "},{"type":"inlineCard","attrs":{"url":"https://c.io"}},{"type":"text","text":" "},{"type":"mention","attrs":{"text":"@Cy"}}]}]},
               "comment":{"comments":[{"author":{"displayName":"Bob","accountId":"b2"},"body":{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"hi"}]}]}}]}}}"#,
        )
        .unwrap();
        let i = Issue::parse(&v).unwrap();
        assert_eq!((i.id, i.key.as_str(), i.done, i.assignee.as_str(), i.creator.as_str()), (759_125, "TKF-559", true, "", "r1"));
        assert_eq!(i.commenters, ["b2"]);
        assert_eq!(i.text, "Problem\n- a\nsee log (https://l.io) https://c.io @Cy\n\n\n--- comment by Bob ---\nhi");
    }
}
