//! Find local clones under the code dir and map forge repos to them (via any remote).

use crate::config::Account;
use crate::forge::RepoId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `RepoId` → clone path. A clone with several remotes (fork + upstream) maps all of them.
pub type CloneMap = BTreeMap<RepoId, PathBuf>;

pub fn scan(code_dir: &Path, depth: usize, accounts: &[Account]) -> CloneMap {
    let mut out = CloneMap::new();
    walk(code_dir, depth, accounts, &mut out);
    out
}

fn walk(dir: &Path, depth: usize, accounts: &[Account], out: &mut CloneMap) {
    let git = dir.join(".git");
    if git.is_dir() {
        if let Ok(cfg) = std::fs::read_to_string(git.join("config")) {
            for url in remote_urls(&cfg) {
                if let Some(id) = parse_remote(&url, accounts) {
                    out.entry(id).or_insert_with(|| dir.to_path_buf());
                }
            }
        }
        return; // don't descend into a repo (submodules/vendored clones aren't targets)
    }
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" || name == "vendor" || name == "target" {
            continue;
        }
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            walk(&e.path(), depth - 1, accounts, out);
        }
    }
}

fn remote_urls(cfg: &str) -> Vec<String> {
    let mut in_remote = false;
    let mut out = Vec::new();
    for l in cfg.lines().map(str::trim) {
        if l.starts_with('[') {
            in_remote = l.starts_with("[remote ");
        } else if in_remote && let Some(v) = l.strip_prefix("url").map(str::trim_start).and_then(|r| r.strip_prefix('=')) {
            out.push(v.trim().to_string());
        }
    }
    out
}

/// `git@host:o/r.git`, `ssh://git@host:22/o/r.git`, `https://host/o/r` → `RepoId` if the host
/// belongs to a configured account.
pub fn parse_remote(url: &str, accounts: &[Account]) -> Option<RepoId> {
    let (host, path) = if let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        rest.split_once('/')?
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map_or(rest, |(_, r)| r);
        let (hp, path) = rest.split_once('/')?;
        (hp.split(':').next()?, path)
    } else {
        let rest = url.split_once('@').map_or(url, |(_, r)| r);
        rest.split_once(':')?
    };
    let host = host.to_ascii_lowercase();
    let acct = accounts.iter().find(|a| a.host() == host)?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    (path.contains('/')).then(|| RepoId::new(acct.forge, &host, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::ForgeKind;

    fn accts() -> Vec<Account> {
        toml::from_str::<toml::Table>("[[a]]\nforge='github'\n[[a]]\nforge='gitlab'\nhost='git.drupalcode.org'").unwrap()["a"]
            .clone()
            .try_into()
            .unwrap()
    }

    #[test]
    fn remotes() {
        let a = accts();
        let gh = |p| Some(RepoId::new(ForgeKind::GitHub, "github.com", p));
        assert_eq!(parse_remote("git@github.com:o/r.git", &a), gh("o/r"));
        assert_eq!(parse_remote("https://github.com/o/r", &a), gh("o/r"));
        assert_eq!(parse_remote("https://x-token:abc@github.com/o/r.git", &a), gh("o/r"));
        assert_eq!(parse_remote("ssh://git@github.com:22/o/r.git", &a), gh("o/r"));
        assert_eq!(
            parse_remote("git@git.drupalcode.org:project/token.git", &a),
            Some(RepoId::new(ForgeKind::GitLab, "git.drupalcode.org", "project/token"))
        );
        assert_eq!(parse_remote("git@bitbucket.org:o/r.git", &a), None);
    }

    #[test]
    fn scans_nested_clones() {
        let d = tempfile::tempdir().unwrap();
        let mk = |rel: &str, url: &str| {
            let g = d.path().join(rel).join(".git");
            std::fs::create_dir_all(&g).unwrap();
            std::fs::write(
                g.join("config"),
                format!(
                    "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n[remote \"up\"]\n\turl = git@github.com:up/{rel}.git\n"
                ),
            )
            .unwrap();
        };
        mk("a", "git@github.com:me/a.git");
        mk("org/b", "https://github.com/me/b");
        mk("node_modules/c", "git@github.com:me/c.git");
        let m = scan(d.path(), 3, &accts());
        assert_eq!(m.len(), 4, "{m:?}"); // me/a, up/a, me/b, up/org/b
        assert!(m.keys().any(|k| k.path == "up/a"));
    }
}
