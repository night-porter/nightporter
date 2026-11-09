//! "Is there a newer Night Porter?" One anonymous GET of the public latest-release page on
//! GitHub, at most once a day, from the daemon. No ID, no usage data, nothing about your repos;
//! GitHub sees an IP address and the `nightporter/<version>` user agent, as with any download.
//! Off with `check_updates = false`. Night Porter never updates itself: it tells you the command.

use crate::state::Db;
use anyhow::{Context, Result};

const LATEST: &str = "https://api.github.com/repos/night-porter/nightporter/releases/latest";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const DAY: i64 = 86_400;

/// `0.2.10` → (0, 2, 10); a leading `v` and any `-pre` suffix are ignored.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let mut it = v.split(['.', '-', '+']).map(str::parse::<u64>);
    Some((it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?))
}

pub fn newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

pub async fn fetch_latest() -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let r: Release = reqwest::Client::builder()
        .user_agent(concat!("nightporter/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(15))
        .build()?
        .get(LATEST)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("unexpected reply from GitHub releases")?;
    Ok(r.tag_name.trim_start_matches('v').to_string())
}

/// Daemon: check if the last check is a day old. Errors are quiet (offline is normal).
pub async fn daily_check(db: &std::sync::Mutex<Db>) {
    let due = db
        .lock()
        .unwrap()
        .get("update_checked_at")
        .ok()
        .flatten()
        .and_then(|t| t.parse::<i64>().ok())
        .is_none_or(|t| crate::state::now() - t > DAY);
    if !due {
        return;
    }
    let latest = fetch_latest().await;
    let db = db.lock().unwrap();
    let _ = db.set("update_checked_at", &crate::state::now().to_string());
    if let Ok(v) = latest {
        let _ = db.set("latest_version", &v);
    }
}

/// The newer version the daemon last saw, if any.
pub fn available(db: &Db) -> Option<String> {
    db.get("latest_version").ok().flatten().filter(|v| newer(v, CURRENT))
}

/// How to upgrade, guessed from where this binary lives.
pub fn how_to_upgrade() -> &'static str {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
    if exe.contains("/cellar/") || exe.contains("homebrew") || exe.contains("linuxbrew") {
        "brew upgrade nightporter"
    } else if exe.contains(".cargo") {
        "cargo install nightporter --locked   (or run the install script again)"
    } else {
        "see https://night-porter.com for the install command you used"
    }
}

/// One line for the TUI header and `status`, or nothing when up to date.
pub fn banner(db: &Db) -> Option<String> {
    available(db).map(|v| format!("Night Porter {v} is out (you have {CURRENT}): {}", how_to_upgrade()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn compares_versions() {
        assert!(super::newer("0.2.0", "0.1.9") && super::newer("v0.10.0", "0.9.3") && super::newer("1.0.0", "0.99.99"));
        assert!(!super::newer("0.1.1", "0.1.1") && !super::newer("0.1.0", "0.1.1") && !super::newer("garbage", "0.1.0"));
        assert!(super::newer("0.3.1-rc.1", "0.3.0") && !super::newer("1.2", "0.1.0"), "suffixes ignored, short versions never newer");
    }

    #[test]
    fn banner_only_for_a_newer_release() {
        let db = crate::state::Db::memory().unwrap();
        assert!(super::banner(&db).is_none());
        db.set("latest_version", super::CURRENT).unwrap();
        assert!(super::available(&db).is_none() && super::banner(&db).is_none());
        db.set("latest_version", "999.0.0").unwrap();
        let b = super::banner(&db).unwrap();
        assert!(b.starts_with(&format!("Night Porter 999.0.0 is out (you have {}): ", super::CURRENT)), "{b}");
        assert!(b.ends_with(super::how_to_upgrade()));
    }

    #[tokio::test]
    async fn daily_check_waits_a_day() {
        let db = std::sync::Mutex::new(crate::state::Db::memory().unwrap());
        let recent = (crate::state::now() - 60).to_string();
        db.lock().unwrap().set("update_checked_at", &recent).unwrap();
        // Checked a minute ago: no request, nothing changes.
        super::daily_check(&db).await;
        let db = db.lock().unwrap();
        assert_eq!(db.get("update_checked_at").unwrap().as_deref(), Some(recent.as_str()));
        assert!(db.get("latest_version").unwrap().is_none());
    }
}
