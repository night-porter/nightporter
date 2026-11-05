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

pub fn tools(forges: &[(ForgeKind, String)], agents: &[AgentKind]) -> Vec<Check> {
    let brew = cfg!(target_os = "macos");
    let install = |pkg: &str, other: &str| if brew { format!("brew install {pkg}") } else { other.to_string() };
    let mut out = vec![tool("git", "--version", &install("git", "install git with your package manager"), false)];
    if forges.iter().any(|(f, _)| *f == ForgeKind::GitHub) {
        out.push(tool("gh", "--version", &install("gh", "see https://cli.github.com"), false));
    }
    if forges.iter().any(|(f, _)| *f == ForgeKind::GitLab) {
        out.push(tool("glab", "--version", &install("glab", "see https://gitlab.com/gitlab-org/cli"), false));
    }
    for (f, host) in forges {
        match f {
            ForgeKind::GitHub => out.push(auth("gh", host)),
            ForgeKind::GitLab => out.push(auth("glab", host)),
        }
    }
    for a in agents {
        let fix = match a {
            AgentKind::Claude => "npm install -g @anthropic-ai/claude-code   (then run `claude` once to log in)",
            AgentKind::OpenCode => "curl -fsSL https://opencode.ai/install | bash   (or brew install opencode)",
        };
        out.push(tool(a.bin(), "--version", fix, false));
    }
    out.push(tool("gitleaks", "version", &install("gitleaks", "see https://github.com/gitleaks/gitleaks"), true));
    out
}

pub fn run(paths: &Paths) -> bool {
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
        let mut agents = vec![cfg.resolve(None).map(|s| s.agent).unwrap_or(AgentKind::Claude)];
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
    let running = paths.daemon_running();
    checks.push(Check {
        name: "daemon".into(),
        ok: running,
        optional: true,
        detail: if running { "running".into() } else { "not running".into() },
        fix: "nightporter service install   (or run `nightporter daemon`)".into(),
    });

    print(&checks);
    checks.iter().all(|c| c.ok || c.optional)
}

/// Text markers, not colour, so it reads the same in any terminal or screen reader.
pub fn print(checks: &[Check]) {
    for c in checks {
        let mark = if c.ok {
            "[ok]  "
        } else if c.optional {
            "[warn]"
        } else {
            "[FAIL]"
        };
        println!("{mark} {:<22} {}", c.name, c.detail);
        if !c.ok {
            println!("       fix: {}", c.fix);
        }
    }
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
