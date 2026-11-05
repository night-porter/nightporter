//! Install the daemon as a per-user service: launchd (macOS) or systemd --user (Linux).

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

const LABEL: &str = "dev.nightporter.daemon";

fn exe() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().into())
}

/// The daemon needs the same PATH as the user's shell to find gh/glab/claude/opencode.
fn path_env() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin".into())
}

fn home() -> Result<PathBuf> {
    std::env::home_dir().context("no home directory")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn plist(exe: &str, path: &str, log: &str) -> String {
    let (exe, path, log) = (xml_escape(exe), xml_escape(path), xml_escape(log));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>daemon</string></array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>30</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#
    )
}

pub fn unit(exe: &str, path: &str) -> String {
    format!(
        "[Unit]\nDescription=Night Porter daemon\nAfter=network-online.target\n\n[Service]\nExecStart=\"{exe}\" daemon\nEnvironment=\"PATH={path}\"\nRestart=on-failure\nRestartSec=30\n\n[Install]\nWantedBy=default.target\n"
    )
}

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd.output().with_context(|| format!("run {cmd:?}"))?;
    if !out.status.success() {
        bail!("{cmd:?}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn install(log: &std::path::Path) -> Result<String> {
    let exe = exe()?;
    if cfg!(target_os = "macos") {
        let p = home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist"));
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, plist(&exe, &path_env(), &log.to_string_lossy()))?;
        let uid = String::from_utf8(Command::new("id").arg("-u").output()?.stdout)?.trim().to_string();
        let _ = Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{LABEL}")]).output();
        run(Command::new("launchctl").args(["bootstrap", &format!("gui/{uid}")]).arg(&p))?;
        Ok(format!("installed launchd agent {}", p.display()))
    } else if cfg!(target_os = "linux") {
        let p = home()?.join(".config/systemd/user/nightporter.service");
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, unit(&exe, &path_env()))?;
        run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
        run(Command::new("systemctl").args(["--user", "enable", "--now", "nightporter.service"]))?;
        Ok(format!("installed systemd user unit {}", p.display()))
    } else {
        bail!("no service installer for this OS yet; run `nightporter daemon` yourself")
    }
}

pub fn uninstall() -> Result<String> {
    if cfg!(target_os = "macos") {
        let p = home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist"));
        let uid = String::from_utf8(Command::new("id").arg("-u").output()?.stdout)?.trim().to_string();
        let _ = Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{LABEL}")]).output();
        let _ = std::fs::remove_file(&p);
        Ok("removed launchd agent".into())
    } else if cfg!(target_os = "linux") {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", "nightporter.service"]).output();
        let _ = std::fs::remove_file(home()?.join(".config/systemd/user/nightporter.service"));
        Ok("removed systemd user unit".into())
    } else {
        bail!("no service on this OS")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn plist_escapes() {
        let p = super::plist("/a&b/nightporter", "/bin", "/tmp/log");
        assert!(p.contains("/a&amp;b/nightporter") && p.contains("<string>daemon</string>"));
    }
}
