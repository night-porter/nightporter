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

fn service_file() -> Option<PathBuf> {
    let h = home().ok()?;
    let p = if cfg!(target_os = "macos") {
        h.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
    } else {
        h.join(".config/systemd/user/nightporter.service")
    };
    p.is_file().then_some(p)
}

/// Is the daemon set to start at login? The service runs the default instance, so an isolated
/// `NIGHTPORTER_HOME` never goes through it.
pub fn installed() -> bool {
    std::env::var_os("NIGHTPORTER_HOME").is_none() && service_file().is_some()
}

fn wait_for(paths: &crate::paths::Paths, running: bool) -> bool {
    for _ in 0..50 {
        if paths.daemon_running() == running {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    false
}

/// Start the daemon in the background: through the login service if it's installed, otherwise
/// as a detached process logging to `daemon.log`.
pub fn start(paths: &crate::paths::Paths) -> Result<String> {
    if paths.daemon_running() {
        return Ok("Night Porter is already running".into());
    }
    let via_service = installed()
        && if cfg!(target_os = "macos") {
            let uid = String::from_utf8(Command::new("id").arg("-u").output()?.stdout)?.trim().to_string();
            run(Command::new("launchctl").args(["kickstart", &format!("gui/{uid}/{LABEL}")])).is_ok()
        } else {
            run(Command::new("systemctl").args(["--user", "start", "nightporter.service"])).is_ok()
        };
    if !via_service {
        paths.ensure()?;
        let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.log_file())?;
        let mut cmd = Command::new(exe()?);
        cmd.arg("daemon").stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
        // Its own process group: closing this terminal doesn't take it down.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        cmd.spawn().context("start the daemon")?;
    }
    if !wait_for(paths, true) {
        bail!("the daemon didn't start; see {}", paths.log_file().display());
    }
    Ok("Night Porter is running: it watches your PRs in the background".into())
}

/// Stop the daemon, however it was started. Running jobs stop too and are queued again on the
/// next start; nothing half-done is pushed.
pub fn stop(paths: &crate::paths::Paths) -> Result<String> {
    if !paths.daemon_running() {
        return Ok("Night Porter is not running".into());
    }
    let pid = std::fs::read_to_string(paths.pid_file())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .context("can't find the daemon's process id; stop it with your system's task manager")?;
    #[cfg(unix)]
    {
        let pid = libc::pid_t::try_from(pid)?;
        // SAFETY: plain syscall; SIGTERM makes the daemon exit cleanly.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            bail!("couldn't signal process {pid}: {}", std::io::Error::last_os_error());
        }
    }
    #[cfg(not(unix))]
    run(Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]))?;
    if !wait_for(paths, false) {
        bail!("the daemon (process {pid}) is still running");
    }
    let _ = std::fs::remove_file(paths.pid_file());
    Ok(if installed() {
        "Night Porter stopped. It starts again when you log in; `nightporter service uninstall` turns that off.".into()
    } else {
        "Night Porter stopped".into()
    })
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
        let p = super::plist("/x", "/bin:<a>", "/tmp/\"log\"");
        assert!(p.contains("<string>/bin:&lt;a&gt;</string>") && p.contains("<string>/tmp/&quot;log&quot;</string>"), "{p}");
    }

    #[test]
    fn systemd_unit_runs_the_daemon_with_path() {
        let u = super::unit("/opt/my bin/nightporter", "/usr/bin:/bin");
        assert!(u.contains("ExecStart=\"/opt/my bin/nightporter\" daemon\n") && u.contains("Environment=\"PATH=/usr/bin:/bin\"\n"));
        assert!(u.contains("Restart=on-failure") && u.ends_with("WantedBy=default.target\n"));
    }

    #[test]
    fn start_and_stop_check_the_daemon_lock_first() {
        let d = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        assert_eq!(super::stop(&paths).unwrap(), "Night Porter is not running");
        assert!(super::wait_for(&paths, false));
        // Hold the lock the way a running daemon does.
        let f = std::fs::File::create(paths.lock()).unwrap();
        f.lock().unwrap();
        assert_eq!(super::start(&paths).unwrap(), "Night Porter is already running");
        let e = super::stop(&paths).unwrap_err().to_string();
        assert!(e.contains("can't find the daemon's process id"), "{e}");
        std::fs::write(paths.pid_file(), "not a pid").unwrap();
        assert!(super::stop(&paths).is_err());
        assert!(super::wait_for(&paths, true));
    }

    #[test]
    fn run_reports_failures_with_stderr() {
        super::run(std::process::Command::new("true").arg("x")).unwrap();
        let e = super::run(std::process::Command::new("sh").args(["-c", "echo nope >&2; exit 1"])).unwrap_err().to_string();
        assert!(e.ends_with(": nope"), "{e}");
        assert!(super::run(&mut std::process::Command::new("nightporter-no-such-tool")).is_err());
    }
}
