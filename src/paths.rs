//! Where Night Porter keeps things. XDG layout on macOS and Linux (`~/.config`, `~/.local/share`).

use anyhow::{Context, Result};
use etcetera::BaseStrategy;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    /// Honours `NIGHTPORTER_HOME` (used by tests and for running several isolated instances).
    pub fn detect() -> Result<Self> {
        if let Some(home) = std::env::var_os("NIGHTPORTER_HOME") {
            let home = PathBuf::from(home);
            return Ok(Self::at(&home.join("config"), &home.join("data")));
        }
        let s = etcetera::choose_base_strategy().context("cannot find home directory")?;
        Ok(Self::at(&s.config_dir().join("nightporter"), &s.data_dir().join("nightporter")))
    }

    pub fn at(config_dir: &Path, data_dir: &Path) -> Self {
        Self { config_dir: config_dir.into(), data_dir: data_dir.into() }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn repos_dir(&self) -> PathBuf {
        self.config_dir.join("repos")
    }
    pub fn repo_file(&self, repo: &crate::forge::RepoId) -> PathBuf {
        self.repos_dir().join(repo.forge.as_str()).join(format!("{}.toml", repo.path))
    }
    pub fn packs_dir(&self) -> PathBuf {
        self.data_dir.join("packs")
    }
    pub fn db(&self) -> PathBuf {
        self.data_dir.join("state.db")
    }
    pub fn lock(&self) -> PathBuf {
        self.data_dir.join("daemon.lock")
    }
    pub fn jobs_dir(&self) -> PathBuf {
        self.data_dir.join("jobs")
    }
    pub fn job_dir(&self, id: i64) -> PathBuf {
        self.jobs_dir().join(id.to_string())
    }
    pub fn worktrees_dir(&self) -> PathBuf {
        self.data_dir.join("worktrees")
    }
    pub fn log_file(&self) -> PathBuf {
        self.data_dir.join("daemon.log")
    }

    pub fn ensure(&self) -> Result<()> {
        for d in [&self.config_dir, &self.data_dir, &self.repos_dir(), &self.jobs_dir(), &self.worktrees_dir()] {
            std::fs::create_dir_all(d).with_context(|| format!("create {}", d.display()))?;
        }
        Ok(())
    }
}

/// Expand a leading `~/`.
pub fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => std::env::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(p)),
        None => PathBuf::from(p),
    }
}
