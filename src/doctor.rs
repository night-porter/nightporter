//! `nightporter doctor` and the tool checks used by the setup wizard.

use crate::config::{AgentKind, Config};
use crate::forge::ForgeKind;
use crate::paths::Paths;
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
    let checks = collect(paths, tools, claude_user_settings_check());
    print(&checks);
    checks.iter().all(|c| c.ok || c.optional)
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
        if h != crate::config::HandoffAgent::Job && !agents.iter().any(|a| a.bin() == h.bin()) {
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
    checks.push(Check {
        name: "daemon".into(),
        ok: running,
        optional: true,
        detail: if running { "running".into() } else { "not running".into() },
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
    fn flags_personal_claude_allows_and_plugins() {
        let v = serde_json::json!({"permissions": {"allow": ["Bash(npm:*)"]}, "enabledPlugins": {"x@y": true, "z@y": false}});
        let c = super::claude_settings_finding(&v);
        assert!(!c.ok && c.optional && c.detail.contains("1 allow rule(s) and 1 plugin(s)"), "{}", c.detail);
        assert!(super::claude_settings_finding(&serde_json::json!({})).ok);
    }

    use super::*;
    use std::cell::RefCell;

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
        assert!(!by("daemon").ok && by("daemon").optional);

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
