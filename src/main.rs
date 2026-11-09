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
mod sweep;
mod trust;
mod tui;
mod update;
mod watch;
mod worktrees;

#[cfg(test)]
mod e2e_tests;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use forge::{ForgeKind, RepoId};
use paths::Paths;
use state::{Db, Disposition, JobStatus};
use std::io::Write;

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
    /// Queue maintainer jobs for a repo's open issues (or PRs/MRs with --prs), oldest first.
    /// Each job stops at maintainer mode's level (at most draft) for you to review.
    Sweep {
        repo: String,
        /// Open PRs/MRs by others instead of issues.
        #[arg(long)]
        prs: bool,
        /// triage (default for issues: restates, checks whether it's already done, suggests
        /// next steps), reproduce, fix (opens a draft PR/MR) or review (default for PRs).
        #[arg(long)]
        verb: Option<String>,
        /// Only items with this label (repeat for several; all must match).
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Only items opened more than this long ago (e.g. 90d).
        #[arg(long)]
        older_than: Option<String>,
        /// Only items opened within this long (e.g. 14d).
        #[arg(long)]
        newer_than: Option<String>,
        /// At most this many items (oldest first).
        #[arg(long)]
        limit: Option<usize>,
        /// List what would be queued; queue nothing.
        #[arg(long)]
        dry_run: bool,
    },
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
    /// Take over a job interactively in herdr, Rex, tmux, Zellij or a new terminal (opens it with a briefed agent).
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
    /// List the worktrees Night Porter made; remove one or bring back a removed one (for 7 days).
    Worktrees {
        #[command(subcommand)]
        cmd: Option<WtCmd>,
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
    /// Notification targets (desktop, webhooks, commands).
    Notify {
        #[command(subcommand)]
        cmd: NotifyCmd,
    },
    /// Teach coding agents (Claude Code) how to work with Night Porter.
    Skill {
        #[command(subcommand)]
        cmd: SkillCmd,
    },
}

#[derive(Subcommand)]
enum NotifyCmd {
    /// Send a sample event to one target (`desktop` for the popup), or to all of them.
    Test { target: Option<String> },
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
enum WtCmd {
    /// Remove a job's worktree (its work is kept for 7 days).
    Rm { job: i64 },
    /// Bring back the last removed worktree of a job's PR or issue.
    Restore { job: i64 },
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
    // Not locked: the daemon's tracing writes to stdout from other threads.
    run(&paths, cmd, &mut std::io::stdout(), std::io::IsTerminal::is_terminal(&std::io::stdout()))
}

/// Run one command; its own output goes to `out` (stdout, or a buffer in tests).
fn run(paths: &Paths, cmd: Cmd, out: &mut dyn Write, tty: bool) -> Result<()> {
    let cmd_name = match &cmd {
        Cmd::Subscribe { .. } => "subscribe",
        _ => "",
    };
    let rt = || tokio::runtime::Builder::new_multi_thread().enable_all().build();
    match cmd {
        Cmd::Tui => {
            paths.ensure()?;
            tui::run(paths)
        }
        Cmd::Events { json } => watch::events(paths, json),
        Cmd::Wait { id, timeout } => {
            let code = watch::wait(paths, id, config::parse_duration(&timeout)?)?;
            std::process::exit(code)
        }
        Cmd::Skill { cmd } => cmd_skill(&cmd, out),
        Cmd::Notify { cmd: NotifyCmd::Test { target } } => notify_test(paths, target.as_deref(), out),
        Cmd::Daemon => {
            init_tracing();
            rt()?.block_on(daemon::run(paths.clone()))
        }
        Cmd::Setup => setup::run(paths),
        Cmd::Doctor => {
            if !doctor::run(paths) {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Status => status(paths, out),
        Cmd::Start | Cmd::Stop => {
            writeln!(out, "{}", if matches!(cmd, Cmd::Start) { service::start(paths)? } else { service::stop(paths)? })?;
            Ok(())
        }
        Cmd::Update => cmd_update(&db(paths)?, out),
        Cmd::Repos { search, all } => cmd_repos(paths, search.as_deref(), all, out, tty),
        Cmd::Repo { repo } => cmd_repo(paths, &repo, out, tty),
        c @ (Cmd::Dismiss { .. } | Cmd::Restore { .. } | Cmd::Purge { .. }) => cmd_archive(paths, c, out),
        Cmd::Subscribe { repo } | Cmd::Unsubscribe { repo } => {
            let on = matches!(cmd_name, "subscribe");
            let r = resolve_repo(paths, &repo)?;
            actions::set_subscribed(paths, &r, on)?;
            writeln!(out, "{} {}/{}", if on { "subscribed to" } else { "unsubscribed from" }, r.host, r.path)?;
            Ok(())
        }
        Cmd::Approve { id, override_blocking } => {
            writeln!(out, "{}", actions::approve(&db(paths)?, id, override_blocking)?)?;
            Ok(())
        }
        Cmd::Reject { id } => {
            actions::reject(&db(paths)?, id)?;
            writeln!(out, "job {id} rejected: nothing will be pushed or posted")?;
            Ok(())
        }
        Cmd::Retry { id } => {
            writeln!(out, "re-queued {} event(s)", actions::retry(&db(paths)?, id)?)?;
            Ok(())
        }
        Cmd::Handoff { id } => {
            let db = db(paths)?;
            let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
            writeln!(out, "{}", handoff::take_over(paths, &db, &j)?)?;
            Ok(())
        }
        Cmd::Show { id } => {
            let db = db(paths)?;
            let j = db.job(id)?.with_context(|| format!("no job {id}"))?;
            write!(out, "{}", printable(&tui::job_text(&db, &j)))?;
            Ok(())
        }
        Cmd::Inbox { cmd } => inbox(paths, cmd, out),
        Cmd::Pause => {
            actions::set_paused(&db(paths)?, true)?;
            writeln!(out, "paused: no new jobs will start (running jobs finish)")?;
            Ok(())
        }
        Cmd::Resume => {
            actions::set_paused(&db(paths)?, false)?;
            writeln!(out, "resumed")?;
            Ok(())
        }
        Cmd::Gc { all } => rt()?.block_on(async {
            let cfg = config::Config::load(paths)?;
            let ctx = daemon::offline_ctx(paths, cfg).await?;
            job::gc(&ctx, all).await
        }),
        Cmd::Worktrees { cmd } => rt()?.block_on(cmd_worktrees(paths, cmd, out)),
        Cmd::Token { cmd: TokenCmd::Set { forge, host } } => cmd_token_set(&forge, host),
        Cmd::Service { cmd } => cmd_service(paths, &cmd, out),
        c @ Cmd::Sweep { .. } => rt()?.block_on(cmd_sweep(paths, c)),
        Cmd::Market { cmd } => rt()?.block_on(market(paths, cmd, out)),
        Cmd::Pack { cmd } => rt()?.block_on(pack(paths, cmd, out)),
    }
}

/// `nightporter worktrees [rm|restore <job>]`.
async fn cmd_worktrees(paths: &Paths, cmd: Option<WtCmd>, out: &mut dyn Write) -> Result<()> {
    let d = db(paths)?;
    let msg = match cmd {
        None => {
            let all = worktrees::list(&d).await?;
            if all.is_empty() { "no worktrees".into() } else { all.iter().map(worktrees::Wt::line).collect::<Vec<_>>().join("\n") }
        }
        Some(WtCmd::Rm { job }) => worktrees::remove(paths, &config::Config::load(paths)?, &d, job).await?,
        Some(WtCmd::Restore { job }) => worktrees::restore(&d, job).await?,
    };
    writeln!(out, "{msg}")?;
    Ok(())
}

/// `nightporter sweep`: queue maintainer jobs for a repo's open issues or PRs.
async fn cmd_sweep(paths: &Paths, cmd: Cmd) -> Result<()> {
    let Cmd::Sweep { repo, prs, verb, labels, older_than, newer_than, limit, dry_run } = cmd else { unreachable!() };

    let r = resolve_repo(paths, &repo)?;
    let target = if prs { forge::Target::Pr } else { forge::Target::Issue };
    let verb = match verb.as_deref() {
        Some(v) => forge::Verb::parse(v).with_context(|| format!("unknown verb {v:?} (triage, reproduce, fix or review)"))?,
        None if prs => forge::Verb::Review,
        None => forge::Verb::Triage,
    };
    let ago = |d: Option<String>| -> Result<Option<jiff::Timestamp>> {
        d.map(|d| {
            let secs = i64::try_from(config::parse_duration(&d)?.as_secs())?;
            Ok(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(secs))
        })
        .transpose()
    };
    let pick = forge::Pick { labels, created_before: ago(older_than)?, created_after: ago(newer_than)?, skip_author: None, limit };

    let ctx = daemon::offline_ctx(paths, config::Config::load(paths)?).await?;
    let rep = sweep::run(&ctx, &r, target, verb, pick, dry_run).await?;
    for i in &rep.picked {
        println!("#{:<6} {}  {}", i.number, i.created_at.get(..10).unwrap_or(&i.created_at), printable(&i.title));
    }
    let what = if prs { "PR(s)/MR(s)" } else { "issue(s)" };
    let verb_s = state::enum_str(&verb);
    if dry_run {
        println!("\n{} open {what} match; nothing queued (drop --dry-run to queue {verb_s} jobs)", rep.picked.len());
    } else {
        let again = rep.picked.len() - rep.queued;
        println!(
            "\nqueued {} {verb_s} job(s){}; they run under your schedule and caps, and wait for your approval",
            rep.queued,
            if again > 0 { format!(" ({again} already queued)") } else { String::new() }
        );
        if !paths.daemon_running() {
            println!("the daemon isn't running: start it with `nightporter start`");
        }
    }
    Ok(())
}

/// Dismiss, restore and purge: clearing out jobs you won't act on.
fn cmd_archive(paths: &Paths, cmd: Cmd, out: &mut dyn Write) -> Result<()> {
    match cmd {
        Cmd::Dismiss { ids } => {
            let db = db(paths)?;
            for id in ids {
                actions::dismiss(&db, id)?;
                writeln!(out, "job {id} dismissed (undo: nightporter restore {id})")?;
            }
        }
        Cmd::Restore { id } => writeln!(out, "job {id} is back: {}", actions::restore(&db(paths)?, id)?.label())?,
        Cmd::Purge { ids, yes } => {
            let mut db = db(paths)?;
            let list = ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
            if !yes && !confirm(&format!("Delete job(s) {list} for good? This can't be undone"))? {
                return Ok(());
            }
            for id in ids {
                actions::purge(&mut db, id)?;
                writeln!(out, "job {id} deleted")?;
            }
        }
        _ => unreachable!("only archive commands are routed here"),
    }
    Ok(())
}

fn cmd_skill(cmd: &SkillCmd, out: &mut dyn Write) -> Result<()> {
    match cmd {
        SkillCmd::Show => write!(out, "{}", skill::GUIDE)?,
        SkillCmd::Setup => writeln!(out, "{}", skill::setup()?)?,
        SkillCmd::Remove => writeln!(out, "{}", skill::remove()?)?,
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

fn cmd_repos(paths: &Paths, search: Option<&str>, all: bool, w: &mut dyn Write, tty: bool) -> Result<()> {
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
    let paint = |code: &str, s: &str| if color_out(tty) { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() };
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
    page(&out, w, tty)
}

/// Colour only when `out` is a terminal, and never with `NO_COLOR`.
fn color_out(tty: bool) -> bool {
    tty && std::env::var_os("NO_COLOR").is_none()
}

/// Print, through `$PAGER` (else `less`) when it's taller than the terminal. q leaves the pager.
fn page(text: &str, out: &mut dyn Write, tty: bool) -> Result<()> {
    let tall = tty && crossterm::terminal::size().is_ok_and(|(_, h)| text.lines().count() >= usize::from(h).saturating_sub(1));
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
    write!(out, "{text}")?;
    Ok(())
}

fn cmd_repo(paths: &Paths, repo: &str, out: &mut dyn Write, tty: bool) -> Result<()> {
    let cfg = config::Config::load(paths)?;
    let r = resolve_repo(paths, repo)?;
    let clone = repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts).remove(&r);
    page(&printable(&settings::repo_text(paths, &cfg, &r, clone.as_deref())?), out, tty)
}

fn cmd_update(db: &Db, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "You have Night Porter {}. Asking GitHub for the latest release…", update::CURRENT)?;
    let latest = tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(update::fetch_latest())?;
    db.set("latest_version", &latest)?;
    db.set("update_checked_at", &state::now().to_string())?;
    if update::newer(&latest, update::CURRENT) {
        writeln!(out, "{latest} is out. Upgrade with:\n  {}", update::how_to_upgrade())?;
    } else {
        writeln!(out, "You're up to date.")?;
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

fn cmd_service(paths: &Paths, cmd: &ServiceCmd, out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "{}",
        match cmd {
            ServiceCmd::Install => service::install(&paths.log_file())?,
            ServiceCmd::Uninstall => service::uninstall()?,
        }
    )?;
    Ok(())
}

fn notify_test(paths: &Paths, target: Option<&str>, out: &mut dyn Write) -> Result<()> {
    let cfg = config::Config::load(paths)?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    writeln!(out, "{}", rt.block_on(notify::test(&cfg.notifications, target))?)?;
    Ok(())
}

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

/// The agent configured for a repo (Claude Code if the config can't be read).
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

fn status(paths: &Paths, out: &mut dyn Write) -> Result<()> {
    let db = db(paths)?;
    let up = paths.daemon_running();
    writeln!(
        out,
        "Night Porter: daemon {}{}",
        if up { "running (stop: nightporter stop)" } else { "NOT RUNNING (start: nightporter start)" },
        if actions::paused(&db) { ", PAUSED" } else { "" }
    )?;
    if let Some(b) = update::banner(&db) {
        writeln!(out, "{b}")?;
    }
    let mut any = false;
    for j in db.recent_jobs(200)?.into_iter().filter(|j| j.status.needs_user() || j.status.is_active() || j.status == JobStatus::Failed) {
        any = true;
        writeln!(
            out,
            "job {:>4}  {:<22} {}#{}  {}",
            j.id,
            j.status.label(),
            j.repo,
            j.number,
            printable(j.summary.lines().next().unwrap_or_default())
        )?;
    }
    let inbox = db.events_with(Disposition::Inbox)?;
    for (e, _) in &inbox {
        any = true;
        writeln!(
            out,
            "inbox {}  @{} on {}#{}: {}",
            e.id,
            e.author,
            e.repo,
            e.number,
            printable(e.body.lines().next().unwrap_or_default())
        )?;
    }
    if !any {
        writeln!(out, "Nothing needs you.")?;
    }
    Ok(())
}

/// Forge text and diffs can carry terminal escape sequences (hiding lines, rewriting the
/// screen). Keep newlines and tabs, drop every other control character.
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect()
}

fn inbox(paths: &Paths, cmd: InboxCmd, out: &mut dyn Write) -> Result<()> {
    let db = db(paths)?;
    let scope_repo = |r: &Option<String>| -> Result<Option<RepoId>> { r.as_deref().map(|r| resolve_repo(paths, r)).transpose() };
    match cmd {
        InboxCmd::Restore { event } => {
            actions::unignore(&db, &event)?;
            writeln!(out, "{event} is back in the inbox")?;
        }
        InboxCmd::List { ignored } => {
            let list = if ignored { db.ignored_events()? } else { db.events_with(Disposition::Inbox)? };
            for (e, _) in list {
                writeln!(
                    out,
                    "{}  @{} ({:?}) on {}#{}\n    {}",
                    e.id,
                    e.author,
                    e.role,
                    e.repo,
                    e.number,
                    printable(&e.body).replace('\n', "\n    ")
                )?;
            }
        }
        InboxCmd::AllowOnce { event } => {
            actions::allow_once(&db, &event)?;
            writeln!(out, "{event} allowed once")?;
        }
        InboxCmd::Ignore { event } => {
            actions::ignore(&db, &event)?;
            writeln!(out, "{event} ignored")?;
        }
        InboxCmd::Allow { user, repo, .. } => {
            let r = scope_repo(&repo)?;
            let scope = r.as_ref().map_or(actions::Scope::Global, actions::Scope::Repo);
            let n = actions::allow_always(paths, &db, &user, scope)?;
            writeln!(out, "@{user} trusted {}; {n} waiting event(s) released", r.map_or("on all repos".into(), |r| format!("on {r}")))?;
        }
        InboxCmd::Deny { user, repo, .. } => {
            let r = scope_repo(&repo)?;
            let scope = r.as_ref().map_or(actions::Scope::Global, actions::Scope::Repo);
            let n = actions::deny(paths, &db, &user, scope)?;
            writeln!(out, "@{user} denied {}; {n} waiting event(s) dropped", r.map_or("on all repos".into(), |r| format!("on {r}")))?;
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

async fn market(paths: &Paths, cmd: MarketCmd, out: &mut dyn Write) -> Result<()> {
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
            writeln!(out, "added marketplace {} ({} packs)", m.name, m.packs.len())?;
        }
        MarketCmd::Update => {
            for u in std::iter::once(packs::OFFICIAL_MARKET.to_string()).chain(cfg.marketplaces.clone()) {
                match packs::market_sync(paths, &u).await {
                    Ok(m) => writeln!(out, "{}: {} packs", m.name, m.packs.len())?,
                    Err(e) => writeln!(out, "{u}: {e:#}")?,
                }
            }
        }
        MarketCmd::List => {
            for u in std::iter::once(packs::OFFICIAL_MARKET.to_string()).chain(cfg.marketplaces.clone()) {
                match packs::market(paths, &u) {
                    Ok(m) => {
                        writeln!(out, "{} ({u})", m.name)?;
                        for p in m.packs {
                            writeln!(out, "  {:<24} {}", p.name, p.description)?;
                        }
                    }
                    Err(_) => writeln!(out, "{u}: not synced (run `nightporter market update`)")?,
                }
            }
        }
    }
    Ok(())
}

async fn pack(paths: &Paths, cmd: PackCmd, out: &mut dyn Write) -> Result<()> {
    let cfg = config::Config::load(paths)?;
    std::fs::create_dir_all(paths.packs_dir())?;
    match cmd {
        PackCmd::List => {
            for (name, l) in packs::lock(paths).packs {
                let d = packs::manifest(&packs::pack_dir(paths, &name)).map(|m| m.description).unwrap_or_default();
                writeln!(out, "{name:<24} {}  {}  {d}", &l.rev[..l.rev.len().min(10)], l.url)?;
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
            if let Some(l) = packs::lock(paths).packs.get(&m.name).filter(|l| l.url != url) {
                let _ = std::fs::remove_dir_all(&staging);
                bail!("a pack named {} is already installed from {}; remove it first", m.name, l.url);
            }
            writeln!(out, "{}", printable(&packs::describe(&m)))?;
            writeln!(
                out,
                "Packs are prompts that steer an agent with access to your code. Review them before installing:\n  {}",
                staging.display()
            )?;
            if !yes && !confirm(&format!("Install {} at {}?", m.name, &sha[..10]))? {
                let _ = std::fs::remove_dir_all(&staging);
                return Ok(());
            }
            packs::commit(paths, &staging, &m, &url, &sha)?;
            writeln!(out, "installed. Enable it with `packs = [\"{}\"]` in [defaults] or a repo file.", m.name)?;
        }
        PackCmd::Update { name, yes } => {
            let l = packs::lock(paths).packs.get(&name).cloned().with_context(|| format!("{name} is not installed"))?;
            let rev = packs::find(paths, &cfg.marketplaces, &name).map_or_else(|| "HEAD".into(), |e| e.rev);
            let (staging, m, sha) = packs::fetch(paths, &l.url, &rev).await?;
            if m.name != name {
                let _ = std::fs::remove_dir_all(&staging);
                bail!("{} now calls itself {:?}, not {name}; not updating (remove and add it again if that's right)", l.url, m.name);
            }
            if sha == l.rev {
                let _ = std::fs::remove_dir_all(&staging);
                writeln!(out, "{name} is up to date")?;
                return Ok(());
            }
            writeln!(out, "{}", printable(&packs::diff_dirs(&packs::pack_dir(paths, &name), &staging)))?;
            writeln!(out, "{}", printable(&packs::describe(&m)))?;
            if !yes && !confirm(&format!("Update {name} {} → {}?", &l.rev[..l.rev.len().min(10)], &sha[..10]))? {
                let _ = std::fs::remove_dir_all(&staging);
                return Ok(());
            }
            packs::commit(paths, &staging, &m, &l.url, &sha)?;
            writeln!(out, "updated")?;
        }
        PackCmd::Remove { name } => {
            packs::remove(paths, &name)?;
            writeln!(out, "removed {name}")?;
        }
        PackCmd::Env { name } => write!(out, "{}", packs::env_preset(paths, &name)?)?,
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
    fn cli_is_consistent_and_sweep_parses() {
        use clap::{CommandFactory, Parser};
        super::Cli::command().debug_assert();
        let c = super::Cli::parse_from([
            "nightporter",
            "sweep",
            "project/x",
            "--label",
            "Bug",
            "--label",
            "Needs work",
            "--older-than",
            "90d",
            "--limit",
            "5",
            "--dry-run",
        ]);
        let Some(super::Cmd::Sweep { repo, prs, verb, labels, older_than, newer_than, limit, dry_run }) = c.cmd else {
            panic!("not sweep")
        };
        assert_eq!(
            (repo.as_str(), prs, verb, older_than.as_deref(), newer_than, limit, dry_run),
            ("project/x", false, None, Some("90d"), None, Some(5), true)
        );
        assert_eq!(labels, ["Bug", "Needs work"]);
        // Queuing jobs isn't allowed from inside a job.
        assert!(!super::read_only(&super::Cli::parse_from(["nightporter", "sweep", "o/r"]).cmd.unwrap()));
    }

    #[test]
    fn printable_strips_terminal_escapes() {
        assert_eq!(super::printable("ok\x1b[2K\x1b[1Ahidden\r\n\tnext\x07"), "ok[2K[1Ahidden\n\tnext");
    }

    use super::{Cli, Db, Disposition, JobStatus, Paths, RepoId};
    use crate::forge::{ForgeKind, Target};
    use clap::Parser;
    use std::path::Path;

    /// A home with a GitHub and a GitLab account, and clones of `o/r` on each plus `o/other`.
    fn home() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        let code = d.path().join("code");
        let clone = |rel: &str, url: &str| {
            let g = code.join(rel).join(".git");
            std::fs::create_dir_all(&g).unwrap();
            std::fs::write(g.join("config"), format!("[remote \"origin\"]\n\turl = {url}\n")).unwrap();
        };
        clone("gh/o/r", "git@github.com:o/r.git");
        clone("gh/o/other", "git@github.com:o/other.git");
        clone("gl/o/r", "git@gitlab.com:o/r.git");
        std::fs::write(
            paths.config_file(),
            format!(
                "code_dir = {:?}\n[[accounts]]\nforge = \"github\"\n[[accounts]]\nforge = \"gitlab\"\n\
                 [[notifications.targets]]\nname = \"ok\"\ntype = \"command\"\ncommand = [\"true\"]\n",
                code.display().to_string()
            ),
        )
        .unwrap();
        (d, paths)
    }

    fn try_run(paths: &Paths, args: &[&str]) -> anyhow::Result<String> {
        let cli = Cli::parse_from(std::iter::once("nightporter").chain(args.iter().copied()));
        let mut out = Vec::new();
        super::run(paths, cli.cmd.unwrap(), &mut out, false)?;
        Ok(String::from_utf8(out).unwrap())
    }

    fn run(paths: &Paths, args: &[&str]) -> String {
        try_run(paths, args).unwrap_or_else(|e| panic!("{args:?}: {e:#}"))
    }

    fn err(paths: &Paths, args: &[&str]) -> String {
        format!("{:#}", try_run(paths, args).expect_err(&format!("{args:?} should fail")))
    }

    fn gh() -> RepoId {
        RepoId::new(ForgeKind::GitHub, "github.com", "o/r")
    }

    fn job(paths: &Paths, status: JobStatus, summary: &str) -> i64 {
        let mut db = Db::open(&paths.db()).unwrap();
        let id = db.create_job(&gh(), Target::Pr, 4, "my_prs", "draft", &[]).unwrap();
        let mut j = db.job(id).unwrap().unwrap();
        j.status = status;
        j.summary = summary.into();
        db.update_job(&j).unwrap();
        id
    }

    fn status_of(paths: &Paths, id: i64) -> JobStatus {
        Db::open(&paths.db()).unwrap().job(id).unwrap().unwrap().status
    }

    #[test]
    fn status_lists_what_needs_you() {
        let (_d, paths) = home();
        let s = run(&paths, &["status"]);
        assert!(s.starts_with("Night Porter: daemon NOT RUNNING") && s.contains("Nothing needs you."), "{s}");

        let waiting = job(&paths, JobStatus::AwaitingApproval, "renamed foo\x1b[2K\nsecond line");
        job(&paths, JobStatus::Done, "old news");
        let db = Db::open(&paths.db()).unwrap();
        db.add_event(&crate::state::tests::ev("e1", 5), Disposition::Inbox, "").unwrap();
        db.set("latest_version", "999.0.0").unwrap();
        assert_eq!(run(&paths, &["pause"]), "paused: no new jobs will start (running jobs finish)\n");
        let s = run(&paths, &["status"]);
        assert!(s.lines().next().unwrap().ends_with(", PAUSED"), "{s}");
        assert!(s.contains("Night Porter 999.0.0 is out"), "{s}");
        assert!(s.contains(&format!("job {waiting:>4}  NEEDS APPROVAL")) && s.contains("o/r#4  renamed foo[2K\n"), "{s}");
        assert!(!s.contains("old news") && !s.contains("second line") && !s.contains("Nothing needs you"), "{s}");
        assert!(s.contains("inbox e1  @alice on o/r#5: fix"), "{s}");
        assert_eq!(run(&paths, &["resume"]), "resumed\n");
        assert!(!run(&paths, &["status"]).contains("PAUSED"));
    }

    #[test]
    fn job_commands_act_on_the_db() {
        let (_d, paths) = home();
        let id = job(&paths, JobStatus::AwaitingApproval, "renamed foo");
        let show = run(&paths, &["show", &id.to_string()]);
        assert!(show.contains("renamed foo"), "{show}");
        assert_eq!(run(&paths, &["approve", &id.to_string()]), "approved: the daemon will push and post\n");
        assert_eq!(status_of(&paths, id), JobStatus::Approved);
        assert!(err(&paths, &["approve", &id.to_string()]).contains("nothing to approve"));
        assert!(err(&paths, &["handoff", &id.to_string()]).contains("wait for it to finish"));
        assert!(err(&paths, &["show", "99"]).contains("no job 99"));
        assert!(err(&paths, &["handoff", "99"]).contains("no job 99"));

        let id = job(&paths, JobStatus::AwaitingApproval, "x");
        assert_eq!(run(&paths, &["reject", &id.to_string()]), format!("job {id} rejected: nothing will be pushed or posted\n"));
        assert_eq!(status_of(&paths, id), JobStatus::Rejected);
        assert_eq!(run(&paths, &["retry", &id.to_string()]), "re-queued 0 event(s)\n");
        assert!(err(&paths, &["reject", &id.to_string()]).contains("rejected"));

        let (a, b) = (job(&paths, JobStatus::Ready, "a"), job(&paths, JobStatus::Ready, "b"));
        let s = run(&paths, &["dismiss", &a.to_string(), &b.to_string()]);
        assert_eq!(s, format!("job {a} dismissed (undo: nightporter restore {a})\njob {b} dismissed (undo: nightporter restore {b})\n"));
        assert_eq!(run(&paths, &["restore", &a.to_string()]), format!("job {a} is back: ready to copy\n"));
        assert_eq!(status_of(&paths, a), JobStatus::Ready);
        assert_eq!(run(&paths, &["purge", &b.to_string(), "--yes"]), format!("job {b} deleted\n"));
        assert!(Db::open(&paths.db()).unwrap().job(b).unwrap().is_none());
        assert!(err(&paths, &["purge", &a.to_string(), "--yes"]).contains(&a.to_string()), "a ready job can't be purged");
    }

    #[test]
    fn inbox_commands() {
        let (_d, paths) = home();
        let db = Db::open(&paths.db()).unwrap();
        for id in ["e1", "e2", "e3"] {
            let mut e = crate::state::tests::ev(id, 5);
            e.body = format!("hello from {id}\n\x1b[2Ksecond");
            db.add_event(&e, Disposition::Inbox, "").unwrap();
        }
        let list = run(&paths, &["inbox", "list"]);
        assert!(list.contains("e1  @alice (Member) on o/r#5\n    hello from e1\n    [2Ksecond"), "{list}");
        assert_eq!(run(&paths, &["inbox", "ignore", "e1"]), "e1 ignored\n");
        let ignored = run(&paths, &["inbox", "list", "--ignored"]);
        assert!(ignored.contains("e1  @alice") && !ignored.contains("e2"), "{ignored}");
        assert_eq!(run(&paths, &["inbox", "restore", "e1"]), "e1 is back in the inbox\n");
        assert_eq!(run(&paths, &["inbox", "allow-once", "e1"]), "e1 allowed once\n");
        assert!(!run(&paths, &["inbox", "list"]).contains("e1 "));
        assert!(err(&paths, &["inbox", "allow", "bob", "--repo", "o/r"]).contains("ambiguous"));
        assert!(err(&paths, &["inbox", "deny", "bob", "--repo", "nope/x"]).contains("nope/x is not cloned under"));
        assert_eq!(
            run(&paths, &["inbox", "allow", "alice", "--repo", "github.com/o/r"]),
            "@alice trusted on o/r; 2 waiting event(s) released\n"
        );
        assert_eq!(run(&paths, &["inbox", "deny", "mallory", "--global"]), "@mallory denied on all repos; 0 waiting event(s) dropped\n");
        let cfg = crate::config::Config::load(&paths).unwrap();
        let s = cfg.settings_for(&paths, &gh()).unwrap();
        assert_eq!((s.trust.allow_users, s.trust.deny_users), (vec!["alice".to_string()], vec!["mallory".to_string()]));
    }

    #[test]
    fn repos_and_subscriptions() {
        let (_d, paths) = home();
        let s = run(&paths, &["repos"]);
        assert!(s.starts_with("Watching 0 repo(s): subscribe with") && s.contains("3 other clone(s) not watched"), "{s}");
        assert!(!s.contains("[off]"), "unwatched clones are only counted: {s}");
        assert_eq!(run(&paths, &["subscribe", "o/other"]), "subscribed to github.com/o/other\n");
        let s = run(&paths, &["repos"]);
        assert!(s.starts_with("Watching 1 repo(s)\n[on]  github.com/o/other") && s.contains("2 other clone(s)"), "{s}");
        let all = run(&paths, &["repos", "--all"]);
        assert_eq!(all.matches("[off]").count(), 2, "{all}");
        assert!(all.find("[on]").unwrap() < all.find("[off]").unwrap(), "watched first: {all}");
        let found = run(&paths, &["repos", "GITLAB"]);
        assert!(found.contains("[off] gitlab.com/o/r") && !found.contains("github.com/o/r"), "search is case-blind: {found}");
        let one = run(&paths, &["repo", "o/other"]);
        assert!(one.contains("o/other"), "{one}");
        assert!(err(&paths, &["subscribe", "o/r"]).contains("o/r is ambiguous: "));
        assert_eq!(run(&paths, &["unsubscribe", "github.com/o/other"]), "unsubscribed from github.com/o/other\n");
        assert!(run(&paths, &["repos"]).starts_with("Watching 0"));
    }

    fn mk_pack(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("pack.toml"),
            "name = \"node\"\ndescription = \"node helpers\"\n[[envs]]\nname = \"ci\"\nfile = \"ci.toml\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("ci.toml"), "setup = [\"npm ci\"]\n").unwrap();
    }

    #[test]
    fn markets_and_packs_offline() {
        let (d, paths) = home();
        let s = run(&paths, &["market", "list"]);
        assert_eq!(s, format!("{}: not synced (run `nightporter market update`)\n", crate::packs::OFFICIAL_MARKET));
        let m = crate::packs::market_dir(&paths, crate::packs::OFFICIAL_MARKET);
        std::fs::create_dir_all(&m).unwrap();
        std::fs::write(
            m.join("marketplace.toml"),
            "name = \"official\"\n[[packs]]\nname = \"node\"\nurl = \"https://example.com/node\"\nrev = \"abc\"\ndescription = \"node helpers\"\n",
        )
        .unwrap();
        let s = run(&paths, &["market", "list"]);
        assert!(s.starts_with("official (https://") && s.contains("  node                     node helpers"), "{s}");

        assert_eq!(run(&paths, &["pack", "list"]), "");
        let staging = d.path().join("stage");
        mk_pack(&staging);
        let man = crate::packs::manifest(&staging).unwrap();
        crate::packs::commit(&paths, &staging, &man, "https://example.com/node", "0123456789abcdef").unwrap();
        let s = run(&paths, &["pack", "list"]);
        assert!(s.starts_with("node") && s.contains("0123456789  https://example.com/node  node helpers"), "{s}");
        assert_eq!(run(&paths, &["pack", "env", "node/ci"]), "setup = [\"npm ci\"]\n");
        assert!(err(&paths, &["pack", "env", "node/nope"]).contains("no env"));
        assert!(err(&paths, &["pack", "update", "ghost"]).contains("ghost is not installed"));
        assert_eq!(run(&paths, &["pack", "remove", "node"]), "removed node\n");
        assert!(err(&paths, &["pack", "remove", "node"]).contains("not installed"));
    }

    #[test]
    fn handoff_prints_the_command_and_gc_cleans_worktrees() {
        let (d, paths) = home();
        // No accounts: gc runs offline. Print handoffs never open anything.
        std::fs::write(paths.config_file(), "handoff = \"print\"\n").unwrap();
        let wt = d.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let id = job(&paths, JobStatus::Done, "x");
        let db = Db::open(&paths.db()).unwrap();
        let mut j = db.job(id).unwrap().unwrap();
        j.worktree = wt.to_string_lossy().into();
        j.meta.clone = d.path().to_string_lossy().into();
        db.update_job(&j).unwrap();
        let s = run(&paths, &["handoff", &id.to_string()]);
        assert!(s.starts_with("run this in a terminal to take over: cd ") && s.contains("'claude'"), "{s}");
        assert_eq!(run(&paths, &["gc"]), "");
        assert!(wt.exists(), "a fresh worktree stays");
        run(&paths, &["gc", "--all"]);
        assert!(!wt.exists());
        assert!(err(&paths, &["pack", "add", "ghost", "--yes"]).contains("ghost not found in any marketplace"));
        assert!(err(&paths, &["pack", "add", "file:///tmp/x", "--yes"]).contains("must be https://"), "no local or odd URLs");
    }

    #[test]
    fn skill_guide_and_notify_test() {
        let (_d, paths) = home();
        assert_eq!(run(&paths, &["skill", "show"]), crate::skill::GUIDE);
        assert_eq!(run(&paths, &["notify", "test", "ok"]), "ok: sent\n");
        assert!(err(&paths, &["notify", "test", "nope"]).contains("there are: desktop, ok"));
    }
}
