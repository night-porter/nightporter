//! Jira Cloud through Atlassian's `acli`. `acli` has one current site for the whole machine (your
//! terminal's too), so every call switches to its source's site and back, under a file lock that
//! the daemon and the CLI share.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Acli {
    bin: PathBuf,
    lock: PathBuf,
}

impl Acli {
    pub fn new(paths: &crate::paths::Paths) -> Self {
        Self { bin: "acli".into(), lock: paths.data_dir.join("acli.lock") }
    }

    /// `acli jira <args>` on `site`. Switches back to the site that was current before, and fails
    /// if `acli` doesn't end up there (someone else switched meanwhile, or the switch back failed).
    pub fn run(&self, site: &str, args: &[&str]) -> Result<String> {
        if let Some(d) = self.lock.parent() {
            std::fs::create_dir_all(d)?;
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock)
            .with_context(|| format!("open {}", self.lock.display()))?;
        lock.lock().with_context(|| format!("lock {}", self.lock.display()))?;
        let before = self.current_site()?;
        if before != site {
            self.acli(&["auth", "switch", "--site", site])
                .with_context(|| format!("acli: switch to {site} (log in: acli jira auth login)"))?;
        }
        let out = self.acli(args);
        if before != site {
            self.acli(&["auth", "switch", "--site", &before]).with_context(|| format!("acli: switch back to {before}"))?;
            let now = self.current_site()?;
            if now != before {
                bail!("acli is on {now}, not {before} as before Night Porter used it; check `acli jira auth status`");
            }
        }
        out
    }

    fn current_site(&self) -> Result<String> {
        let out = self.acli(&["auth", "status"]).context("acli: not logged in to Jira (acli jira auth login)")?;
        out.lines()
            .find_map(|l| l.trim().strip_prefix("Site:").map(|s| s.trim().to_string()))
            .context("acli jira auth status: no Site line")
    }

    fn acli(&self, args: &[&str]) -> Result<String> {
        let o = Command::new(&self.bin).arg("jira").args(args).output().with_context(|| format!("run {}", self.bin.display()))?;
        if !o.status.success() {
            bail!("acli jira {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&o.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

/// The sites `acli` is logged in to, from its own config (`- site: x` lines).
pub fn sites() -> Vec<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    sites_in(&home.join(".config/acli/jira_config.yaml"))
}

fn sites_in(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().trim_start_matches("- ").strip_prefix("site:").map(|s| s.trim().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `acli` keeping its current site in a file and logging every call.
    fn fake(dir: &Path) -> Acli {
        let bin = dir.join("acli");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nd={d}\necho \"$*\" >> $d/calls\n\
                 case \"$2 $3\" in\n\
                   'auth status') echo '✓ Authenticated'; echo \"  Site: $(cat $d/site)\" ;;\n\
                   'auth switch') [ \"$5\" = nowhere ] && exit 1; echo \"$5\" > $d/site ;;\n\
                   *) echo \"ran on $(cat $d/site)\" ;;\n\
                 esac\n",
                d = dir.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("site"), "home.atlassian.net\n").unwrap();
        Acli { bin, lock: dir.join("data/lock") }
    }

    #[test]
    fn runs_on_the_site_then_switches_back() {
        let d = tempfile::tempdir().unwrap();
        let a = fake(d.path());
        assert_eq!(a.run("other.atlassian.net", &["workitem", "search"]).unwrap().trim(), "ran on other.atlassian.net");
        assert_eq!(std::fs::read_to_string(d.path().join("site")).unwrap().trim(), "home.atlassian.net");
        // Already there: no switching.
        let calls = || std::fs::read_to_string(d.path().join("calls")).unwrap().lines().count();
        let n = calls();
        a.run("home.atlassian.net", &["workitem", "search"]).unwrap();
        assert_eq!(calls() - n, 2, "status + the command");
        let e = a.run("nowhere", &["workitem", "search"]).unwrap_err();
        assert!(format!("{e:#}").contains("switch to nowhere"), "{e:#}");
    }

    #[test]
    fn reads_logged_in_sites() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("jira_config.yaml");
        std::fs::write(&f, "version: 1\nprofiles:\n    - site: a.jira.com\n      email: x\n    - site: b.atlassian.net\n").unwrap();
        assert_eq!(sites_in(&f), ["a.jira.com", "b.atlassian.net"]);
    }
}
