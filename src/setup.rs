//! First-run wizard. Plain line-based prompts: works over SSH, in any terminal, with screen readers.

use crate::config::{AgentKind, Config, TEMPLATE, edit_file};
use crate::forge::ForgeKind;
use crate::paths::Paths;
use anyhow::Result;
use std::io::{BufRead, Write};

fn ask(q: &str, default: &str) -> Result<String> {
    print!("{q}{}: ", if default.is_empty() { String::new() } else { format!(" [{default}]") });
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    let s = s.trim();
    Ok(if s.is_empty() { default.to_string() } else { s.to_string() })
}

fn yes(q: &str, default: bool) -> Result<bool> {
    let a = ask(&format!("{q} (y/n)"), if default { "y" } else { "n" })?;
    Ok(a.to_ascii_lowercase().starts_with('y'))
}

pub fn run(paths: &Paths) -> Result<()> {
    paths.ensure()?;
    println!("Night Porter setup. Press Enter to accept the [default].\n");
    if paths.config_file().exists() && !yes(&format!("{} exists. Update it?", paths.config_file().display()), true)? {
        return Ok(());
    }
    if !paths.config_file().exists() {
        std::fs::write(paths.config_file(), TEMPLATE)?;
    }

    // 1. Forges
    println!("\n1. Which forges?  1) GitHub  2) GitLab  3) both");
    let f = ask("Choose", "1")?;
    let mut forges: Vec<(ForgeKind, String)> = Vec::new();
    if f == "1" || f == "3" {
        forges.push((ForgeKind::GitHub, ask("GitHub host", "github.com")?));
    }
    if f == "2" || f == "3" {
        let hosts = ask("GitLab host(s), comma-separated (e.g. gitlab.com, git.drupalcode.org)", "gitlab.com")?;
        forges.extend(hosts.split(',').map(|h| (ForgeKind::GitLab, h.trim().to_string())).filter(|(_, h)| !h.is_empty()));
    }

    // 2. Agents
    println!("\n2. Which agent by default?  1) Claude Code  2) OpenCode");
    let agent = if ask("Choose", "1")? == "2" { AgentKind::OpenCode } else { AgentKind::Claude };

    // 3. Tools
    println!("\n3. Checking tools…");
    let checks = crate::doctor::tools(&forges, &[agent]);
    for c in &checks {
        let mark = if c.ok {
            "[ok]  "
        } else if c.optional {
            "[warn]"
        } else {
            "[FAIL]"
        };
        println!("  {mark} {:<22} {}", c.name, c.detail);
        if !c.ok {
            println!("         fix: {}", c.fix);
        }
    }
    if checks.iter().any(|c| !c.ok && !c.optional) {
        println!("  Some tools are missing or logged out. Nothing is installed for you: run the fix commands, then `nightporter doctor`.");
    }

    // 4. Code dir + defaults
    let code_dir = ask("\n4. Where are your clones?", "~/coding")?;
    println!(
        "\n5. Default autonomy:\n  observe = notify only\n  suggest = commit locally, you copy/paste\n  draft   = you approve each push/reply (recommended)\n  auto    = push/reply on its own (checks still apply)"
    );
    let level = ask("Level", "draft")?;
    let sched = ask("\n6. Active hours, e.g. `mon-fri 08:00-09:00,17:00-22:00` (empty = always)", "")?;

    edit_file(&paths.config_file(), None, |d| {
        d["code_dir"] = toml_edit::value(&code_dir);
        let mut accounts = toml_edit::ArrayOfTables::new();
        for (f, host) in &forges {
            let mut t = toml_edit::Table::new();
            t["forge"] = toml_edit::value(f.as_str());
            if !matches!(host.as_str(), "github.com" | "gitlab.com") {
                t["host"] = toml_edit::value(host.as_str());
            }
            accounts.push(t);
        }
        d["accounts"] = toml_edit::Item::ArrayOfTables(accounts);
        d["defaults"]["agent"] = toml_edit::value(if agent == AgentKind::Claude { "claude" } else { "opencode" });
        d["defaults"]["level"] = toml_edit::value(&level);
        if let Some((days, windows)) = sched.trim().split_once(' ') {
            let mut w = toml_edit::Table::new();
            w["days"] = toml_edit::value(days);
            let mut arr = toml_edit::Array::new();
            for r in windows.split(',') {
                arr.push(r.trim());
            }
            w["windows"] = toml_edit::value(arr);
            let mut aot = toml_edit::ArrayOfTables::new();
            aot.push(w);
            d["defaults"]["schedule"] = toml_edit::Item::ArrayOfTables(aot);
        }
    })?;

    // 7. Subscribe
    let cfg = Config::load(paths)?;
    let clones = crate::repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts);
    let list: Vec<_> = clones.iter().collect();
    println!("\n7. Found {} repos. Subscribe to which? (numbers, comma-separated; empty = none, add later in the TUI)", list.len());
    for (i, (r, p)) in list.iter().enumerate() {
        println!("  {:>3}) {:<45} {}", i + 1, r.path, p.display());
    }
    let picks = ask("Subscribe", "")?;
    for n in picks.split(',').filter_map(|s| s.trim().parse::<usize>().ok()) {
        if let Some((r, _)) = list.get(n.wrapping_sub(1)) {
            crate::actions::set_subscribed(paths, r, true)?;
            println!("  subscribed {}", r.path);
        }
    }

    // 8. Service
    if yes("\n8. Install the background service now?", true)? {
        match crate::service::install(&paths.log_file()) {
            Ok(m) => println!("  {m}"),
            Err(e) => println!("  could not install: {e:#}"),
        }
    }
    println!("\nDone. Open the TUI with `nightporter`, check health with `nightporter doctor`.");
    Ok(())
}
