//! First-run wizard. Plain line-based prompts: works over SSH, in any terminal, with screen readers.

use crate::config::{AgentKind, Config, Level, TEMPLATE, Window, edit_file};
use crate::forge::ForgeKind;
use crate::paths::Paths;
use anyhow::{Context, Result, bail};
use std::io::{BufRead, Write};

fn ask(q: &str, default: &str) -> Result<String> {
    print!("{q}{}: ", if default.is_empty() { String::new() } else { format!(" [{default}]") });
    std::io::stdout().flush()?;
    let mut s = String::new();
    if std::io::stdin().lock().read_line(&mut s)? == 0 {
        bail!("setup cancelled (end of input)");
    }
    let s = s.trim();
    Ok(if s.is_empty() { default.to_string() } else { s.to_string() })
}

/// Ask until `parse` accepts the answer; explain what was wrong each time.
fn ask_valid<T>(q: &str, default: &str, parse: impl Fn(&str) -> Result<T>) -> Result<T> {
    loop {
        match parse(&ask(q, default)?) {
            Ok(v) => return Ok(v),
            Err(e) => println!("  {e:#}. Please try again."),
        }
    }
}

fn yes(q: &str, default: bool) -> Result<bool> {
    ask_valid(&format!("{q} (y/n)"), if default { "y" } else { "n" }, |a| match a.to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => bail!("answer y or n"),
    })
}

fn parse_level(a: &str) -> Result<Level> {
    toml::Value::String(a.trim().to_ascii_lowercase())
        .try_into()
        .map_err(|_| anyhow::anyhow!("{a:?} is not one of observe, suggest, draft, auto"))
}

/// `mon-fri 08:00-09:00,17:00-22:00`, or empty / `always` for no schedule.
fn parse_schedule(a: &str) -> Result<Option<Window>> {
    let a = a.trim();
    if a.is_empty() || a.eq_ignore_ascii_case("always") {
        return Ok(None);
    }
    let (days, ranges) = a.split_once(' ').context("use `<days> <HH:MM-HH:MM,...>`, e.g. `mon-fri 08:00-09:00,17:00-22:00`")?;
    let w = Window { days: days.into(), windows: ranges.split(',').map(|r| r.trim().to_string()).collect(), tz: None };
    crate::schedule::validate(std::slice::from_ref(&w))?;
    Ok(Some(w))
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
    let f = ask_valid("Choose", "1", |a| if matches!(a, "1" | "2" | "3") { Ok(a.to_string()) } else { bail!("choose 1, 2 or 3") })?;
    let mut forges: Vec<(ForgeKind, String)> = Vec::new();
    if f == "1" || f == "3" {
        forges.push((ForgeKind::GitHub, ask("GitHub host", "github.com")?.to_ascii_lowercase()));
    }
    if f == "2" || f == "3" {
        let hosts = ask_valid("GitLab host(s), comma-separated (e.g. gitlab.com, git.drupalcode.org)", "gitlab.com", |a| {
            let v: Vec<String> = a.split(',').map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect();
            if v.is_empty() {
                bail!("enter at least one host")
            }
            Ok(v)
        })?;
        forges.extend(hosts.into_iter().map(|h| (ForgeKind::GitLab, h)));
    }

    // 2. Agents
    println!("\n2. Which agent by default?  1) Claude Code  2) OpenCode");
    let agent = ask_valid("Choose", "1", |a| match a {
        "1" => Ok(AgentKind::Claude),
        "2" => Ok(AgentKind::OpenCode),
        _ => bail!("choose 1 or 2"),
    })?;

    // 3. Tools
    println!("\n3. Checking tools…");
    let checks = crate::doctor::tools(&forges, &[agent]);
    crate::doctor::print(&checks);
    if checks.iter().any(|c| !c.ok && !c.optional) {
        println!("  Some tools are missing or logged out. Nothing is installed for you: run the fix commands, then `nightporter doctor`.");
    }

    // 4. Code dir + defaults
    let code_dir = ask_valid("\n4. Where are your clones?", "~/coding", |a| {
        if crate::paths::expand_tilde(a).is_dir() { Ok(a.to_string()) } else { bail!("{a} is not a directory") }
    })?;
    println!(
        "\n5. Default autonomy:\n  observe = notify only\n  suggest = commit locally, you copy/paste\n  draft   = you approve each push/reply (recommended)\n  auto    = push/reply on its own (checks still apply)"
    );
    let level = ask_valid("Level", "draft", parse_level)?;
    let sched = ask_valid("\n6. Active hours, e.g. `mon-fri 08:00-09:00,17:00-22:00` (empty = always)", "", parse_schedule)?;

    edit_file(&paths.config_file(), |d| {
        d["code_dir"] = toml_edit::value(&code_dir);
        // Keep accounts that are already there (and their token settings); add new ones.
        let accounts = d["accounts"].or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
        if let Some(aot) = accounts.as_array_of_tables_mut() {
            for (f, host) in &forges {
                let default_host = if *f == ForgeKind::GitHub { "github.com" } else { "gitlab.com" };
                let exists = aot.iter().any(|t| {
                    t.get("forge").and_then(|v| v.as_str()) == Some(f.as_str())
                        && t.get("host").and_then(|v| v.as_str()).unwrap_or(default_host).eq_ignore_ascii_case(host)
                });
                if !exists {
                    let mut t = toml_edit::Table::new();
                    t["forge"] = toml_edit::value(f.as_str());
                    if host != default_host {
                        t["host"] = toml_edit::value(host.as_str());
                    }
                    aot.push(t);
                }
            }
        }
        d["defaults"]["agent"] = toml_edit::value(if agent == AgentKind::Claude { "claude" } else { "opencode" });
        d["defaults"]["level"] = toml_edit::value(level.as_str());
        if let Some(w) = &sched {
            let mut t = toml_edit::Table::new();
            t["days"] = toml_edit::value(&w.days);
            t["windows"] = toml_edit::value(w.windows.iter().map(String::as_str).collect::<toml_edit::Array>());
            let mut aot = toml_edit::ArrayOfTables::new();
            aot.push(t);
            d["defaults"]["schedule"] = toml_edit::Item::ArrayOfTables(aot);
        }
    })?;

    // 7. Subscribe
    let cfg = Config::load(paths)?;
    let clones = crate::repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts);
    let list: Vec<_> = clones.iter().collect();
    println!("\n7. Found {} repos. Subscribe to which? (numbers, comma-separated; empty = none, add later in the TUI)", list.len());
    for (i, (r, p)) in list.iter().enumerate() {
        println!("  {:>3}) {:<50} {}", i + 1, format!("{}/{}", r.host, r.path), p.display());
    }
    let picks = ask_valid("Subscribe", "", |a| {
        a.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(|x| match x.parse::<usize>() {
                Ok(n) if (1..=list.len()).contains(&n) => Ok(n - 1),
                _ => bail!("{x:?} is not a number between 1 and {}", list.len()),
            })
            .collect::<Result<Vec<_>>>()
    })?;
    for i in picks {
        let (r, _) = list[i];
        crate::actions::set_subscribed(paths, r, true)?;
        println!("  subscribed {}/{}", r.host, r.path);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_answers() {
        assert_eq!(parse_level(" Auto ").unwrap(), Level::Auto);
        assert!(parse_level("yolo").is_err());
        assert!(parse_schedule("").unwrap().is_none());
        assert!(parse_schedule("always").unwrap().is_none());
        assert_eq!(parse_schedule("mon-fri 08:00-09:00, 17:00-22:00").unwrap().unwrap().windows.len(), 2);
        assert!(parse_schedule("daily").is_err());
        assert!(parse_schedule("mon-fri 8-9").is_err());
    }
}
