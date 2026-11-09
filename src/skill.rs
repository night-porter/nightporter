//! `nightporter skill`: tell coding agents how to work with Night Porter. The guide is printed by
//! the installed binary (`skill show`), so it always matches this version; `skill setup` only
//! installs a small stub that points at it.

use anyhow::{Context, Result};
use std::path::PathBuf;

pub const GUIDE: &str = include_str!("../prompts/skill.md");

const STUB: &str = "---
name: nightporter
description: Work with Night Porter, the daemon that handles review comments, failing CI and merge conflicts on the user's PRs overnight. Use when the user asks what Night Porter did, wants to review, approve or take over a Night Porter job, or check its queue, inbox or health.
---
<!-- Installed by `nightporter skill setup`. Remove it with `nightporter skill remove`. -->

Run `nightporter skill show` and follow the guide it prints. It comes from the installed
Night Porter, so it matches the commands available in that version.
";

/// Claude Code's personal skills folder (respects `CLAUDE_CONFIG_DIR`).
fn skill_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) => PathBuf::from(d),
        None => std::env::home_dir().context("no home directory")?.join(".claude"),
    };
    Ok(base.join("skills").join("nightporter"))
}

pub fn setup() -> Result<String> {
    setup_in(&skill_dir()?)
}

pub fn remove() -> Result<String> {
    remove_in(&skill_dir()?)
}

fn setup_in(dir: &std::path::Path) -> Result<String> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("SKILL.md"), STUB)?;
    Ok(format!("installed {}", dir.join("SKILL.md").display()))
}

fn remove_in(dir: &std::path::Path) -> Result<String> {
    if dir.join("SKILL.md").exists() {
        std::fs::remove_dir_all(dir)?;
        Ok(format!("removed {}", dir.display()))
    } else {
        Ok("not installed".into())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn guide_and_stub_name_real_commands() {
        assert!(super::STUB.starts_with("---\nname: nightporter\n"));
        for cmd in ["nightporter status", "nightporter show", "nightporter wait", "nightporter events", "nightporter handoff"] {
            assert!(super::GUIDE.contains(cmd), "guide should mention `{cmd}`");
        }
    }

    #[test]
    fn setup_writes_the_stub_and_remove_takes_it_away() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("skills/nightporter");
        assert_eq!(super::remove_in(&dir).unwrap(), "not installed");
        assert!(super::setup_in(&dir).unwrap().ends_with("nightporter/SKILL.md"));
        assert_eq!(std::fs::read_to_string(dir.join("SKILL.md")).unwrap(), super::STUB);
        assert!(super::remove_in(&dir).unwrap().starts_with("removed ") && !dir.exists());
    }
}
