//! Night Porter: handles your PRs while you're away.

mod actions;
mod agent;
mod checks;
mod config;
mod daemon;
mod doctor;
mod forge;
mod git;
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
mod setup;
mod state;
mod trust;
mod tui;

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
    /// List known repos and whether they are subscribed.
    Repos,
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
    /// Re-run a finished or failed job's requests as a new job.
    Retry { id: i64 },
    /// Take over a job interactively in herdr (opens a workspace with a briefed agent).
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
}

#[derive(Subcommand)]
enum InboxCmd {
    /// List events from unknown authors that are waiting for a decision.
    List,
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
    /// Store a token for `github` or `gitlab` (optionally a self-hosted host) in the keychain.
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
        Cmd::Daemon => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_env("NIGHTPORTER_LOG").unwrap_or_else(|_| "nightporter=info".into()),
                )
                .init();
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
        Cmd::Repos => {
            let cfg = config::Config::load(&paths)?;
            for (r, p) in repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts) {
                let s = cfg.settings_for(&paths, &r)?;
                println!(
                    "{} {:<45} {:<8} {}",
                    if s.subscribed { "[on] " } else { "[off]" },
                    format!("{}/{}", r.host, r.path),
                    s.level.as_str(),
                    p.display()
                );
            }
            Ok(())
        }
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
            println!("{}", herdr::handoff(&db, &j, agent_for(&paths, &j.repo))?);
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
        Cmd::Token { cmd: TokenCmd::Set { forge, host } } => {
            let f = ForgeKind::parse(&forge).context("forge must be github or gitlab")?;
            let host = host.unwrap_or_else(|| if f == ForgeKind::GitHub { "github.com".into() } else { "gitlab.com".into() });
            eprint!("Paste the token for {host} and press Enter (input is hidden): ");
            let t = read_secret()?;
            if t.is_empty() {
                bail!("empty token");
            }
            http::store_token(f, &host, &t)?;
            eprintln!("\nstored in the OS keychain; set `token = \"keyring\"` on the account in config.toml");
            Ok(())
        }
        Cmd::Service { cmd } => {
            println!(
                "{}",
                match cmd {
                    ServiceCmd::Install => service::install(&paths.log_file())?,
                    ServiceCmd::Uninstall => service::uninstall()?,
                }
            );
            Ok(())
        }
        Cmd::Market { cmd } => rt()?.block_on(market(&paths, cmd)),
        Cmd::Pack { cmd } => rt()?.block_on(pack(&paths, cmd)),
    }
}

/// The agent configured for a repo (Claude Code if the config can't be read).
pub fn agent_for(paths: &Paths, repo: &RepoId) -> config::AgentKind {
    config::Config::load(paths).and_then(|c| c.settings_for(paths, repo)).map(|s| s.agent).unwrap_or(config::AgentKind::Claude)
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
    println!(
        "Night Porter: daemon {}{}",
        if paths.daemon_running() { "running" } else { "NOT RUNNING" },
        if actions::paused(&db) { ", PAUSED" } else { "" }
    );
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
        InboxCmd::List => {
            for (e, _) in db.events_with(Disposition::Inbox)? {
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

fn read_secret() -> Result<String> {
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
            let rev = packs::find(paths, &cfg.marketplaces, &name).map(|e| e.rev).unwrap_or_else(|| "HEAD".into());
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
    fn printable_strips_terminal_escapes() {
        assert_eq!(super::printable("ok\x1b[2K\x1b[1Ahidden\r\n\tnext\x07"), "ok[2K[1Ahidden\n\tnext");
    }
}
