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
    pub reporter: String,
    pub reporter_name: String,
    /// Description, then the comments (a view only).
    pub text: String,
}

impl Issue {
    fn parse(v: &Value) -> Result<Self> {
        let f = &v["fields"];
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let mut text = adf_text(&f["description"]);
        for c in f["comment"]["comments"].as_array().into_iter().flatten() {
            let _ = write!(text, "\n\n--- comment by {} ---\n{}", s(&c["author"]["displayName"]), adf_text(&c["body"]));
        }
        Ok(Self {
            id: s(&v["id"]).parse().with_context(|| format!("jira issue without a numeric id: {v}"))?,
            key: s(&v["key"]),
            summary: s(&f["summary"]),
            done: f["status"]["statusCategory"]["key"] == "done",
            assignee: s(&f["assignee"]["accountId"]),
            reporter: s(&f["reporter"]["accountId"]),
            reporter_name: s(&f["reporter"]["displayName"]),
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
        Self { bin: "acli".into(), lock: paths.data_dir.join("acli.lock"), config: home.join(".config/acli/jira_config.yaml") }
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
        profiles_in(&self.config).into_iter().find(|(s, _)| s == site).map(|(_, a)| a)
    }

    /// `acli jira <args>` on `site`. Switches back to the site that was current before, and fails
    /// if `acli` doesn't end up there (someone else switched meanwhile, or the switch back failed).
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
                .with_context(|| format!("acli: switch to {site} (log in: acli jira auth login)"))?;
        }
        let out = self.acli(args);
        if before != site {
            self.acli(&["auth", "switch", "--site", &before]).with_context(|| format!("acli: switch back to {before}"))?;
            let now = self.current_site()?;
            if now != before {
                bail!("acli is on {now}, not {before} as before Night Porter used it; check `acli jira auth status`");
            }
        }
        out
    }

    /// Up to `limit` issues matching `jql`, oldest first.
    pub fn search(&self, site: &str, jql: &str, limit: usize) -> Result<Vec<Issue>> {
        let jql = format!("{jql} ORDER BY created ASC");
        let out = self.run(
            site,
            &["workitem", "search", "--jql", &jql, "--fields", "summary,status,assignee,reporter", "--limit", &limit.to_string(), "--json"],
        )?;
        let v: Value = serde_json::from_str(&out).with_context(|| format!("acli search on {site}: not JSON"))?;
        v.as_array().into_iter().flatten().map(Issue::parse).collect()
    }

    /// One issue with its description and comments.
    pub fn view(&self, site: &str, key: &str) -> Result<Issue> {
        let out =
            self.run(site, &["workitem", "view", key, "--fields", "summary,description,comment,status,assignee,reporter", "--json"])?;
        Issue::parse(&serde_json::from_str(&out).with_context(|| format!("acli view {key}: not JSON"))?)
    }

    /// A comment written in Markdown, sent as Atlassian Document Format so it keeps its layout.
    pub fn comment(&self, site: &str, key: &str, markdown: &str) -> Result<()> {
        self.run(site, &["workitem", "comment", "create", "--key", key, "--body", &md_to_adf(markdown).to_string()]).map(|_| ())
    }

    /// Assign to you (`on`) or to nobody.
    pub fn assign(&self, site: &str, key: &str, on: bool) -> Result<()> {
        let who: &[&str] = if on { &["--assignee", "@me"] } else { &["--remove-assignee"] };
        self.run(site, &[&["workitem", "assign", "--key", key, "--yes"], who].concat()).map(|_| ())
    }

    fn current_site(&self) -> Result<String> {
        let out = self.acli(&["auth", "status"]).context("acli: not logged in to Jira (acli jira auth login)")?;
        out.lines()
            .find_map(|l| l.trim().strip_prefix("Site:").map(|s| s.trim().to_string()))
            .context("acli jira auth status: no Site line")
    }

    fn acli(&self, args: &[&str]) -> Result<String> {
        let o = Command::new(&self.bin).arg("jira").args(args).output().with_context(|| format!("run {}", self.bin.display()))?;
        if !o.status.success() {
            // acli prints some errors (`✗ Error: ...`) on stdout.
            let err = [&o.stderr, &o.stdout].map(|b| String::from_utf8_lossy(b).trim().to_string()).join(" ");
            bail!("acli jira {}: {}", args[..args.len().min(2)].join(" "), err.trim());
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }
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
            "text" => out.push_str(v["text"].as_str().unwrap_or_default()),
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
            if code.is_empty() {
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
            (.., Some(u)) => text(u.as_str(), link(u.as_str())),
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
    }

    #[test]
    fn adf_reads_as_text() {
        let v: Value = serde_json::from_str(
            r#"{"id":"759125","key":"TKF-559","fields":{"summary":"S","status":{"statusCategory":{"key":"done"}},
               "reporter":{"accountId":"r1","displayName":"Ann"},"assignee":null,
               "description":{"type":"doc","content":[{"type":"heading","content":[{"type":"text","text":"Problem"}]},
                 {"type":"bulletList","content":[{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"a"}]}]}]}]},
               "comment":{"comments":[{"author":{"displayName":"Bob"},"body":{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"hi"}]}]}}]}}}"#,
        )
        .unwrap();
        let i = Issue::parse(&v).unwrap();
        assert_eq!((i.id, i.key.as_str(), i.done, i.assignee.as_str(), i.reporter.as_str()), (759_125, "TKF-559", true, "", "r1"));
        assert_eq!(i.text, "Problem\n- a\n\n\n--- comment by Bob ---\nhi");
    }
}
