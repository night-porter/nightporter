//! `nightporter doctor` and the tool checks used by the setup wizard.

use crate::config::{AgentKind, Config, TokenSource};
use crate::forge::ForgeKind;
use crate::paths::Paths;
use std::path::Path;
use std::process::Command;

pub struct Check {
    pub name: String,
    pub ok: bool,
    pub optional: bool,
    pub detail: String,
    pub fix: String,
}

fn version(bin: &str, arg: &str) -> Option<String> {
    let o = Command::new(bin).arg(arg).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or_default().trim().to_string())
}

fn tool(name: &str, arg: &str, fix: &str, optional: bool) -> Check {
    match version(name, arg) {
        Some(v) => Check { name: name.into(), ok: true, optional, detail: v, fix: String::new() },
        None => Check { name: name.into(), ok: false, optional, detail: "not found on PATH".into(), fix: fix.into() },
    }
}

fn auth(bin: &str, host: &str) -> Check {
    let ok = Command::new(bin).args(["auth", "status", "--hostname", host]).output().is_ok_and(|o| o.status.success());
    Check {
        name: format!("{bin} auth ({host})"),
        ok,
        optional: false,
        detail: if ok { "logged in".into() } else { "not logged in".into() },
        fix: format!("{bin} auth login --hostname {host}"),
    }
}

/// Forgejo/Gitea: a keychain token, or a `tea` login for the host. The token itself is never shown.
fn forgejo_auth(host: &str) -> Check {
    let keyring = keyring::Entry::new("nightporter", &format!("forgejo:{host}")).and_then(|e| e.get_password()).is_ok();
    let tea = !keyring && crate::http::tea_token(host).is_ok();
    Check {
        name: format!("forgejo token ({host})"),
        ok: keyring || tea,
        optional: false,
        detail: if keyring {
            "in the keychain".into()
        } else if tea {
            "from tea".into()
        } else {
            "no token".into()
        },
        fix: format!("nightporter token set forgejo {host}   (scopes: {}; then token = \"keyring\")", crate::forge::forgejo::SCOPES),
    }
}

pub fn tools(forges: &[(ForgeKind, String)], agents: &[AgentKind]) -> Vec<Check> {
    let brew = cfg!(target_os = "macos");
    let install = |pkg: &str, other: &str| if brew { format!("brew install {pkg}") } else { other.to_string() };
    let mut out = vec![tool("git", "--version", &install("git", "install git with your package manager"), false)];
    if forges.iter().any(|(f, _)| *f == ForgeKind::GitHub) {
        out.push(tool("gh", "--version", &install("gh", "see https://cli.github.com"), false));
    }
    if forges.iter().any(|(f, _)| *f == ForgeKind::Forgejo) {
        out.push(tool("tea", "--version", &install("tea", "see https://gitea.com/gitea/tea"), true));
    }
    if forges.iter().any(|(f, _)| *f == ForgeKind::GitLab) {
        out.push(tool("glab", "--version", &install("glab", "see https://gitlab.com/gitlab-org/cli"), false));
    }
    for (f, host) in forges {
        match f {
            ForgeKind::GitHub => out.push(auth("gh", host)),
            ForgeKind::GitLab => out.push(auth("glab", host)),
            ForgeKind::Forgejo => out.push(forgejo_auth(host)),
        }
    }
    for a in agents {
        let fix = match a {
            AgentKind::Claude => "npm install -g @anthropic-ai/claude-code   (then run `claude` once to log in)",
            AgentKind::OpenCode => "curl -fsSL https://opencode.ai/install | bash   (or brew install opencode)",
            AgentKind::Codex => "npm install -g @openai/codex   (or brew install --cask codex; then run `codex` once to log in)",
            AgentKind::Gemini => {
                "npm install -g @google/gemini-cli   (0.67 or newer; set GEMINI_API_KEY for the daemon or run `gemini` once to log in)"
            }
        };
        out.push(tool(a.bin(), "--version", fix, false));
    }
    out.push(tool("gitleaks", "version", &install("gitleaks", "see https://github.com/gitleaks/gitleaks"), true));
    out.push(tool("herdr", "--version", "optional, for `nightporter handoff`: see https://herdr.dev", true));
    // Claude Code's and Gemini CLI's sandboxes (on by default) need these on Linux; macOS uses
    // built-in Seatbelt.
    if cfg!(target_os = "linux") && (agents.contains(&AgentKind::Claude) || agents.contains(&AgentKind::Gemini)) {
        out.push(tool("bwrap", "--version", "sudo apt-get install bubblewrap   (or set sandbox = false)", false));
    }
    if cfg!(target_os = "linux") && agents.contains(&AgentKind::Claude) {
        out.push(tool("socat", "-V", "sudo apt-get install socat   (or set sandbox = false)", false));
    }
    out
}

pub fn run(paths: &Paths) -> bool {
    let mut checks = collect(paths, tools, claude_user_settings_check());
    if let Ok(cfg) = Config::load(paths) {
        checks.extend(token_checks(&cfg, uses_projects(paths, &cfg)));
        checks.extend(jira_checks(paths, &cfg));
    }
    print(&checks);
    checks.iter().all(|c| c.ok || c.optional)
}

/// What Night Porter does with a token, and the scopes that allow it (any one is enough).
/// Required ones fail the doctor; the others only warn, since they turn one feature off.
struct Need {
    feature: &'static str,
    any_of: &'static [&'static str],
    required: bool,
}

const GITHUB: &[Need] = &[
    Need { feature: "PRs, issues, comments, pushes", any_of: &["repo"], required: true },
    Need { feature: "notifications (new activity)", any_of: &["notifications", "repo"], required: true },
    Need { feature: "team trust (trust.allow_teams)", any_of: &["read:org", "write:org", "admin:org"], required: false },
    Need { feature: PROJECTS, any_of: &["read:project", "project"], required: false },
];

const GITLAB: &[Need] = &[Need { feature: "MRs, issues, comments, pushes", any_of: &["api"], required: true }];

const PROJECTS: &str = "sprint pickup from a Project iteration";

/// Some repo's `[maintainer.pickup]` reads a GitHub Project.
fn uses_projects(paths: &Paths, cfg: &Config) -> bool {
    walk_repo_files(&paths.repos_dir()).unwrap_or_default().iter().any(|f| {
        std::fs::read_to_string(f)
            .ok()
            .and_then(|t| t.parse::<toml::Table>().ok())
            .and_then(|t| cfg.resolve(Some(&t)).ok())
            .is_some_and(|s| s.maintainer.pickup.is_some_and(|p| p.project.is_some()))
    })
}

/// Every repo's `[[jira]]` sources: `acli` is installed and logged in to their sites, each JQL
/// runs (so it's online), and where sources may fight over an issue or a folder.
fn jira_checks(paths: &Paths, cfg: &Config) -> Vec<Check> {
    let dir = paths.repos_dir();
    let mut broken = Vec::new();
    let mut sources: Vec<(String, crate::config::JiraSource)> = Vec::new();
    for f in walk_repo_files(&dir).unwrap_or_default() {
        let name = f.strip_prefix(&dir).unwrap_or(&f).with_extension("").display().to_string();
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        // A repo file that doesn't load leaves the repo unwatched: say why, not just for Jira.
        match text.parse().map_err(anyhow::Error::from).and_then(|t| cfg.resolve(Some(&t))) {
            Ok(s) => sources.extend(s.jira.into_iter().map(|j| (name.clone(), j))),
            Err(e) => broken.push(Check {
                name: format!("repo file {name}"),
                ok: false,
                optional: false,
                detail: format!("{e:#}; the repo isn't watched until it's fixed"),
                fix: format!("edit {}", f.display()),
            }),
        }
    }
    if sources.is_empty() {
        return broken;
    }
    let acli = tool("acli", "--version", "install Atlassian's acli: https://developer.atlassian.com/cloud/acli/", false);
    if !acli.ok {
        broken.push(acli);
        return broken;
    }
    let mut out = broken;
    out.push(acli);
    let client = crate::jira::Acli::new(paths);
    let logged_in = client.sites();
    let mut sites: Vec<&str> = sources.iter().map(|(_, j)| j.site.as_str()).collect();
    sites.sort_unstable();
    sites.dedup();
    for site in sites.iter().filter(|s| !logged_in.iter().any(|l| l == *s)) {
        out.push(Check {
            name: format!("acli ({site})"),
            ok: false,
            optional: false,
            detail: "not logged in".into(),
            fix: format!("acli jira auth login --site {site}"),
        });
    }
    for (repo, j) in sources.iter().filter(|(_, j)| logged_in.contains(&j.site)) {
        let r = client.run(&j.site, &["workitem", "search", "--jql", &j.query(), "--count"]);
        out.push(Check {
            name: format!("jira {repo} ({})", j.site),
            ok: r.is_ok(),
            optional: false,
            detail: match &r {
                Ok(o) => o.lines().last().unwrap_or_default().trim_start_matches('✓').trim().to_string(),
                Err(e) => format!("{e:#}"),
            },
            fix: match &r {
                Err(e) if format!("{e:#}").contains("switch") => format!("acli jira auth login --site {}", j.site),
                _ => format!("check the jql of its [[jira]] in {}", dir.join(repo).with_extension("toml").display()),
            },
        });
    }
    out.extend(jira_overlaps(&sources));
    out
}

/// Warnings: the same JQL on two repos (the first to claim an issue gets it), and two sources on
/// one repo and site whose `paths` overlap (the agent can't tell which part an issue is for).
fn jira_overlaps(sources: &[(String, crate::config::JiraSource)]) -> Vec<Check> {
    let covers = |a: &[String], b: &[String]| {
        a.is_empty() || b.is_empty() || a.iter().any(|x| b.iter().any(|y| Path::new(x).starts_with(y) || Path::new(y).starts_with(x)))
    };
    let mut out = Vec::new();
    for (i, (ra, a)) in sources.iter().enumerate() {
        for (rb, b) in &sources[i + 1..] {
            if a.site != b.site {
                continue;
            }
            let detail = if ra != rb && a.jql.trim() == b.jql.trim() {
                format!("{ra} and {rb} use the same JQL; whichever claims an issue first gets it")
            } else if ra == rb && covers(&a.paths, &b.paths) {
                let show = |p: &[String]| if p.is_empty() { "the whole repo".to_string() } else { p.join(", ") };
                format!("{ra}: two [[jira]] sources cover the same folders ({} and {})", show(&a.paths), show(&b.paths))
            } else {
                continue;
            };
            out.push(Check {
                name: format!("jira overlap ({})", a.site),
                ok: false,
                optional: true,
                detail,
                fix: "narrow a jql (component, label) or set distinct paths".into(),
            });
        }
    }
    out
}

/// Each account's token scopes against [`GITHUB`] / [`GITLAB`]. Logs in, so it's online.
fn token_checks(cfg: &Config, projects: bool) -> Vec<Check> {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return Vec::new() };
    let mut out = Vec::new();
    // Forgejo/Gitea don't list scopes: `forgejo_auth` names the ones to create the token with.
    for a in cfg.accounts.iter().filter(|a| a.forge != ForgeKind::Forgejo) {
        match rt.block_on(async { Ok::<_, anyhow::Error>(crate::daemon::connect_one(a).await?.scopes().await) }) {
            Ok(scopes) => out.extend(scope_checks(a.forge, a.host(), a.token, scopes.as_deref(), projects)),
            // The CLI auth check doesn't see a keychain token, so say it here.
            Err(e) => out.push(Check {
                name: format!("login ({})", a.host()),
                ok: false,
                optional: false,
                detail: format!("{e:#}"),
                fix: refresh(a.forge, a.host(), a.token, None),
            }),
        }
    }
    out
}

/// How to get a token with `scope` (or a working one, with `None`) from where this account reads it.
fn refresh(forge: ForgeKind, host: &str, source: TokenSource, scope: Option<&str>) -> String {
    let with = scope.map(|s| format!(" with the `{s}` scope")).unwrap_or_default();
    match (forge, source) {
        (ForgeKind::GitHub, TokenSource::Cli) => match scope {
            Some(s) => format!("gh auth refresh --hostname {host} --scopes {s}"),
            None => format!("gh auth login --hostname {host}"),
        },
        (ForgeKind::GitLab, TokenSource::Cli) => format!("create a token{with} and run `glab auth login --hostname {host}`"),
        (f, _) => format!("create a token{with} on {host}, then `nightporter token set {} {host}`", f.as_str()),
    }
}

/// One check per feature the token's scopes don't cover, or one that says it couldn't tell.
fn scope_checks(forge: ForgeKind, host: &str, source: TokenSource, scopes: Option<&[String]>, projects: bool) -> Vec<Check> {
    let needs: Vec<&Need> = match forge {
        ForgeKind::GitHub => GITHUB,
        ForgeKind::GitLab => GITLAB,
        ForgeKind::Forgejo => return Vec::new(), // `forgejo_auth` names the scopes to create it with
    }
    .iter()
    // Projects only matter where a repo's pickup reads one.
    .filter(|n| projects || n.feature != PROJECTS)
    .collect();
    let name = format!("token scopes ({host})");
    let Some(scopes) = scopes else {
        // A GitHub token that lists nothing is most likely fine-grained, and those can't read
        // notifications, so Night Porter would never see new activity.
        let github = forge == ForgeKind::GitHub;
        let all: Vec<_> = needs.iter().map(|n| format!("{}: {}", n.feature, n.any_of[0])).collect();
        return vec![Check {
            name,
            ok: !github,
            optional: true,
            detail: if github {
                "this token doesn't list its scopes: if it's fine-grained, it can't read notifications and nothing new will show up".into()
            } else {
                format!("this token doesn't list its scopes; it needs {}", all.join("; "))
            },
            fix: if github { refresh(forge, host, TokenSource::Cli, None) } else { String::new() },
        }];
    };
    let missing: Vec<&Need> = needs.iter().copied().filter(|n| !n.any_of.iter().any(|s| scopes.iter().any(|t| t == s))).collect();
    if missing.is_empty() {
        return vec![Check {
            name,
            ok: true,
            optional: false,
            detail: format!("covers every feature ({})", scopes.join(", ")),
            fix: String::new(),
        }];
    }
    missing
        .into_iter()
        .map(|n| Check {
            name: name.clone(),
            ok: false,
            optional: !n.required,
            detail: format!("no `{}`: {} won't work", n.any_of[0], n.feature),
            fix: refresh(forge, host, source, Some(n.any_of[0])),
        })
        .collect()
}

/// Every check, with the tool probes and the Claude settings finding passed in (tests stub them).
fn collect(paths: &Paths, tools: impl Fn(&[(ForgeKind, String)], &[AgentKind]) -> Vec<Check>, claude: Option<Check>) -> Vec<Check> {
    let mut checks = Vec::new();
    let cfg = match Config::load(paths) {
        Ok(c) => {
            checks.push(Check {
                name: "config".into(),
                ok: true,
                optional: false,
                detail: paths.config_file().display().to_string(),
                fix: String::new(),
            });
            Some(c)
        }
        Err(e) => {
            checks.push(Check {
                name: "config".into(),
                ok: false,
                optional: false,
                detail: format!("{e:#}"),
                fix: format!("edit {}", paths.config_file().display()),
            });
            None
        }
    };
    if let Some(cfg) = &cfg {
        let forges: Vec<_> = cfg.accounts.iter().map(|a| (a.forge, a.host().to_string())).collect();
        let mut agents = vec![cfg.resolve(None).map_or(AgentKind::Claude, |s| s.agent)];
        if let Ok(rd) = walk_repo_files(&paths.repos_dir()) {
            for f in rd {
                if let Ok(t) = std::fs::read_to_string(&f)
                    && let Ok(tbl) = t.parse::<toml::Table>()
                    && let Ok(s) = cfg.resolve(Some(&tbl))
                    && !agents.contains(&s.agent)
                {
                    agents.push(s.agent);
                }
            }
        }
        checks.extend(tools(&forges, &agents));
        let h = cfg.handoff_agent;
        if cfg.handoff == crate::config::Handoff::T3 {
            checks.push(tool("t3", "--version", "install T3 Code (https://t3.codes) or change handoff", false));
        } else if h != crate::config::HandoffAgent::Job && !agents.iter().any(|a| a.bin() == h.bin()) {
            checks.push(tool(h.bin(), "--version", &format!("install {} or set handoff_agent = \"job\"", h.label()), false));
        }
        let dir = cfg.code_dir();
        checks.push(Check {
            name: "code dir".into(),
            ok: dir.is_dir(),
            optional: false,
            detail: dir.display().to_string(),
            fix: "set code_dir in config.toml".into(),
        });
        let free = crate::git::free_gb(&paths.data_dir).unwrap_or(0);
        checks.push(Check {
            name: "disk".into(),
            ok: free >= cfg.min_free_gb,
            optional: false,
            detail: format!("{free} GB free (min {})", cfg.min_free_gb),
            fix: "free disk space or lower min_free_gb".into(),
        });
        if cfg.accounts.is_empty() {
            checks.push(Check {
                name: "accounts".into(),
                ok: false,
                optional: false,
                detail: "no [[accounts]]".into(),
                fix: "run `nightporter setup`".into(),
            });
        }
    }
    checks.extend(claude);
    let running = paths.daemon_running();
    // Installed as a service but down: nothing is being watched, and it won't come back alone.
    let service = crate::service::installed();
    checks.push(Check {
        name: "daemon".into(),
        ok: running,
        optional: !service,
        detail: match (running, service) {
            (true, _) => "running".into(),
            (false, true) => "not running, though it's installed to run in the background".into(),
            (false, false) => "not running".into(),
        },
        fix: "nightporter start   (and `nightporter service install` to start it at login)".into(),
    });
    checks
}

/// Your own Claude Code settings also load for Night Porter's jobs (hooks are switched off, and
/// Night Porter's denies always win). Broad allow rules or plugins widen what the unattended
/// agent may do, so say so.
fn claude_user_settings_check() -> Option<Check> {
    let dir =
        std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from).or_else(|| std::env::home_dir().map(|h| h.join(".claude")))?;
    claude_settings_in(&dir)
}

fn claude_settings_in(dir: &std::path::Path) -> Option<Check> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).ok()?).ok()?;
    Some(claude_settings_finding(&v))
}

fn claude_settings_finding(v: &serde_json::Value) -> Check {
    let allows = v["permissions"]["allow"].as_array().map_or(0, Vec::len);
    let plugins = v["enabledPlugins"].as_object().map_or(0, |m| m.values().filter(|x| x.as_bool() == Some(true)).count());
    let mut found = Vec::new();
    if allows > 0 {
        found.push(format!("{allows} allow rule(s)"));
    }
    if plugins > 0 {
        found.push(format!("{plugins} plugin(s)"));
    }
    Check {
        name: "claude settings".into(),
        ok: found.is_empty(),
        optional: true,
        detail: if found.is_empty() {
            "no extra allows or plugins".into()
        } else {
            format!("~/.claude/settings.json has {} that also apply to jobs", found.join(" and "))
        },
        fix: "keep personal allow rules narrow; see https://docs.night-porter.com/threat-model".into(),
    }
}

/// Text markers, not colour, so it reads the same in any terminal or screen reader.
pub fn print(checks: &[Check]) {
    print!("{}", render(checks));
}

fn render(checks: &[Check]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for c in checks {
        let mark = if c.ok {
            "[ok]  "
        } else if c.optional {
            "[warn]"
        } else {
            "[FAIL]"
        };
        let _ = writeln!(s, "{mark} {:<22} {}", c.name, c.detail);
        if !c.ok {
            let _ = writeln!(s, "       fix: {}", c.fix);
        }
    }
    s
}

pub fn walk_repo_files(dir: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            out.extend(walk_repo_files(&p)?);
        } else if p.extension().is_some_and(|x| x == "toml") {
            out.push(p);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn scope_checks_name_what_breaks() {
        use TokenSource::{Cli, Keyring};
        let v = |xs: &[&str]| xs.iter().map(ToString::to_string).collect::<Vec<_>>();
        let gh = |xs: &[&str]| scope_checks(ForgeKind::GitHub, "github.com", Cli, Some(&v(xs)), true);
        let all = gh(&["repo", "read:org", "project"]);
        assert!(all.len() == 1 && all[0].ok, "every feature covered");
        let no_org = gh(&["repo", "read:project"]);
        assert!(no_org.len() == 1 && !no_org[0].ok && no_org[0].optional && no_org[0].fix.contains("--scopes read:org"));
        assert!(gh(&["admin:org", "repo", "project"])[0].ok, "a broader scope counts");
        let bare = gh(&["gist"]);
        assert!(bare.iter().filter(|c| !c.optional).count() == 2, "core and notifications fail");
        let unknown = scope_checks(ForgeKind::GitHub, "github.com", Cli, None, true);
        assert!(!unknown[0].ok && unknown[0].optional && unknown[0].detail.contains("notifications"), "fine-grained: warn");
        let pat = scope_checks(ForgeKind::GitHub, "github.com", Keyring, Some(&v(&["repo"])), true);
        assert!(pat[0].fix.contains("nightporter token set github"), "a keychain token isn't fixed with gh: {}", pat[0].fix);
        assert!(!scope_checks(ForgeKind::GitLab, "gitlab.com", Cli, Some(&v(&["read_api"])), true)[0].ok);
        assert!(scope_checks(ForgeKind::GitLab, "gitlab.com", Cli, None, true)[0].ok);
        assert!(scope_checks(ForgeKind::Forgejo, "codeberg.org", Cli, None, true).is_empty());
        let no_projects = scope_checks(ForgeKind::GitHub, "github.com", Cli, Some(&v(&["repo", "read:org"])), false);
        assert!(no_projects.len() == 1 && no_projects[0].ok, "read:project only matters where a pickup uses a project");
    }

    #[test]
    fn flags_personal_claude_allows_and_plugins() {
        let v = serde_json::json!({"permissions": {"allow": ["Bash(npm:*)"]}, "enabledPlugins": {"x@y": true, "z@y": false}});
        let c = super::claude_settings_finding(&v);
        assert!(!c.ok && c.optional && c.detail.contains("1 allow rule(s) and 1 plugin(s)"), "{}", c.detail);
        assert!(super::claude_settings_finding(&serde_json::json!({})).ok);
    }

    use super::*;
    use std::cell::RefCell;

    #[test]
    fn a_repo_file_that_does_not_load_fails() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        let f = paths.repos_dir().join("github/o/r.toml");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "[[jira]]\nsite = \"https://a.atlassian.net\"\njql = \"project = FE\"\n").unwrap();
        let c = jira_checks(&paths, &Config::parse("").unwrap());
        assert_eq!(c.len(), 1);
        assert!(!c[0].ok && c[0].name == "repo file github/o/r" && c[0].detail.contains("bare site name"), "{:?}", c[0].detail);
    }

    #[test]
    fn jira_overlaps_warn() {
        let j = |site: &str, jql: &str, paths: &[&str]| crate::config::JiraSource {
            site: site.into(),
            jql: jql.into(),
            paths: paths.iter().map(|p| (*p).to_string()).collect(),
            pickup: crate::config::JiraPickup::Assigned,
            verb: crate::forge::Verb::Fix,
            max_claimed: 2,
            priority: 0,
            creators: vec![],
            in_progress: vec![],
            in_review: vec![],
        };
        let found = |s: &[(String, crate::config::JiraSource)]| jira_overlaps(s).into_iter().map(|c| c.detail).collect::<Vec<_>>();
        // FE and BE projects, one folder each: fine.
        let split = [("x".to_string(), j("a", "project = FE", &["frontend/"])), ("x".to_string(), j("a", "project = BE", &["backend"]))];
        assert_eq!(found(&split), Vec::<String>::new());
        let nested = [("x".to_string(), j("a", "project = FE", &["web/"])), ("x".to_string(), j("a", "project = BE", &["web/api"]))];
        assert!(found(&nested)[0].contains("same folders"));
        let twice = [("x".to_string(), j("a", "project = FE", &[])), ("y".to_string(), j("a", "project = FE", &[]))];
        assert!(found(&twice)[0].contains("first gets it"));
        let sites = [("x".to_string(), j("a", "project = FE", &[])), ("x".to_string(), j("b", "project = FE", &[]))];
        assert_eq!(found(&sites), Vec::<String>::new(), "different sites never overlap");
    }

    fn check(name: &str, ok: bool, optional: bool) -> Check {
        Check { name: name.into(), ok, optional, detail: format!("{name} detail"), fix: format!("fix {name}") }
    }

    #[test]
    fn renders_text_markers_and_fixes_for_failures() {
        let s = render(&[check("a", true, false), check("b", false, true), check("c", false, false)]);
        assert_eq!(
            s,
            "[ok]   a                      a detail\n\
             [warn] b                      b detail\n       fix: fix b\n\
             [FAIL] c                      c detail\n       fix: fix c\n"
        );
    }

    #[test]
    fn reads_claude_settings_from_the_config_dir() {
        let d = tempfile::tempdir().unwrap();
        assert!(claude_settings_in(d.path()).is_none(), "no settings file, no finding");
        std::fs::write(d.path().join("settings.json"), "not json").unwrap();
        assert!(claude_settings_in(d.path()).is_none());
        std::fs::write(d.path().join("settings.json"), r#"{"permissions":{"allow":["a","b"]}}"#).unwrap();
        assert!(claude_settings_in(d.path()).is_some_and(|c| c.detail.contains("2 allow rule(s) that also apply")));
    }

    #[test]
    fn missing_tools_say_how_to_install_them() {
        let c = tool("nightporter-no-such-tool", "--version", "install it", true);
        assert!(!c.ok && c.optional && c.detail == "not found on PATH" && c.fix == "install it");
        let git = tool("git", "--version", "", false);
        assert!(git.ok && git.detail.starts_with("git version") && git.fix.is_empty(), "{}", git.detail);
    }

    #[test]
    fn collects_config_driven_checks() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        let code = d.path().join("code");
        std::fs::write(
            paths.config_file(),
            format!(
                "code_dir = {:?}\nmin_free_gb = 0\n[[accounts]]\nforge = \"github\"\n[[accounts]]\nforge = \"forgejo\"\nhost = \"git.example.org\"\n",
                code.display().to_string()
            ),
        )
        .unwrap();
        let repo = crate::forge::RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        std::fs::create_dir_all(paths.repo_file(&repo).parent().unwrap()).unwrap();
        std::fs::write(paths.repo_file(&repo), "agent = \"codex\"\n").unwrap();
        std::fs::write(paths.repos_dir().join("github/o/broken.toml"), "agent = \"nope\"\n").unwrap();
        let seen = RefCell::new(Vec::new());
        let probe = |f: &[(ForgeKind, String)], a: &[AgentKind]| {
            seen.borrow_mut().push((f.to_vec(), a.to_vec()));
            vec![check("gh", true, false)]
        };
        let checks = collect(&paths, probe, Some(check("claude settings", false, true)));
        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["config", "gh", "code dir", "disk", "claude settings", "daemon"]);
        assert_eq!(
            seen.borrow()[0],
            (
                vec![(ForgeKind::GitHub, "github.com".into()), (ForgeKind::Forgejo, "git.example.org".into())],
                vec![AgentKind::Claude, AgentKind::Codex]
            ),
            "agents from repo files are probed too; unreadable ones are skipped"
        );
        let by = |n: &str| checks.iter().find(|c| c.name == n).unwrap();
        assert!(!by("code dir").ok, "code dir doesn't exist yet");
        assert!(by("disk").ok && by("disk").detail.ends_with("GB free (min 0)"));
        assert!(!by("daemon").ok && by("daemon").optional != crate::service::installed(), "required once installed as a service");

        std::fs::create_dir_all(&code).unwrap();
        std::fs::write(paths.config_file(), format!("code_dir = {:?}\n", code.display().to_string())).unwrap();
        let checks = collect(&paths, |_, _| vec![], None);
        assert!(checks.iter().any(|c| c.name == "code dir" && c.ok));
        assert!(checks.iter().any(|c| c.name == "accounts" && !c.ok && c.fix.contains("nightporter setup")));

        std::fs::write(paths.config_file(), "nope = [").unwrap();
        let checks = collect(&paths, |_, _| panic!("no tools are probed without a config"), None);
        assert_eq!(checks.iter().map(|c| (c.name.as_str(), c.ok)).collect::<Vec<_>>(), [("config", false), ("daemon", false)]);
        assert!(checks[0].fix.starts_with("edit "));
    }

    #[test]
    fn walks_nested_repo_files() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(walk_repo_files(&d.path().join("missing")).unwrap(), Vec::<std::path::PathBuf>::new());
        std::fs::create_dir_all(d.path().join("github/o")).unwrap();
        std::fs::write(d.path().join("github/o/r.toml"), "").unwrap();
        std::fs::write(d.path().join("github/o/notes.txt"), "").unwrap();
        assert_eq!(walk_repo_files(d.path()).unwrap(), [d.path().join("github/o/r.toml")]);
    }
}
