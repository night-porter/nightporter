//! First-run wizard. In a terminal it uses interactive prompts (editable text with a cursor,
//! menus, a searchable checklist of repos). Plain line-based prompts are kept for screen readers,
//! `TERM=dumb`, pipes and scripts, or whenever `NIGHTPORTER_PLAIN` is set.

use crate::config::{AgentKind, Config, Level, TEMPLATE, Window, edit_file};
use crate::forge::ForgeKind;
use crate::paths::Paths;
use anyhow::{Context, Result, bail};
use std::io::{BufRead, IsTerminal, Write};

/// Interactive prompts, or the plain fallback.
fn fancy() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::env::var("TERM").map_or(true, |t| t != "dumb")
        && std::env::var_os("NIGHTPORTER_PLAIN").is_none()
}

/// A leading newline in a question becomes a blank line before the prompt, not part of it.
fn label(q: &str) -> &str {
    q.strip_prefix('\n').inspect(|_| println!()).unwrap_or(q)
}

/// Ctrl-C / Esc in a prompt stop the wizard, like end of input does in plain mode.
fn answer<T>(r: std::result::Result<T, inquire::InquireError>) -> Result<T> {
    use inquire::InquireError::{OperationCanceled, OperationInterrupted};
    r.map_err(|e| match e {
        OperationCanceled | OperationInterrupted => anyhow::anyhow!("setup cancelled"),
        e => e.into(),
    })
}

fn ask_plain(q: &str, default: &str) -> Result<String> {
    print!("{q}{}: ", if default.is_empty() { String::new() } else { format!(" [{default}]") });
    std::io::stdout().flush()?;
    let mut s = String::new();
    if std::io::stdin().lock().read_line(&mut s)? == 0 {
        bail!("setup cancelled (end of input)");
    }
    let s = s.trim();
    Ok(if s.is_empty() { default.to_string() } else { s.to_string() })
}

fn ask(q: &str, default: &str) -> Result<String> {
    if !fancy() {
        return ask_plain(q, default);
    }
    // The default is pre-filled, so it can be edited instead of retyped.
    let s = answer(inquire::Text::new(label(q)).with_initial_value(default).with_help_message("Enter to accept, Esc to stop").prompt())?;
    Ok(s.trim().to_string())
}

/// Ask until `parse` accepts the answer; explain what was wrong each time.
fn ask_valid<T>(q: &str, default: &str, parse: impl Fn(&str) -> Result<T>) -> Result<T> {
    let mut last = default.to_string();
    loop {
        last = ask(q, &last)?;
        match parse(&last) {
            Ok(v) => return Ok(v),
            Err(e) => println!("  {e:#}. Please try again."),
        }
    }
}

pub fn yes(q: &str, default: bool) -> Result<bool> {
    if fancy() {
        return answer(inquire::Confirm::new(label(q)).with_default(default).prompt());
    }
    ask_valid(&format!("{q} (y/n)"), if default { "y" } else { "n" }, |a| match a.to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => bail!("answer y or n"),
    })
}

/// Pick one of `options`; returns its index.
fn choose(q: &str, options: &[&str], default: usize) -> Result<usize> {
    if fancy() {
        let picked = answer(inquire::Select::new(label(q), options.to_vec()).with_starting_cursor(default).prompt())?;
        return Ok(options.iter().position(|o| *o == picked).unwrap_or(default));
    }
    println!("{q}");
    for (i, o) in options.iter().enumerate() {
        println!("  {}) {o}", i + 1);
    }
    ask_valid("Choose", &(default + 1).to_string(), |a| match a.parse::<usize>() {
        Ok(n) if (1..=options.len()).contains(&n) => Ok(n - 1),
        _ => bail!("choose a number from 1 to {}", options.len()),
    })
}

/// `mon-fri 08:00-09:00,17:00-22:00`, or empty / `always` for no schedule.
pub fn parse_schedule(a: &str) -> Result<Option<Window>> {
    let a = a.trim();
    if a.is_empty() || a.eq_ignore_ascii_case("always") {
        return Ok(None);
    }
    let (days, ranges) = a.split_once(' ').context("use `<days> <HH:MM-HH:MM,...>`, e.g. `mon-fri 08:00-09:00,17:00-22:00`")?;
    let w = Window { days: days.into(), windows: ranges.split(',').map(|r| r.trim().to_string()).collect(), tz: None };
    crate::schedule::validate(std::slice::from_ref(&w))?;
    Ok(Some(w))
}

/// Step 1: which forges and hosts.
fn ask_forges() -> Result<Vec<(ForgeKind, String)>> {
    let f = choose("\n1. Which forges?", &["GitHub", "GitLab (gitlab.com or self-hosted)", "Both"], 0)?;
    let mut forges: Vec<(ForgeKind, String)> = Vec::new();
    if f != 1 {
        forges.push((ForgeKind::GitHub, ask("GitHub host", "github.com")?.to_ascii_lowercase()));
    }
    if f != 0 {
        let hosts = ask_valid("GitLab host(s), comma-separated (e.g. gitlab.com, git.drupalcode.org)", "gitlab.com", |a| {
            let v: Vec<String> = a.split(',').map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect();
            if v.is_empty() {
                bail!("enter at least one host")
            }
            Ok(v)
        })?;
        forges.extend(hosts.into_iter().map(|h| (ForgeKind::GitLab, h)));
    }
    Ok(forges)
}

/// Step 7: pick which of the clones found under the code dir to watch. Already-subscribed repos
/// start ticked; unticking one unsubscribes it.
fn subscribe_repos(paths: &Paths) -> Result<()> {
    let cfg = Config::load(paths)?;
    let clones: Vec<_> = crate::repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts).into_keys().collect();
    if clones.is_empty() {
        println!("\n7. No clones found under {}. Add repos later in the TUI (tab 4).", cfg.code_dir().display());
        return Ok(());
    }
    let was: Vec<bool> = clones.iter().map(|r| cfg.settings_for(paths, r).is_ok_and(|s| s.subscribed)).collect();
    let labels: Vec<String> = clones.iter().map(|r| format!("{}/{}", r.host, r.path)).collect();
    let picked: Vec<usize> = if fancy() {
        let on: Vec<usize> = (0..clones.len()).filter(|&i| was[i]).collect();
        println!();
        answer(
            inquire::MultiSelect::new(&format!("7. Found {} repos. Which should Night Porter watch?", clones.len()), labels.clone())
                .with_default(&on)
                .with_page_size(15)
                .with_help_message("type to search, ↑↓ move, space toggle, → all, ← none, Enter confirm")
                .raw_prompt(),
        )?
        .into_iter()
        .map(|o| o.index)
        .collect()
    } else {
        println!("\n7. Found {} repos. Subscribe to which? (numbers, comma-separated; empty = keep as is)", clones.len());
        for (i, l) in labels.iter().enumerate() {
            println!("  {:>3}) [{}] {l}", i + 1, if was[i] { "x" } else { " " });
        }
        let picks = ask_valid("Subscribe", "", |a| {
            a.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(|x| match x.parse::<usize>() {
                    Ok(n) if (1..=clones.len()).contains(&n) => Ok(n - 1),
                    _ => bail!("{x:?} is not a number between 1 and {}", clones.len()),
                })
                .collect::<Result<Vec<_>>>()
        })?;
        // Plain mode only adds: an empty answer must not unsubscribe everything.
        (0..clones.len()).filter(|i| was[*i] || picks.contains(i)).collect()
    };
    for (i, r) in clones.iter().enumerate() {
        let on = picked.contains(&i);
        if on != was[i] {
            crate::actions::set_subscribed(paths, r, on)?;
            println!("  {} {}", if on { "subscribed" } else { "unsubscribed" }, labels[i]);
        }
    }
    Ok(())
}

pub fn run(paths: &Paths) -> Result<()> {
    paths.ensure()?;
    if std::env::var_os("NO_COLOR").is_some() {
        inquire::set_global_render_config(inquire::ui::RenderConfig::empty());
    }
    println!("Night Porter setup. Enter accepts each answer, Esc stops.\n");
    if paths.config_file().exists() && !yes(&format!("{} exists. Update it?", paths.config_file().display()), true)? {
        return Ok(());
    }
    if !paths.config_file().exists() {
        std::fs::write(paths.config_file(), TEMPLATE)?;
    }

    let forges = ask_forges()?;

    // 2. Agents
    let agent = [AgentKind::Claude, AgentKind::OpenCode][choose("\n2. Which agent by default?", &["Claude Code", "OpenCode"], 0)?];

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
    let levels = [
        (Level::Observe, "observe   notify only"),
        (Level::Suggest, "suggest   commit locally, you copy/paste"),
        (Level::Draft, "draft     you approve each push/reply (recommended)"),
        (Level::Auto, "auto      push/reply on its own (checks still apply)"),
    ];
    let level = levels[choose("\n5. Default autonomy level?", &levels.map(|l| l.1), 2)?].0;
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

    subscribe_repos(paths)?;

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
        assert!(parse_schedule("").unwrap().is_none());
        assert!(parse_schedule("always").unwrap().is_none());
        assert_eq!(parse_schedule("mon-fri 08:00-09:00, 17:00-22:00").unwrap().unwrap().windows.len(), 2);
        assert!(parse_schedule("daily").is_err());
        assert!(parse_schedule("mon-fri 8-9").is_err());
    }
}
