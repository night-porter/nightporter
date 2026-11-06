//! Taking over a job interactively: open it in herdr (<https://herdr.dev>) or Rex
//! (<https://www.superlogical.com/rex>) at the job's worktree, with an agent that's already briefed.

use crate::config::{AgentKind, Handoff};
use crate::paths::Paths;
use crate::state::{Db, Job, JobStatus};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

pub fn take_over(paths: &Paths, db: &Db, j: &Job) -> Result<String> {
    if matches!(j.status, JobStatus::Running | JobStatus::Queued | JobStatus::Approved) {
        bail!("job {} is {}; wait for it to finish before taking over", j.id, j.status.label());
    }
    if j.worktree.is_empty() || !std::path::Path::new(&j.worktree).is_dir() {
        bail!("job {} has no worktree (it may have been cleaned up)", j.id);
    }
    let choice = crate::config::Config::load(paths).map(|c| c.handoff).unwrap_or_default();
    let agent = crate::agent_for(paths, &j.repo);
    match pick(choice)? {
        Handoff::Rex => rex(db, j, agent),
        _ => crate::herdr::handoff(db, j, agent),
    }
}

/// `auto`: the terminal you're in (herdr or Rex), else whichever is installed, herdr first.
fn pick(choice: Handoff) -> Result<Handoff> {
    Ok(match choice {
        Handoff::Auto if std::env::var_os("HERDR_BIN_PATH").is_some() => Handoff::Herdr,
        Handoff::Auto if std::env::var_os("REX_SESSION").is_some() => Handoff::Rex,
        Handoff::Auto if on_path("herdr").is_some() => Handoff::Herdr,
        Handoff::Auto if rex_bin().is_some() => Handoff::Rex,
        Handoff::Auto => bail!("neither herdr nor Rex was found; install one (https://herdr.dev, https://www.superlogical.com/rex)"),
        c => c,
    })
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

/// A new Rex session at the worktree, running the agent with the briefing as its first prompt.
fn rex(db: &Db, j: &Job, agent: AgentKind) -> Result<String> {
    let bin = rex_bin().context("Rex not found: install it from https://www.superlogical.com/rex or set REX_BIN")?;
    let slug: String = j.repo.path.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    // Labels must be unique; a suffix lets you take over the same job twice.
    let label = format!("np-{slug}-{}-{}", j.number, crate::state::now() % 100_000);
    // Through your login shell, so the agent is on PATH as it is in your terminal.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let brief = crate::herdr::briefing(db, j);
    let mut cmd = Command::new(&bin);
    cmd.args(["new", &label, "-c", &j.worktree, "--keep-open", "--shell", &shell, "--json", "--", agent.bin()]);
    match agent {
        AgentKind::Claude | AgentKind::Codex => cmd.arg(&brief),
        AgentKind::OpenCode => cmd.args(["--prompt", &brief]),
    };
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
        let msg = rex(&db, &j, AgentKind::Claude).unwrap();
        assert!(msg.contains("np-acme-api-7-"), "{msg}");
        let calls = std::fs::read_to_string(&log).unwrap();
        let args: Vec<&str> = calls.lines().collect();
        assert_eq!(&args[..1], ["new"]);
        assert!(args.windows(2).any(|w| w == ["-c", d.path().to_str().unwrap()]), "{calls}");
        let dash = args.iter().position(|a| *a == "--").unwrap();
        assert_eq!(args[dash + 1], "claude");
        assert!(calls.contains("renamed foo"), "the agent gets the briefing: {calls}");
        assert_eq!(pick(Handoff::Rex).unwrap(), Handoff::Rex);
        unsafe { std::env::remove_var("REX_BIN") };
    }
}
