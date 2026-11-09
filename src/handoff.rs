//! Taking over a job interactively: open it in herdr (<https://herdr.dev>), Rex
//! (<https://www.superlogical.com/rex>), tmux, Zellij or a new terminal window at the job's
//! worktree, with an agent that's already briefed (or print the command to do it by hand).

use crate::config::{Handoff, HandoffAgent};
use crate::paths::Paths;
use crate::state::{Db, Job, JobStatus};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A job can be taken over once it's finished or waiting, has a worktree, and (for someone else's
/// code) you approved running it: an interactive agent there loads the branch's own agent config
/// with none of a job's isolation.
pub fn can_take_over(j: &Job) -> Result<()> {
    if matches!(j.status, JobStatus::Running | JobStatus::Queued | JobStatus::Approved) {
        bail!("job {} is {}; wait for it to finish before taking over", j.id, j.status.label());
    }
    if j.worktree.is_empty() || !std::path::Path::new(&j.worktree).is_dir() {
        bail!("job {} has no worktree (it may have been cleaned up)", j.id);
    }
    if j.meta.untrusted_code && !(j.meta.run_approved && j.meta.run_approved_head == j.head_before) {
        bail!(
            "job {} is on someone else's code you haven't approved running: approve the run first (a), or review {} by hand",
            j.id,
            j.worktree
        );
    }
    Ok(())
}

pub fn take_over(paths: &Paths, db: &Db, j: &Job) -> Result<String> {
    can_take_over(j)?;
    let cfg = crate::config::Config::load(paths).ok();
    let choice = cfg.as_ref().map(|c| c.handoff).unwrap_or_default();
    let agent = cfg.map(|c| c.handoff_agent).unwrap_or_default().or_job(crate::agent_for(paths, &j.repo));
    let target = pick(choice, |k| std::env::var_os(k).is_some(), available);
    let res = match target {
        Handoff::Herdr => crate::herdr::handoff(db, j, agent),
        Handoff::Rex => rex(db, j, agent),
        t => launch(paths, db, j, agent, t),
    };
    match res {
        // `auto` never leaves you with nothing: say what failed and how to do it by hand.
        Err(e) if choice == Handoff::Auto && target != Handoff::Print => {
            Ok(format!("{} ({e:#})", launch(paths, db, j, agent, Handoff::Print)?))
        }
        r => r,
    }
}

/// `auto`: the multiplexer you're in, else herdr or Rex if installed, else a new terminal
/// window, else print the command. `env` says whether a variable is set; `has` whether a
/// target is installed / usable here.
fn pick(choice: Handoff, env: impl Fn(&str) -> bool, has: impl Fn(Handoff) -> bool) -> Handoff {
    if choice != Handoff::Auto {
        return choice;
    }
    let inside = [("HERDR_BIN_PATH", Handoff::Herdr), ("REX_SESSION", Handoff::Rex), ("TMUX", Handoff::Tmux), ("ZELLIJ", Handoff::Zellij)];
    if let Some((_, t)) = inside.iter().find(|(k, _)| env(k)) {
        return *t;
    }
    [Handoff::Herdr, Handoff::Rex, Handoff::Terminal].into_iter().find(|t| has(*t)).unwrap_or(Handoff::Print)
}

fn available(t: Handoff) -> bool {
    match t {
        Handoff::Herdr => on_path("herdr").is_some(),
        Handoff::Rex => rex_bin().is_some(),
        // Over SSH a new window would open on the far end's screen (or nowhere).
        Handoff::Terminal => std::env::var_os("SSH_CONNECTION").is_none() && terminal().is_some(),
        _ => false,
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// The `rex` CLI: `REX_BIN`, then `PATH`, then inside the macOS app bundle (it isn't on `PATH`
/// by default there).
fn rex_bin() -> Option<PathBuf> {
    if let Some(b) = std::env::var_os("REX_BIN") {
        return Some(b.into());
    }
    if let Some(p) = on_path("rex") {
        return Some(p);
    }
    let home = std::env::home_dir().unwrap_or_default();
    ["/Applications".into(), home.join("Applications")]
        .iter()
        .flat_map(|dir| ["Rex.app", "Rex Beta.app"].map(|app| dir.join(app).join("Contents/Helpers/Rex Server.rex/Contents/MacOS/rex")))
        .find(|p| p.is_file())
}

/// Unique (so you can take over the same job twice) and safe as a Rex label or tmux/Zellij name.
fn label(j: &Job) -> String {
    let slug: String = j.repo.path.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    format!("np-{slug}-{}-{}", j.number, crate::state::now() % 100_000)
}

/// POSIX single-quoting: safe for any string (paths and briefings come from strangers).
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `cd <worktree> && <agent> "$(cat <brief file>)"`: the briefing is read from a file at run
/// time, never pasted into the command.
fn shell_line(worktree: &str, agent: HandoffAgent, brief_file: &Path) -> String {
    const BRIEF: &str = "\0brief";
    let cmd: Vec<String> = agent
        .argv(BRIEF)
        .iter()
        .map(|a| if a == BRIEF { format!("\"$(cat {})\"", sh_quote(&brief_file.to_string_lossy())) } else { sh_quote(a) })
        .collect();
    format!("cd {} && {}", sh_quote(worktree), cmd.join(" "))
}

/// tmux, Zellij, a new terminal window, or printing: all run (or show) a small script in the
/// job's directory that starts the briefed agent at the worktree.
fn launch(paths: &Paths, db: &Db, j: &Job, agent: HandoffAgent, target: Handoff) -> Result<String> {
    let dir = paths.job_dir(j.id);
    std::fs::create_dir_all(&dir)?;
    let brief = dir.join("handoff-brief.md");
    std::fs::write(&brief, crate::herdr::briefing(db, j))?;
    let line = shell_line(&j.worktree, agent, &brief);
    if target == Handoff::Print {
        return Ok(format!("run this in a terminal to take over: {line}"));
    }
    // `.command` so macOS terminals run it when opened.
    let script = dir.join("handoff.command");
    std::fs::write(&script, format!("#!/bin/sh\n{line}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    }
    let script = script.to_string_lossy().into_owned();
    let label = label(j);
    let wt = j.worktree.as_str();
    let who = agent.label();
    match target {
        Handoff::Tmux if std::env::var_os("TMUX").is_some() => {
            run("tmux", &["new-window", "-n", &label, "-c", wt, "--", "/bin/sh", &script])?;
            Ok(format!("opened tmux window \"{label}\" with {who}"))
        }
        Handoff::Tmux => {
            run("tmux", &["new-session", "-d", "-s", &label, "-c", wt, "--", "/bin/sh", &script])?;
            Ok(format!("started tmux session with {who}; attach with: tmux attach -t {label}"))
        }
        Handoff::Zellij if std::env::var_os("ZELLIJ").is_some() => {
            run("zellij", &["action", "new-pane", "--name", &label, "--cwd", wt, "--", "/bin/sh", &script])?;
            Ok(format!("opened Zellij pane \"{label}\" with {who}"))
        }
        Handoff::Zellij => {
            run("zellij", &["attach", "--create-background", &label])?;
            run("zellij", &["--session", &label, "action", "new-pane", "--cwd", wt, "--", "/bin/sh", &script])?;
            Ok(format!("started Zellij session with {who}; attach with: zellij attach {label}"))
        }
        _ => {
            let (bin, args) = terminal().context("no terminal found; set $TERMINAL or use handoff = \"print\"")?;
            let mut cmd = Command::new(&bin);
            // iTerm types the command into a new window's login shell (so your PATH applies);
            // the path goes in as an AppleScript argument, never spliced into the script.
            let arg = if bin == Path::new("osascript") { sh_quote(&script) } else { script };
            cmd.args(&args).arg(arg).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            if cfg!(target_os = "macos") {
                run_cmd(&mut cmd)?;
            } else {
                cmd.spawn().with_context(|| format!("run {}", bin.display()))?; // stays open; don't wait
            }
            Ok(format!("opened a new terminal window with {who}"))
        }
    }
}

fn run(bin: &str, args: &[&str]) -> Result<()> {
    run_cmd(Command::new(bin).args(args))
}

fn run_cmd(cmd: &mut Command) -> Result<()> {
    let bin = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd.output().with_context(|| format!("run {bin}"))?;
    if !out.status.success() {
        bail!("{bin}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// A program (plus leading args) that opens a new window running the script given after them.
/// macOS: iTerm2 if installed, else Terminal (which runs `.command` files in a login shell). Elsewhere: `$TERMINAL`, then common emulators.
fn terminal() -> Option<(PathBuf, Vec<&'static str>)> {
    if cfg!(target_os = "macos") {
        let home = std::env::home_dir().unwrap_or_default();
        let iterm = ["/Applications".into(), home.join("Applications")].iter().any(|d: &PathBuf| d.join("iTerm.app").is_dir());
        return Some(if iterm {
            let script = [
                "on run argv",
                "tell application \"iTerm\"",
                "tell current session of (create window with default profile) to write text (item 1 of argv)",
                "end tell",
                "end run",
            ];
            ("osascript".into(), script.iter().flat_map(|l| ["-e", l]).collect())
        } else {
            ("open".into(), vec!["-a", "Terminal"])
        });
    }
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return None;
    }
    if let Some(t) = std::env::var_os("TERMINAL").filter(|t| !t.is_empty()) {
        return Some((t.into(), vec!["-e"]));
    }
    [
        ("x-terminal-emulator", vec!["-e"]),
        ("ghostty", vec!["-e"]),
        ("kitty", vec![]),
        ("wezterm", vec!["start", "--"]),
        ("gnome-terminal", vec!["--"]),
        ("konsole", vec!["-e"]),
    ]
    .into_iter()
    .find_map(|(name, args)| on_path(name).map(|p| (p, args)))
}

/// A new Rex session at the worktree, running the agent with the briefing as its first prompt.
fn rex(db: &Db, j: &Job, agent: HandoffAgent) -> Result<String> {
    let bin = rex_bin().context("Rex not found: install it from https://www.superlogical.com/rex or set REX_BIN")?;
    let label = label(j);
    // Through your login shell, so the agent is on PATH as it is in your terminal.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let brief = crate::herdr::briefing(db, j);
    let mut cmd = Command::new(&bin);
    cmd.args(["new", &label, "-c", &j.worktree, "--keep-open", "--shell", &shell, "--json", "--"]).args(agent.argv(&brief));
    let out = cmd.output().context("run rex")?;
    if !out.status.success() {
        bail!("rex new: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(format!("opened Rex session \"{label}\" with {}; switch to it in Rex", agent.label()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{ForgeKind, RepoId, Target};

    #[test]
    fn someone_elses_code_needs_run_approval_before_a_take_over() {
        let d = tempfile::tempdir().unwrap();
        let mut db = Db::memory().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&repo, Target::Pr, 7, "maintainer", "draft", &[]).unwrap();
        let mut j = db.job(id).unwrap().unwrap();
        j.status = JobStatus::AwaitingRunApproval;
        j.worktree = d.path().to_string_lossy().into();
        j.head_before = "abc".into();
        j.meta.untrusted_code = true;
        assert!(can_take_over(&j).unwrap_err().to_string().contains("approve the run first"));
        // Approved, but for an older head: still no.
        (j.meta.run_approved, j.meta.run_approved_head) = (true, "old".into());
        assert!(can_take_over(&j).is_err());
        j.meta.run_approved_head = "abc".into();
        j.status = JobStatus::AwaitingApproval;
        can_take_over(&j).unwrap();
        j.status = JobStatus::Running;
        assert!(can_take_over(&j).is_err());
    }

    #[test]
    fn rex_handoff_opens_a_session_at_the_worktree() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("calls.log");
        let stub = d.path().join("rex");
        std::fs::write(&stub, format!("#!/bin/sh\nfor a in \"$@\"; do echo \"$a\" >> {}; done\necho '{{}}'\n", log.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // SAFETY: test-only; no other test reads REX_BIN.
        unsafe { std::env::set_var("REX_BIN", &stub) };

        let mut db = Db::memory().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "Acme/API");
        let id = db.create_job(&repo, Target::Pr, 7, "my_prs", "draft", &[]).unwrap();
        let mut j = db.job(id).unwrap().unwrap();
        j.status = JobStatus::AwaitingApproval;
        j.worktree = d.path().to_string_lossy().into();
        j.summary = "renamed foo".into();
        let msg = rex(&db, &j, HandoffAgent::Claude).unwrap();
        assert!(msg.contains("np-acme-api-7-"), "{msg}");
        let calls = std::fs::read_to_string(&log).unwrap();
        let args: Vec<&str> = calls.lines().collect();
        assert_eq!(&args[..1], ["new"]);
        assert!(args.windows(2).any(|w| w == ["-c", d.path().to_str().unwrap()]), "{calls}");
        let dash = args.iter().position(|a| *a == "--").unwrap();
        assert_eq!(args[dash + 1], "claude");
        assert!(calls.contains("renamed foo"), "the agent gets the briefing: {calls}");
        unsafe { std::env::remove_var("REX_BIN") };
    }

    #[test]
    fn auto_prefers_the_multiplexer_you_are_in_then_installed_then_terminal_then_print() {
        let none = |_: Handoff| false;
        assert_eq!(pick(Handoff::Auto, |k| k == "TMUX", |_| true), Handoff::Tmux);
        assert_eq!(pick(Handoff::Auto, |k| k == "ZELLIJ", none), Handoff::Zellij);
        assert_eq!(pick(Handoff::Auto, |k| k == "REX_SESSION" || k == "TMUX", none), Handoff::Rex, "Rex inside tmux");
        assert_eq!(pick(Handoff::Auto, |k| k == "HERDR_BIN_PATH" || k == "TMUX", none), Handoff::Herdr);
        assert_eq!(pick(Handoff::Auto, |_| false, |_| true), Handoff::Herdr);
        assert_eq!(pick(Handoff::Auto, |_| false, |t| t != Handoff::Herdr), Handoff::Rex);
        assert_eq!(pick(Handoff::Auto, |_| false, |t| t == Handoff::Terminal), Handoff::Terminal);
        assert_eq!(pick(Handoff::Auto, |_| false, none), Handoff::Print);
        assert_eq!(pick(Handoff::Zellij, |k| k == "TMUX", |_| true), Handoff::Zellij, "an explicit choice wins");
    }

    #[cfg(unix)]
    #[test]
    fn every_handoff_agent_starts_interactive_with_the_brief() {
        use crate::config::AgentKind;
        let argv = |a: HandoffAgent| a.argv("B").join(" ");
        assert_eq!(argv(HandoffAgent::Pi), "pi B");
        assert_eq!(argv(HandoffAgent::Cursor), "cursor-agent B");
        assert_eq!(argv(HandoffAgent::Copilot), "copilot -i B");
        assert_eq!(argv(HandoffAgent::Goose), "goose run -s -t B");
        assert_eq!(argv(HandoffAgent::Qwen), "qwen -i B");
        assert_eq!(argv(HandoffAgent::Kiro), "kiro-cli chat B");
        assert_eq!(argv(HandoffAgent::Gemini), "gemini -i B");
        assert_eq!(argv(HandoffAgent::OpenCode), "opencode --prompt B");
        // `job` follows the job's agent; anything else overrides it.
        assert_eq!(HandoffAgent::Job.or_job(AgentKind::Codex), HandoffAgent::Codex);
        assert_eq!(HandoffAgent::Pi.or_job(AgentKind::Codex), HandoffAgent::Pi);
        // Every config name parses, and the brief is always the last argument.
        for n in HandoffAgent::NAMES {
            let a: HandoffAgent = toml::Value::String((*n).into()).try_into().unwrap();
            assert_eq!(a.or_job(AgentKind::Claude).argv("B").last().map(String::as_str), Some("B"), "{n}");
        }
    }

    #[test]
    fn shell_line_passes_hostile_paths_and_briefings_through_intact() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("it's $(touch pwned) `x` \"q\"\nline");
        std::fs::create_dir(&wt).unwrap();
        let brief = d.path().join("b'r $HOME.md");
        let text = "Fix \"it\"; $(touch pwned) `rm -rf ~` 'quoted' \\ end\nsecond line";
        std::fs::write(&brief, text).unwrap();
        let bin = d.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let out = d.path().join("out");
        // A stub agent that records its working directory and each argument.
        let stub = bin.join("opencode");
        std::fs::write(&stub, format!("#!/bin/sh\nprintf '%s\\0' \"$(pwd -P)\" \"$@\" > '{}'\n", out.display())).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let line = shell_line(wt.to_str().unwrap(), HandoffAgent::OpenCode, &brief);
        assert!(!line.contains("rm -rf"), "the briefing is never in the command: {line}");
        let st = Command::new("/bin/sh").arg("-c").arg(&line).env("PATH", format!("{}:/usr/bin:/bin", bin.display())).status().unwrap();
        assert!(st.success(), "{line}");
        let got = std::fs::read_to_string(&out).unwrap();
        let wt = std::fs::canonicalize(&wt).unwrap();
        assert_eq!(got.split('\0').collect::<Vec<_>>(), [wt.to_str().unwrap(), "--prompt", text, ""]);
        assert!(!d.path().join("pwned").exists() && !wt.join("pwned").exists());
    }

    #[test]
    fn print_writes_the_briefing_to_the_job_dir() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("cfg"), &d.path().join("data"));
        let mut db = Db::memory().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&repo, Target::Pr, 3, "my_prs", "draft", &[]).unwrap();
        let mut j = db.job(id).unwrap().unwrap();
        j.worktree = d.path().to_string_lossy().into();
        j.summary = "renamed foo".into();
        let msg = launch(&paths, &db, &j, HandoffAgent::Claude, Handoff::Print).unwrap();
        let brief = paths.job_dir(id).join("handoff-brief.md");
        assert!(std::fs::read_to_string(&brief).unwrap().contains("renamed foo"));
        assert!(msg.contains(&format!("&& 'claude' \"$(cat '{}')\"", brief.display())), "{msg}");
    }

    #[test]
    fn take_over_needs_a_finished_job_with_a_worktree() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        std::fs::write(paths.config_file(), "handoff = \"print\"\n[defaults]\nagent = \"codex\"\n").unwrap();
        let mut db = Db::memory().unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&repo, Target::Pr, 7, "my_prs", "draft", &[]).unwrap();
        let mut j = db.job(id).unwrap().unwrap();
        for s in [JobStatus::Running, JobStatus::Queued, JobStatus::Approved] {
            j.status = s;
            assert!(take_over(&paths, &db, &j).unwrap_err().to_string().contains("wait for it to finish"));
        }
        j.status = JobStatus::Ready;
        assert!(take_over(&paths, &db, &j).unwrap_err().to_string().contains("has no worktree"));
        j.worktree = d.path().join("gone").to_string_lossy().into();
        assert!(take_over(&paths, &db, &j).is_err(), "a cleaned-up worktree is refused too");
        j.worktree = d.path().to_string_lossy().into();
        let msg = take_over(&paths, &db, &j).unwrap();
        assert!(msg.starts_with("run this in a terminal to take over: cd ") && msg.contains("&& 'codex'"), "{msg}");
    }

    #[test]
    fn run_reports_stderr_on_failure() {
        run("true", &[]).unwrap();
        let e = run("sh", &["-c", "echo broken >&2; exit 3"]).unwrap_err().to_string();
        assert_eq!(e, "sh: broken");
        assert!(run("nightporter-no-such-tool", &[]).unwrap_err().to_string().contains("run nightporter-no-such-tool"));
        assert!(on_path("nightporter-no-such-tool").is_none() && on_path("sh").is_some());
    }
}
