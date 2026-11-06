//! Packs: shareable prompt-only extensions (steps, reply styles, env presets), pinned to a commit.
//! Marketplaces: git repos with a `marketplace.toml` index. No server, no executable code.

use crate::paths::Paths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Hook {
    /// Read-only gate before any work; may drop the job.
    Filter,
    /// Extra instructions given to the main agent before it starts.
    PreFix,
    /// Extra instructions given to the main agent after its main task.
    PostFix,
    /// Read-only gate after the agent; may block or require approval.
    PrePush,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub styles: Vec<Named>,
    #[serde(default)]
    pub envs: Vec<Named>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    pub hook: Hook,
    pub prompt: String,
    /// The step asks for write access (only meaningful for pre/post_fix, which share the main run).
    #[serde(default)]
    pub write: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Named {
    pub name: String,
    #[serde(alias = "prompt")]
    pub file: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Lock {
    #[serde(default)]
    pub packs: BTreeMap<String, Locked>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Locked {
    pub url: String,
    pub rev: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Market {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub packs: Vec<MarketEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MarketEntry {
    pub name: String,
    pub url: String,
    pub rev: String,
    #[serde(default)]
    pub description: String,
}

pub const OFFICIAL_MARKET: &str = "https://github.com/night-porter/marketplace";

/// Only these files may appear in a pack. No scripts, no binaries.
const ALLOWED_EXT: &[&str] = &["md", "toml", "txt"];

fn lock_path(paths: &Paths) -> PathBuf {
    paths.config_dir.join("packs.lock")
}

pub fn lock(paths: &Paths) -> Lock {
    std::fs::read_to_string(lock_path(paths)).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
}

fn save_lock(paths: &Paths, l: &Lock) -> Result<()> {
    crate::config::write_atomic(&lock_path(paths), toml::to_string_pretty(l)?.as_bytes())
}

pub fn pack_dir(paths: &Paths, name: &str) -> PathBuf {
    paths.packs_dir().join(name)
}

pub fn manifest(dir: &Path) -> Result<Manifest> {
    let t = std::fs::read_to_string(dir.join("pack.toml")).with_context(|| format!("{}: no pack.toml", dir.display()))?;
    let m: Manifest = toml::from_str(&t).context("invalid pack.toml")?;
    if !m.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || m.name.is_empty() {
        bail!("pack name {:?} must be [A-Za-z0-9_-]", m.name);
    }
    for f in m.steps.iter().map(|s| &s.prompt).chain(m.styles.iter().map(|s| &s.file)).chain(m.envs.iter().map(|s| &s.file)) {
        safe_rel(f)?;
    }
    Ok(m)
}

/// Reject absolute paths and `..` so a manifest can't read outside its own directory.
fn safe_rel(p: &str) -> Result<()> {
    let path = Path::new(p);
    if path.is_absolute() || path.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        bail!("pack file path {p:?} must be relative and stay inside the pack");
    }
    Ok(())
}

/// Every file must be text with an allowed extension and not executable.
pub fn audit(dir: &Path) -> Result<()> {
    fn walk(d: &Path, root: &Path) -> Result<()> {
        for e in std::fs::read_dir(d)? {
            let e = e?;
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let ft = e.file_type()?;
            if ft.is_symlink() {
                bail!("{}: symlinks are not allowed in packs", p.strip_prefix(root)?.display());
            }
            if ft.is_dir() {
                walk(&p, root)?;
                continue;
            }
            let ext_ok = p.extension().is_some_and(|x| ALLOWED_EXT.contains(&x.to_string_lossy().as_ref()))
                || matches!(name.as_str(), "LICENSE" | "LICENSE-MIT" | "LICENSE-APACHE" | "README" | "NOTICE");
            if !ext_ok {
                bail!("{}: packs may only contain .md, .toml and .txt files", p.strip_prefix(root)?.display());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if e.metadata()?.permissions().mode() & 0o111 != 0 {
                    bail!("{}: executable files are not allowed in packs", p.strip_prefix(root)?.display());
                }
            }
        }
        Ok(())
    }
    walk(dir, dir)
}

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    crate::git::git(dir, args).await
}

/// Clone `url` at `rev` into a staging dir, audit it, and return (dir, manifest, resolved sha).
/// URLs and revs come from marketplace files: never let them become git options or exotic
/// transports.
pub fn validate_source(url: &str, rev: &str) -> Result<()> {
    let url_ok = (url.starts_with("https://") || url.starts_with("ssh://") || url.starts_with("git@"))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control());
    if !url_ok {
        bail!("pack/marketplace URL must be https://, ssh:// or git@ (got {url:?})");
    }
    let rev_ok = !rev.is_empty() && !rev.starts_with('-') && rev.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c));
    if !rev_ok {
        bail!("invalid rev {rev:?}");
    }
    Ok(())
}

pub async fn fetch(paths: &Paths, url: &str, rev: &str) -> Result<(PathBuf, Manifest, String)> {
    validate_source(url, rev)?;
    let staging = paths.packs_dir().join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    git(&staging, &["init", "--quiet"]).await?;
    git(&staging, &["fetch", "--quiet", "--depth", "1", "--end-of-options", url, rev])
        .await
        .with_context(|| format!("fetch {url} {rev}"))?;
    git(&staging, &["-c", "core.symlinks=false", "checkout", "--quiet", "FETCH_HEAD"]).await?;
    let sha = git(&staging, &["rev-parse", "HEAD"]).await?;
    audit(&staging)?;
    let m = manifest(&staging)?;
    Ok((staging, m, sha))
}

/// Human summary shown before install/update.
pub fn describe(m: &Manifest) -> String {
    let mut s = format!("{} {} — {}\n", m.name, m.version, m.description);
    for st in &m.steps {
        s.push_str(&format!("  step {:?} at {:?}{}\n", st.name, st.hook, if st.write { "  ⚠ requests WRITE access" } else { "" }));
    }
    for st in &m.styles {
        s.push_str(&format!("  reply style {:?}\n", st.name));
    }
    for e in &m.envs {
        s.push_str(&format!("  env preset {:?}\n", e.name));
    }
    s
}

/// Move a fetched staging dir into place and record it in the lock file.
pub fn commit(paths: &Paths, staging: &Path, m: &Manifest, url: &str, sha: &str) -> Result<()> {
    let dest = pack_dir(paths, &m.name);
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_dir_all(staging.join(".git"));
    std::fs::rename(staging, &dest)?;
    let mut l = lock(paths);
    l.packs.insert(m.name.clone(), Locked { url: url.into(), rev: sha.into() });
    save_lock(paths, &l)
}

pub fn remove(paths: &Paths, name: &str) -> Result<()> {
    let mut l = lock(paths);
    if l.packs.remove(name).is_none() {
        bail!("pack {name:?} is not installed");
    }
    let _ = std::fs::remove_dir_all(pack_dir(paths, name));
    save_lock(paths, &l)
}

/// Unified diff of prompt files between the installed pack and a staged update.
pub fn diff_dirs(old: &Path, new: &Path) -> String {
    let out = std::process::Command::new("git").args(["diff", "--no-index", "--no-color", "--"]).arg(old).arg(new).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

// ---- runtime lookups ----

/// Prompts of enabled packs for a hook, as (pack/step name, text).
pub fn steps(paths: &Paths, enabled: &[String], hook: Hook) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    for name in enabled {
        let dir = pack_dir(paths, name);
        let Ok(m) = manifest(&dir) else {
            tracing::warn!("pack {name:?} is enabled but not installed");
            continue;
        };
        for s in m.steps.iter().filter(|s| s.hook == hook) {
            if let Ok(t) = std::fs::read_to_string(dir.join(&s.prompt)) {
                out.push((format!("{name}/{}", s.name), t, s.write));
            }
        }
    }
    out
}

/// A reply style from any installed pack: `pack/style` or just `style`.
pub fn style(paths: &Paths, name: &str) -> Option<String> {
    let (pack, style) = name.split_once('/').map_or((None, name), |(p, s)| (Some(p), s));
    for (pname, _) in lock(paths).packs {
        if pack.is_some_and(|p| p != pname) {
            continue;
        }
        let dir = pack_dir(paths, &pname);
        let m = manifest(&dir).ok()?;
        if let Some(s) = m.styles.iter().find(|s| s.name == style) {
            return std::fs::read_to_string(dir.join(&s.file)).ok();
        }
    }
    None
}

/// An env preset (`[env]` table) from a pack, for `nightporter pack env <pack>/<name>`.
pub fn env_preset(paths: &Paths, name: &str) -> Result<String> {
    let (pack, env) = name.split_once('/').context("use <pack>/<env>")?;
    let dir = pack_dir(paths, pack);
    let m = manifest(&dir)?;
    let e = m.envs.iter().find(|e| e.name == env).with_context(|| format!("{pack} has no env {env:?}"))?;
    Ok(std::fs::read_to_string(dir.join(&e.file))?)
}

// ---- marketplaces ----

pub fn market_dir(paths: &Paths, url: &str) -> PathBuf {
    let slug: String = url.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    paths.data_dir.join("markets").join(slug)
}

pub async fn market_sync(paths: &Paths, url: &str) -> Result<Market> {
    validate_source(url, "HEAD")?;
    let dir = market_dir(paths, url);
    if dir.join(".git").exists() {
        git(&dir, &["pull", "--quiet", "--ff-only"]).await?;
    } else {
        std::fs::create_dir_all(dir.parent().unwrap_or(&dir))?;
        let parent = dir.parent().unwrap_or(&dir).to_path_buf();
        git(&parent, &["clone", "--quiet", "--depth", "1", "--end-of-options", url, &dir.to_string_lossy()]).await?;
    }
    market(paths, url)
}

pub fn market(paths: &Paths, url: &str) -> Result<Market> {
    let t = std::fs::read_to_string(market_dir(paths, url).join("marketplace.toml"))
        .with_context(|| format!("{url}: no marketplace.toml (run `nightporter market update`)"))?;
    Ok(toml::from_str(&t)?)
}

/// Find a pack by name across configured marketplaces (official first).
pub fn find(paths: &Paths, markets: &[String], name: &str) -> Option<MarketEntry> {
    std::iter::once(OFFICIAL_MARKET.to_string())
        .chain(markets.iter().cloned())
        .filter_map(|u| market(paths, &u).ok())
        .find_map(|m| m.packs.into_iter().find(|p| p.name == name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_pack(dir: &Path) {
        std::fs::create_dir_all(dir.join("steps")).unwrap();
        std::fs::write(
            dir.join("pack.toml"),
            r#"name = "a11y"
description = "accessibility review"
[[steps]]
name = "review"
hook = "post_fix"
prompt = "steps/a11y.md"
[[styles]]
name = "haiku-pirate"
file = "pirate.md"
"#,
        )
        .unwrap();
        std::fs::write(dir.join("steps/a11y.md"), "Check alt text.").unwrap();
        std::fs::write(dir.join("pirate.md"), "Arr.").unwrap();
    }

    #[test]
    fn manifest_and_audit() {
        let d = tempfile::tempdir().unwrap();
        mk_pack(d.path());
        audit(d.path()).unwrap();
        let m = manifest(d.path()).unwrap();
        assert!(describe(&m).contains("post_fix") || describe(&m).contains("PostFix"));
        std::fs::write(d.path().join("hook.sh"), "rm -rf ~").unwrap();
        assert!(audit(d.path()).is_err());
    }

    #[test]
    fn sources_are_validated() {
        assert!(validate_source("--upload-pack=touch /tmp/x", ".").is_err());
        assert!(validate_source("ext::sh -c id", "HEAD").is_err());
        assert!(validate_source("https://github.com/a/b", "--output=x").is_err());
        assert!(validate_source("https://github.com/a/b", "3f2c9a0").is_ok());
        assert!(validate_source("git@github.com:a/b.git", "main").is_ok());
    }

    #[test]
    fn rejects_escaping_paths() {
        assert!(safe_rel("../../etc/passwd").is_err());
        assert!(safe_rel("/etc/passwd").is_err());
        assert!(safe_rel("steps/x.md").is_ok());
    }

    #[test]
    fn runtime_lookup() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        let staging = d.path().join("stage");
        mk_pack(&staging);
        let m = manifest(&staging).unwrap();
        std::fs::create_dir_all(paths.packs_dir()).unwrap();
        commit(&paths, &staging, &m, "https://example.com/a11y", "abc").unwrap();
        assert_eq!(lock(&paths).packs["a11y"].rev, "abc");
        let s = steps(&paths, &["a11y".into()], Hook::PostFix);
        assert_eq!(s[0].1, "Check alt text.");
        assert!(steps(&paths, &["a11y".into()], Hook::Filter).is_empty());
        assert_eq!(style(&paths, "haiku-pirate").as_deref(), Some("Arr."));
        assert_eq!(style(&paths, "a11y/haiku-pirate").as_deref(), Some("Arr."));
        remove(&paths, "a11y").unwrap();
        assert!(style(&paths, "haiku-pirate").is_none());
    }
}
