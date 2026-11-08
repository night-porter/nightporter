//! Night Porter: handles your PRs while you're away.

mod actions;
mod agent;
mod checks;
mod config;
mod daemon;
mod doctor;
mod forge;
mod git;
mod handoff;
mod herdr;
mod http;
mod job;
mod notify;
mod packs;
mod paths;
mod prompts;
mod repos;
mod schedule;
mod service;
mod settings;
mod setup;
mod skill;
mod state;
mod trust;
mod tui;
mod update;
mod watch;

#[cfg(test)]
mod e2e_tests;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use forge::{ForgeKind, RepoId};
use paths::Paths;
use state::{Db, Disposition, JobStatus};

#[derive(Parser)]
#[command(name = "nightporter", version, about = "Handles your PRs while you're away.")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the terminal UI (default).
    Tui,
    /// Run the background daemon (normally started by the service).
    Daemon,
    /// First-run wizard.
    Setup,
    /// Check tools, logins, config and disk.
    Doctor,
    /// Plain-text summary of what needs you (script- and screen-reader-friendly).
    Status,
    /// Start the background daemon (it watches your PRs and runs jobs).
    Start,
    /// Stop the background daemon. Nothing is watched until `nightporter start`.
    Stop,
    /// Check for a newer Night Porter (asks GitHub for the latest release number) and print the upgrade command.
    Update,
    /// List watched repos, then how many other clones there are. Paged in a terminal (q quits).
    Repos {
        /// Only repos whose name contains this.
        search: Option<String>,
        /// Also list every clone that isn't watched.
        #[arg(long)]
        all: bool,
    },
    /// Everything Night Porter does on one repo: level, agent, prompt, reply style, trust, hours.
    Repo { repo: String },
    /// Subscribe to a repo (`owner/name`, or `host/owner/name` for non-default hosts).
    Subscribe { repo: String },
    /// Stop watching a repo.
    Unsubscribe { repo: String },
    /// Approve a job (push + post, or allow untrusted code to run).
    Approve {
        id: i64,
        /// Approve despite blocking check failures (secrets, protected paths, history).
        #[arg(long)]
        override_blocking: bool,
    },
    /// Reject a job: nothing is pushed or posted.
    Reject { id: i64 },
    /// Put jobs away without acting on them (nothing is pushed or posted). Undo with `restore`.
    Dismiss {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Bring a dismissed job back to where it was.
    Restore { id: i64 },
    /// Delete finished or dismissed jobs for good, with their logs. Can't be undone.
    Purge {
        #[arg(required = true)]
        ids: Vec<i64>,
        #[arg(long)]
        yes: bool,
    },
    /// Re-run a finished or failed job's requests as a new job.
    Retry { id: i64 },
    /// Take over a job interactively in herdr or Rex (opens it with a briefed agent).
    Handoff { id: i64 },
    /// Show a job: checks, replies, diff, log.
    Show { id: i64 },
    /// Inbox decision for an untrusted author's event.
    Inbox {
        #[command(subcommand)]
        cmd: InboxCmd,
    },
    /// Stop starting new jobs (running jobs finish).
    Pause,
    /// Start new jobs again.
    Resume,
    /// Remove idle worktrees (`--all`: every worktree not in use).
    Gc {
        #[arg(long)]
        all: bool,
    },
    /// Store a personal access token in the OS keychain (read from stdin).
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// Install or remove the background service (launchd / systemd --user).
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
    /// Manage marketplaces.
    Market {
        #[command(subcommand)]
        cmd: MarketCmd,
    },
    /// Manage packs.
    Pack {
        #[command(subcommand)]
        cmd: PackCmd,
    },
    /// Print one line per job or inbox change until interrupted (for scripts and status bars).
    Events {
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Wait for a job: exit 0 done, 1 failed or rejected, 2 needs you, 3 timed out.
    Wait {
        id: i64,
        #[arg(long, default_value = "30m")]
        timeout: String,
    },
    /// Teach coding agents (Claude Code) how to work with Night Porter.
    Skill {
        #[command(subcommand)]
        cmd: SkillCmd,
    },
}

#[derive(Subcommand)]
enum SkillCmd {
    /// Print the guide for agents (always matches this version).
    Show,
    /// Install a small Claude Code skill that runs `nightporter skill show`.
    Setup,
    /// Remove that skill.
    Remove,
}

#[derive(Subcommand)]
enum InboxCmd {
    /// List events from unknown authors that are waiting for a decision.
    List {
        /// List the events you ignored instead (bring one back with `inbox restore`).
        #[arg(long)]
        ignored: bool,
    },
    /// Put an ignored event back in the inbox.
    Restore { event: String },
    /// Let this one event through (the author stays untrusted).
    AllowOnce { event: String },
    /// Trust the author from now on, on one repo (`--repo`) or everywhere (`--global`).
    Allow {
        user: String,
        #[arg(long, required_unless_present = "global", conflicts_with = "global")]
        repo: Option<String>,
        #[arg(long)]
        global: bool,
    },
    /// Never act on this author, on one repo (`--repo`) or everywhere (`--global`).
    Deny {
        user: String,
        #[arg(long, required_unless_present = "global", conflicts_with = "global")]
        repo: Option<String>,
        #[arg(long)]
        global: bool,
    },
    /// Drop this one event.
    Ignore { event: String },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Store a token for `github`, `gitlab` or `forgejo` (optionally a self-hosted host) in the keychain.
    Set { forge: String, host: Option<String> },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Start the daemon at login (launchd on macOS, systemd --user on Linux).
    Install,
    /// Stop the daemon and remove the service.
    Uninstall,
}

#[derive(Subcommand)]
enum MarketCmd {
    /// Add a marketplace (a git repo with a marketplace.toml).
    Add { url: String },
    /// List marketplaces and their packs.
    List,
    /// Refresh all marketplaces.
    Update,
}

#[derive(Subcommand)]
enum PackCmd {
    /// List installed packs and their pinned commits.
    List,
    /// Install from a marketplace by name, or from a git URL (`url@rev`).
    Add {
        name: String,
        #[arg(long)]
        yes: bool,
    },
    Update {
        name: String,
        #[arg(long)]
        yes: bool,
    },
    /// Uninstall a pack.
    Remove { name: String },
    /// Print an env preset (`pack/env`) to paste into a repo file.
    Env { name: String },
}

fn main() -> Result<()> {
    // Behave like other CLIs when piped into `head`: exit quietly instead of panicking.
    #[cfg(unix)]
    // SAFETY: called before any threads exist; restores the default disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let paths = Paths::detect()?;
    let cmd = cli.cmd.unwrap_or(Cmd::Tui);
    // A job's agent may look, never act: it must not approve its own work or change trust.
    if std::env::var_os("NIGHTPORTER_JOB").is_some() && !read_only(&cmd) {
        bail!("not available inside a Night Porter job; only read-only commands (status, show, inbox list, ...) are");
    }
    let cmd_name = match &cmd {
        Cmd::Subscribe { .. } => "subscribe",
        _ => "",
    };
    let rt = || tokio::runtime::Builder::new_multi_thread().enable_all().build();
    match cmd {
        Cmd::Tui => {
            paths.ensure()?;
            tui::run(&paths)
        }
        Cmd::Events { json } => watch::events(&paths, json),
        Cmd::Wait { id, timeout } => {
            let code = watch::wait(&paths, id, config::parse_duration(&timeout)?)?;
            std::process::exit(code)
        }
        Cmd::Skill { cmd } => cmd_skill(&cmd),
        Cmd::Daemon => {
            init_tracing();
            rt()?.block_on(daemon::run(paths))
        }
        Cmd::Setup => setup::run(&paths),
        Cmd::Doctor => {
            if !doctor::run(&paths) {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Status => status(&paths),
        Cmd::Start | Cmd::Stop => {
            println!("{}", if matches!(cmd, Cmd::Start) { service::start(&paths)? } else { service::stop(&paths)? });
            Ok(())
        }
        Cmd::Update => cmd_update(&db(&paths)?),
        Cmd::Repos { search, all } => cmd_repos(&paths, search.as_deref(), all),
        Cmd::Repo { repo } => cmd_repo(&paths, &repo),
        c @ (Cmd::Dismiss { .. } | Cmd::Restore { .. } | Cmd::Purge { .. }) => cmd_archive(&paths, c),
        Cmd::Subscribe { repo } | Cmd::Unsubscribe { repo } => {
            let on = matches!(cmd_name, "subscribe");
            let r = resolve_repo(&paths, &repo)?;
            actions::set_subscribed(&paths, &r, on)?;
            println!("{} {}/{}", if on { "subscribed to" } else { "unsubscribed from" }, r.host, r.path);
            Ok(())
        }
        Cmd::Approve { id, override_blocking } => {
            println!("{}", actions::approve(&db(&paths)?, id, override_blocking)?);
            Ok(())
        }
        Cmd::Reject { id } => {
            actions::reject(&db(&paths)?, id)?;
            println!("job {id} rejected: nothing will be pushed or posted");
            Ok(())
        }
        Cmd::Retry { id } => {
            println!("re-queued {} event(s)", actions::retry(&db(&paths)?, id)?);
            Ok(())
        }
        Cmd::Handoff { id } => {
            let db = db(&paths)?;
            let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
            println!("{}", handoff::take_over(&paths, &db, &j)?);
            Ok(())
        }
        Cmd::Show { id } => {
            let db = db(&paths)?;
            let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
            print!("{}", printable(&tui::job_text(&db, &j)));
            Ok(())
        }
        Cmd::Inbox { cmd } => inbox(&paths, cmd),
        Cmd::Pause => {
            actions::set_paused(&db(&paths)?, true)?;
            println!("paused: no new jobs will start (running jobs finish)");
            Ok(())
        }
        Cmd::Resume => {
            actions::set_paused(&db(&paths)?, false)?;
            println!("resumed");
            Ok(())
        }
        Cmd::Gc { all } => rt()?.block_on(async {
            let cfg = config::Config::load(&paths)?;
            let ctx = daemon::offline_ctx(&paths, cfg).await?;
            job::gc(&ctx, all).await
        }),
        Cmd::Token { cmd: TokenCmd::Set { forge, host } } => cmd_token_set(&forge, host),
        Cmd::Service { cmd } => cmd_service(&paths, &cmd),
        Cmd::Market { cmd } => rt()?.block_on(market(&paths, cmd)),
        Cmd::Pack { cmd } => rt()?.block_on(pack(&paths, cmd)),
    }
}

/// Dismiss, restore and purge: clearing out jobs you won't act on.
fn cmd_archive(paths: &Paths, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Dismiss { ids } => {
            let db = db(paths)?;
            for id in ids {
                actions::dismiss(&db, id)?;
                println!("job {id} dismissed (undo: nightporter restore {id})");
            }
        }
        Cmd::Restore { id } => println!("job {id} is back: {}", actions::restore(&db(paths)?, id)?.label()),
        Cmd::Purge { ids, yes } => {
            let mut db = db(paths)?;
            let list = ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
            if !yes && !confirm(&format!("Delete job(s) {list} for good? This can't be undone"))? {
                return Ok(());
            }
            for id in ids {
                actions::purge(&mut db, id)?;
                println!("job {id} deleted");
            }
        }
        _ => unreachable!("only archive commands are routed here"),
    }
    Ok(())
}

fn cmd_skill(cmd: &SkillCmd) -> Result<()> {
    match cmd {
        SkillCmd::Show => print!("{}", skill::GUIDE),
        SkillCmd::Setup => println!("{}", skill::setup()?),
        SkillCmd::Remove => println!("{}", skill::remove()?),
    }
    Ok(())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("NIGHTPORTER_LOG").unwrap_or_else(|_| "nightporter=info".into()))
        // daemon.log is a file: no colour codes in it.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
}

fn cmd_repos(paths: &Paths, search: Option<&str>, all: bool) -> Result<()> {
    use std::fmt::Write;
    let cfg = config::Config::load(paths)?;
    let needle = search.unwrap_or_default().to_lowercase();
    let mut rows: Vec<_> = repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts)
        .into_iter()
        .filter(|(r, _)| format!("{}/{}", r.host, r.path).to_lowercase().contains(&needle))
        .map(|(r, p)| {
            let s = cfg.settings_for(paths, &r);
            (s.as_ref().is_ok_and(|s| s.subscribed), r, p, s)
        })
        .collect();
    // Watched first, then by name.
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
    let (on, off) = (rows.iter().filter(|r| r.0).count(), rows.iter().filter(|r| !r.0).count());
    let show_off = all || search.is_some();
    let paint = |code: &str, s: &str| if color_out() { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Watching {on} repo(s){}",
        if on == 0 { ": subscribe with `nightporter subscribe owner/name` or the TUI's Repos tab" } else { "" }
    );
    for (sub, r, p, s) in rows.iter().filter(|r| show_off || r.0) {
        let (level, agent) = match s {
            Ok(s) => (s.level.as_str().to_string(), s.agent.label().to_string()),
            Err(e) => ("ERROR".into(), format!("{e:#}")),
        };
        let mark = if *sub { paint("32", "[on] ") } else { paint("2", "[off]") };
        let _ = writeln!(out, "{mark} {:<45} {:<8} {:<12} {}", format!("{}/{}", r.host, r.path), level, agent, p.display());
    }
    if !show_off && off > 0 {
        let _ =
            writeln!(out, "\n{off} other clone(s) not watched: `nightporter repos --all` lists them, `nightporter repos <word>` searches.");
    }
    let _ = writeln!(out, "Details for one repo: `nightporter repo owner/name`.");
    page(&out)
}

/// Colour on stdout only for a terminal, and never with `NO_COLOR`.
fn color_out() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout()) && std::env::var_os("NO_COLOR").is_none()
}

/// Print, through `$PAGER` (else `less`) when it's taller than the terminal. q leaves the pager.
fn page(text: &str) -> Result<()> {
    use std::io::Write;
    let tall = std::io::IsTerminal::is_terminal(&std::io::stdout())
        && crossterm::terminal::size().is_ok_and(|(_, h)| text.lines().count() >= usize::from(h).saturating_sub(1));
    if tall {
        let pager = std::env::var("PAGER").ok().filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "less -FRX".into());
        let mut parts = pager.split_whitespace();
        if let Some(bin) = parts.next()
            && let Ok(mut child) = std::process::Command::new(bin).args(parts).stdin(std::process::Stdio::piped()).spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            child.wait()?;
            return Ok(());
        }
    }
    print!("{text}");
    Ok(())
}

fn cmd_repo(paths: &Paths, repo: &str) -> Result<()> {
    let cfg = config::Config::load(paths)?;
    let r = resolve_repo(paths, repo)?;
    let clone = repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts).remove(&r);
    page(&printable(&settings::repo_text(paths, &cfg, &r, clone.as_deref())?))
}

fn cmd_update(db: &Db) -> Result<()> {
    println!("You have Night Porter {}. Asking GitHub for the latest release…", update::CURRENT);
    let latest = tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(update::fetch_latest())?;
    db.set("latest_version", &latest)?;
    db.set("update_checked_at", &state::now().to_string())?;
    if update::newer(&latest, update::CURRENT) {
        println!("{latest} is out. Upgrade with:\n  {}", update::how_to_upgrade());
    } else {
        println!("You're up to date.");
    }
    Ok(())
}

fn cmd_token_set(forge: &str, host: Option<String>) -> Result<()> {
    let f = ForgeKind::parse(forge).context("forge must be github, gitlab or forgejo")?;
    let host = host.unwrap_or_else(|| f.default_host().into());
    eprint!("Paste the token for {host} and press Enter (input is hidden): ");
    let t = read_secret()?;
    if t.is_empty() {
        bail!("empty token");
    }
    http::store_token(f, &host, &t)?;
    eprintln!("\nstored in the OS keychain; set `token = \"keyring\"` on the account in config.toml");
    Ok(())
}

fn cmd_service(paths: &Paths, cmd: &ServiceCmd) -> Result<()> {
    println!(
        "{}",
        match cmd {
            ServiceCmd::Install => service::install(&paths.log_file())?,
            ServiceCmd::Uninstall => service::uninstall()?,
        }
    );
    Ok(())
}

/// The agent configured for a repo (Claude Code if the config can't be read).
/// Commands that only read state.
fn read_only(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::Status
            | Cmd::Show { .. }
            | Cmd::Repos { .. }
            | Cmd::Repo { .. }
            | Cmd::Doctor
            | Cmd::Events { .. }
            | Cmd::Wait { .. }
            | Cmd::Skill { cmd: SkillCmd::Show }
            | Cmd::Inbox { cmd: InboxCmd::List { .. } }
            | Cmd::Market { cmd: MarketCmd::List }
            | Cmd::Pack { cmd: PackCmd::List }
    )
}

#[must_use]
pub fn agent_for(paths: &Paths, repo: &RepoId) -> config::AgentKind {
    config::Config::load(paths).and_then(|c| c.settings_for(paths, repo)).map_or(config::AgentKind::Claude, |s| s.agent)
}

fn db(paths: &Paths) -> Result<Db> {
    Db::open(&paths.db())
}

/// `owner/name` → the matching cloned repo (any forge/host); `host/owner/name` to disambiguate.
fn resolve_repo(paths: &Paths, s: &str) -> Result<RepoId> {
    let cfg = config::Config::load(paths)?;
    let clones = repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts);
    let hits: Vec<_> = clones.keys().filter(|r| r.path == s || format!("{}/{}", r.host, r.path) == s).collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("{s} is not cloned under {}", cfg.code_dir().display()),
        many => bail!("{s} is ambiguous: {}", many.iter().map(|r| format!("{}/{}", r.host, r.path)).collect::<Vec<_>>().join(", ")),
    }
}

fn status(paths: &Paths) -> Result<()> {
    let db = db(paths)?;
    let up = paths.daemon_running();
    println!(
        "Night Porter: daemon {}{}",
        if up { "running (stop: nightporter stop)" } else { "NOT RUNNING (start: nightporter start)" },
        if actions::paused(&db) { ", PAUSED" } else { "" }
    );
    if let Some(b) = update::banner(&db) {
        println!("{b}");
    }
    let mut any = false;
    for j in db.recent_jobs(200)?.into_iter().filter(|j| j.status.needs_user() || j.status.is_active() || j.status == JobStatus::Failed) {
        any = true;
        println!(
            "job {:>4}  {:<22} {}#{}  {}",
            j.id,
            j.status.label(),
            j.repo,
            j.number,
            printable(j.summary.lines().next().unwrap_or_default())
        );
    }
    let inbox = db.events_with(Disposition::Inbox)?;
    for (e, _) in &inbox {
        any = true;
        println!("inbox {}  @{} on {}#{}: {}", e.id, e.author, e.repo, e.number, printable(e.body.lines().next().unwrap_or_default()));
    }
    if !any {
        println!("Nothing needs you.");
    }
    Ok(())
}

/// Forge text and diffs can carry terminal escape sequences (hiding lines, rewriting the
/// screen). Keep newlines and tabs, drop every other control character.
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect()
}

fn inbox(paths: &Paths, cmd: InboxCmd) -> Result<()> {
    let db = db(paths)?;
    let scope_repo = |r: &Option<String>| -> Result<Option<RepoId>> { r.as_deref().map(|r| resolve_repo(paths, r)).transpose() };
    match cmd {
        InboxCmd::Restore { event } => {
            actions::unignore(&db, &event)?;
            println!("{event} is back in the inbox");
        }
        InboxCmd::List { ignored } => {
            let list = if ignored { db.ignored_events()? } else { db.events_with(Disposition::Inbox)? };
            for (e, _) in list {
                println!(
                    "{}  @{} ({:?}) on {}#{}\n    {}",
                    e.id,
                    e.author,
                    e.role,
                    e.repo,
                    e.number,
                    printable(&e.body).replace('\n', "\n    ")
                );
            }
        }
        InboxCmd::AllowOnce { event } => {
            actions::allow_once(&db, &event)?;
            println!("{event} allowed once");
        }
        InboxCmd::Ignore { event } => {
            actions::ignore(&db, &event)?;
            println!("{event} ignored");
        }
        InboxCmd::Allow { user, repo, .. } => {
            let r = scope_repo(&repo)?;
            let scope = r.as_ref().map_or(actions::Scope::Global, actions::Scope::Repo);
            let n = actions::allow_always(paths, &db, &user, scope)?;
            println!("@{user} trusted {}; {n} waiting event(s) released", r.map_or("on all repos".into(), |r| format!("on {r}")));
        }
        InboxCmd::Deny { user, repo, .. } => {
            let r = scope_repo(&repo)?;
            let scope = r.as_ref().map_or(actions::Scope::Global, actions::Scope::Repo);
            let n = actions::deny(paths, &db, &user, scope)?;
            println!("@{user} denied {}; {n} waiting event(s) dropped", r.map_or("on all repos".into(), |r| format!("on {r}")));
        }
    }
    Ok(())
}

pub(crate) fn read_secret() -> Result<String> {
    use std::io::BufRead;
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if tty {
        let _ = std::process::Command::new("stty").arg("-echo").stdin(std::process::Stdio::inherit()).status();
    }
    let mut s = String::new();
    let r = std::io::stdin().lock().read_line(&mut s);
    if tty {
        let _ = std::process::Command::new("stty").arg("echo").stdin(std::process::Stdio::inherit()).status();
    }
    r?;
    Ok(s.trim().to_string())
}

fn confirm(q: &str) -> Result<bool> {
    use std::io::{BufRead, Write};
    print!("{q} (y/N): ");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    Ok(s.trim().eq_ignore_ascii_case("y"))
}

async fn market(paths: &Paths, cmd: MarketCmd) -> Result<()> {
    let cfg = config::Config::load(paths)?;
    match cmd {
        MarketCmd::Add { url } => {
            let m = packs::market_sync(paths, &url).await?;
            config::edit_file(&paths.config_file(), |d| {
                let arr = d["marketplaces"].or_insert(toml_edit::value(toml_edit::Array::new()));
                if let Some(a) = arr.as_array_mut()
                    && !a.iter().any(|v| v.as_str() == Some(&url))
                {
                    a.push(url.as_str());
                }
            })?;
            println!("added marketplace {} ({} packs)", m.name, m.packs.len());
        }
        MarketCmd::Update => {
            for u in std::iter::once(packs::OFFICIAL_MARKET.to_string()).chain(cfg.marketplaces.clone()) {
                match packs::market_sync(paths, &u).await {
                    Ok(m) => println!("{}: {} packs", m.name, m.packs.len()),
                    Err(e) => println!("{u}: {e:#}"),
                }
            }
        }
        MarketCmd::List => {
            for u in std::iter::once(packs::OFFICIAL_MARKET.to_string()).chain(cfg.marketplaces.clone()) {
                match packs::market(paths, &u) {
                    Ok(m) => {
                        println!("{} ({u})", m.name);
                        for p in m.packs {
                            println!("  {:<24} {}", p.name, p.description);
                        }
                    }
                    Err(_) => println!("{u}: not synced (run `nightporter market update`)"),
                }
            }
        }
    }
    Ok(())
}

async fn pack(paths: &Paths, cmd: PackCmd) -> Result<()> {
    let cfg = config::Config::load(paths)?;
    std::fs::create_dir_all(paths.packs_dir())?;
    match cmd {
        PackCmd::List => {
            for (name, l) in packs::lock(paths).packs {
                let d = packs::manifest(&packs::pack_dir(paths, &name)).map(|m| m.description).unwrap_or_default();
                println!("{name:<24} {}  {}  {d}", &l.rev[..l.rev.len().min(10)], l.url);
            }
        }
        PackCmd::Add { name, yes } => {
            let (url, rev) = if name.contains("://") || name.starts_with("git@") {
                let (u, r) = name.rsplit_once('@').filter(|(u, _)| u.contains('/')).map_or((name.as_str(), "HEAD"), |(u, r)| (u, r));
                (u.to_string(), r.to_string())
            } else {
                let e = packs::find(paths, &cfg.marketplaces, &name)
                    .with_context(|| format!("{name} not found in any marketplace (try `nightporter market update`)"))?;
                (e.url, e.rev)
            };
            let (staging, m, sha) = packs::fetch(paths, &url, &rev).await?;
            println!("{}", packs::describe(&m));
            println!(
                "Packs are prompts that steer an agent with access to your code. Review them before installing:\n  {}",
                staging.display()
            );
            if !yes && !confirm(&format!("Install {} at {}?", m.name, &sha[..10]))? {
                let _ = std::fs::remove_dir_all(&staging);
                return Ok(());
            }
            packs::commit(paths, &staging, &m, &url, &sha)?;
            println!("installed. Enable it with `packs = [\"{}\"]` in [defaults] or a repo file.", m.name);
        }
        PackCmd::Update { name, yes } => {
            let l = packs::lock(paths).packs.get(&name).cloned().with_context(|| format!("{name} is not installed"))?;
            let rev = packs::find(paths, &cfg.marketplaces, &name).map_or_else(|| "HEAD".into(), |e| e.rev);
            let (staging, m, sha) = packs::fetch(paths, &l.url, &rev).await?;
            if sha == l.rev {
                let _ = std::fs::remove_dir_all(&staging);
                println!("{name} is up to date");
                return Ok(());
            }
            println!("{}", packs::diff_dirs(&packs::pack_dir(paths, &name), &staging));
            println!("{}", packs::describe(&m));
            if !yes && !confirm(&format!("Update {name} {} → {}?", &l.rev[..l.rev.len().min(10)], &sha[..10]))? {
                let _ = std::fs::remove_dir_all(&staging);
                return Ok(());
            }
            packs::commit(paths, &staging, &m, &l.url, &sha)?;
            println!("updated");
        }
        PackCmd::Remove { name } => {
            packs::remove(paths, &name)?;
            println!("removed {name}");
        }
        PackCmd::Env { name } => print!("{}", packs::env_preset(paths, &name)?),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn jobs_can_only_run_read_only_commands() {
        use clap::Parser;
        let ro = |args: &[&str]| super::read_only(&super::Cli::parse_from(args).cmd.unwrap());
        assert!(ro(&["nightporter", "status"]) && ro(&["nightporter", "show", "3"]) && ro(&["nightporter", "inbox", "list"]));
        assert!(!ro(&["nightporter", "approve", "3"]) && !ro(&["nightporter", "inbox", "allow", "x", "--global"]));
        assert!(!ro(&["nightporter", "skill", "setup"]) && !ro(&["nightporter", "tui"]));
    }

    #[test]
    fn printable_strips_terminal_escapes() {
        assert_eq!(super::printable("ok\x1b[2K\x1b[1Ahidden\r\n\tnext\x07"), "ok[2K[1Ahidden\n\tnext");
    }
}
