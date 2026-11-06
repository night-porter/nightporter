//! Running the coding agent: no forge credentials, restricted tools, bounded time.

use crate::checks::BUILTIN_PROTECTED;
use crate::config::{AgentKind, RepoSettings};
use crate::forge::Reply;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use tokio::process::Command;

/// What we got back from a run.
#[derive(Debug, Default)]
pub struct Outcome {
    pub summary: String,
    /// Gate steps answer `pass`, `block` or `needs_approval`.
    pub verdict: Option<String>,
    pub replies: Vec<Reply>,
    pub cost_usd: Option<f64>,
}

/// Git commands the agent may run. `git log/diff/show/blame/grep` are deliberately absent: they
/// accept `--output=<file>`, which would let the agent write anywhere. It can read files instead.
const GIT_ALLOWED: &[&str] =
    &["git status", "git add", "git commit", "git rev-parse", "git ls-files", "git merge", "git checkout --", "git restore", "git stash"];
/// Git commands that change the worktree; dropped for read-only steps.
const WRITE_GIT: &[&str] = &["git add", "git commit", "git merge", "git checkout --", "git restore", "git stash"];
const DENIED: &[&str] = &[
    "git push",
    "git remote",
    "git config",
    "git reset",
    "git rebase",
    "git commit --amend",
    "git filter-branch",
    "gh",
    "glab",
    "curl",
    "wget",
    "ssh",
    "scp",
    "nc",
    "rm -rf /",
    "sudo",
];

/// Home-relative credential locations the agent must not read.
const SECRET_PATHS: &[&str] = &[
    "~/.ssh/**",
    "~/.gnupg/**",
    "~/.aws/**",
    "~/.config/gh/**",
    "~/.config/glab-cli/**",
    "~/.git-credentials",
    "~/.netrc",
    "~/.docker/config.json",
    "~/.kube/**",
    "~/.npmrc",
    "~/Library/Keychains/**",
];

/// Folders under `~` that sandboxed commands may read: git identity and the usual toolchains and
/// package caches. Everything else in your home folder is off limits to the agent's commands.
const TOOLCHAIN_READS: &[&str] = &[
    "~/.claude/shell-snapshots",
    "~/.cargo",
    "~/.rustup",
    "~/.npm",
    "~/.nvm",
    "~/.volta",
    "~/.fnm",
    "~/.bun",
    "~/.deno",
    "~/.yarn",
    "~/.pnpm-store",
    "~/Library/pnpm",
    "~/.local/share/pnpm",
    "~/.cache",
    "~/Library/Caches",
    "~/go",
    "~/.composer",
    "~/.config/composer",
    "~/.pyenv",
    "~/.local/bin",
    "~/.local/lib",
    "~/.local/share/mise",
    "~/.asdf",
    "~/.sdkman",
    "~/.gradle",
    "~/.m2",
    "~/.rbenv",
    "~/.gem",
    "~/.dotnet",
    "~/.nuget",
];

/// Credential files inside the allowed toolchain folders: still denied.
const TOOLCHAIN_SECRETS: &[&str] = &[
    "~/.cargo/credentials",
    "~/.cargo/credentials.toml",
    "~/.composer/auth.json",
    "~/.config/composer/auth.json",
    "~/.gradle/gradle.properties",
    "~/.m2/settings.xml",
    "~/.m2/settings-security.xml",
    "~/.gem/credentials",
];

/// The git dir shared by the clone and its worktrees (`<clone>/.git`), read from `wt/.git`.
/// Sandboxed `git` needs it; it's the same repository the agent is working on.
fn common_git_dir(wt: &Path) -> Option<std::path::PathBuf> {
    let text = std::fs::read_to_string(wt.join(".git")).ok()?;
    let dir = Path::new(text.trim_end().strip_prefix("gitdir: ")?);
    Some(dir.parent()?.parent()?.to_path_buf())
}

/// Night Porter's own files the agent must not read: other jobs' prompts and diffs, the state db,
/// the log and the config. Built from the real paths (they move with XDG dirs and
/// `NIGHTPORTER_HOME`); worktrees, where the agent works, stay readable. `//` = absolute path.
fn own_paths() -> Vec<String> {
    let Ok(p) = crate::paths::Paths::detect() else { return Vec::new() };
    let abs = |x: std::path::PathBuf| format!("/{}", x.to_string_lossy());
    vec![format!("{}/**", abs(p.config_dir.clone())), format!("{}/**", abs(p.jobs_dir())), format!("{}*", abs(p.db())), abs(p.log_file())]
}

/// Model credentials the agent itself needs (and nothing else does).
const MODEL_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
];
const MODEL_PREFIXES: &[&str] = &["ANTHROPIC_", "CLAUDE_", "OPENCODE_"];

fn is_model_key(k: &str) -> bool {
    let u = k.to_ascii_uppercase();
    MODEL_KEYS.contains(&u.as_str()) || MODEL_PREFIXES.iter().any(|p| u.starts_with(p))
}

/// Environment for the user's env hooks (setup/teardown/cleanup): the agent's, minus model
/// credentials. Those commands run outside the sandbox and can read repo-controlled files.
pub fn hook_env(job_dir: &Path) -> Vec<(String, String)> {
    agent_env(job_dir).into_iter().filter(|(k, _)| !is_model_key(k) && k != "GIT_CONFIG_GLOBAL").collect()
}

/// The agent's own global git config: just the name and email you'd commit with in this repo
/// (including `includeIf` rules). Your real `~/.gitconfig` (credential helpers, URL rewrites,
/// includes) stays out of the sandbox's reach.
fn write_agent_git_config(job_dir: &Path, wt: &Path) {
    let dir = job_dir.join("git");
    let repo = common_git_dir(wt).and_then(|d| d.parent().map(Path::to_path_buf)).unwrap_or_else(|| wt.to_path_buf());
    let get = |k: &str| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["config", "--get", k])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let quote = |v: String| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(
        dir.join("config"),
        format!("[user]\n\tname = {}\n\temail = {}\n[commit]\n\tgpgsign = false\n", quote(get("user.name")), quote(get("user.email"))),
    );
}

/// Environment for the agent: everything from ours minus credentials, plus hardening.
pub fn agent_env(job_dir: &Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| {
            let u = k.to_ascii_uppercase();
            if is_model_key(&u) {
                return true;
            }
            let forge = ["GH_", "GITHUB_", "GITLAB_", "GLAB_", "CI_JOB_TOKEN", "NIGHTPORTER_", "SSH_AUTH_SOCK", "GIT_"]
                .iter()
                .any(|p| u.starts_with(p));
            let secretish =
                ["TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "KEY", "AUTH", "COOKIE", "SESSION", "DSN", "DATABASE_URL"]
                    .iter()
                    .any(|p| u.contains(p));
            !(forge || secretish)
        })
        .collect();
    let empty = job_dir.join("empty-config");
    let _ = std::fs::create_dir_all(&empty);
    let e = empty.to_string_lossy().to_string();
    let git_cfg = job_dir.join("git").join("config").to_string_lossy().to_string();
    env.extend(
        [
            ("GH_CONFIG_DIR", e.as_str()),
            ("GLAB_CONFIG_DIR", e.as_str()),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_ASKPASS", "/usr/bin/false"),
            ("SSH_ASKPASS", "/usr/bin/false"),
            ("GIT_SSH_COMMAND", "/usr/bin/false"),
            // Clear credential helpers so `git push` over https has nothing to use, and don't run
            // repo-controlled hooks (husky etc.) when the agent commits.
            ("GIT_CONFIG_COUNT", "3"),
            ("GIT_CONFIG_KEY_0", "credential.helper"),
            ("GIT_CONFIG_VALUE_0", ""),
            ("GIT_CONFIG_KEY_1", "core.hooksPath"),
            ("GIT_CONFIG_VALUE_1", "/dev/null"),
            ("GIT_CONFIG_KEY_2", "core.fsmonitor"),
            ("GIT_CONFIG_VALUE_2", "false"),
            ("GIT_CONFIG_GLOBAL", git_cfg.as_str()),
            ("NIGHTPORTER_JOB", "1"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string())),
    );
    env
}

/// Claude Code settings: allow-list tools, deny pushes/network/forge CLIs and protected paths.
pub fn claude_settings(s: &RepoSettings, wt: &Path, job_dir: &Path, read_only: bool) -> serde_json::Value {
    let mut allow: Vec<String> = ["Read", "Glob", "Grep", "LS", "TodoWrite"].map(String::from).to_vec();
    let git = GIT_ALLOWED.iter().copied().filter(|c| !read_only || !WRITE_GIT.contains(c));
    allow.extend(git.chain(s.allow_commands.iter().map(String::as_str)).map(|c| format!("Bash({c}:*)")));
    let mut deny: Vec<String> = DENIED.iter().map(|c| format!("Bash({c}:*)")).collect();
    if read_only {
        deny.extend(["Edit", "Write", "MultiEdit", "NotebookEdit"].map(String::from));
    } else {
        allow.extend(["Edit", "Write", "MultiEdit", "NotebookEdit"].map(String::from));
    }
    deny.extend(["WebFetch", "WebSearch"].map(String::from));
    // Credentials and Night Porter's own state on disk. With the sandbox on, these also bind
    // the agent's shell commands.
    for p in SECRET_PATHS.iter().map(|p| p.to_string()).chain(own_paths()) {
        deny.push(format!("Read({p})"));
    }
    for g in BUILTIN_PROTECTED.iter().copied().chain(s.checks.protected.iter().map(String::as_str)) {
        deny.push(format!("Edit({g})"));
        deny.push(format!("Write({g})"));
    }
    // The file tools only read inside the worktree; your own Claude hooks don't run for Night
    // Porter's unattended jobs.
    let mut v = json!({
        "permissions": { "allow": allow, "deny": deny, "blockReadsOutsideWorkingDirectories": true },
        "enableAllProjectMcpServers": false,
        "disableAllHooks": true
    });
    if s.sandbox {
        // Fail closed: no sandbox → Claude Code exits and the job fails visibly. Read denies
        // above are merged into the sandbox, so they also bind the agent's shell commands.
        let mut sb = json!({ "enabled": true, "failIfUnavailable": true, "allowUnsandboxedCommands": false });
        if !s.sandbox_domains.is_empty() {
            sb["network"] = json!({ "allowedDomains": s.sandbox_domains });
        }
        // Commands can't read your home folder, apart from the worktree, its git dir, the
        // toolchain folders and what you allow. A narrower deny still wins inside an allow.
        let mut allow_read: Vec<String> = vec![wt.to_string_lossy().into()];
        allow_read.extend(common_git_dir(wt).map(|d| d.to_string_lossy().to_string()));
        allow_read.push(job_dir.join("git").to_string_lossy().into());
        allow_read.extend(TOOLCHAIN_READS.iter().map(|p| p.to_string()));
        allow_read.extend(s.sandbox_read_allow.iter().cloned());
        let deny_read: Vec<String> = std::iter::once("~/".to_string()).chain(TOOLCHAIN_SECRETS.iter().map(|p| p.to_string())).collect();
        sb["filesystem"] = json!({ "denyRead": deny_read, "allowRead": allow_read });
        v["sandbox"] = sb;
    }
    v
}

/// OpenCode config (passed via `OPENCODE_CONFIG`).
pub fn opencode_config(s: &RepoSettings, read_only: bool) -> serde_json::Value {
    let mut bash = serde_json::Map::new();
    bash.insert("*".into(), json!("deny"));
    let git = GIT_ALLOWED.iter().copied().filter(|c| !read_only || !WRITE_GIT.contains(c));
    for c in git.chain(s.allow_commands.iter().map(String::as_str)) {
        bash.insert(format!("{c}*"), json!("allow"));
    }
    for c in DENIED {
        bash.insert(format!("{c}*"), json!("deny"));
    }
    json!({
        "$schema": "https://opencode.ai/config.json",
        "permission": {
            "edit": if read_only { "deny" } else { "allow" },
            "webfetch": "deny",
            "external_directory": "deny",
            "bash": bash
        },
        "mcp": {},
        "share": "disabled",
        "autoupdate": false
    })
}

pub async fn run(kind: AgentKind, s: &RepoSettings, wt: &Path, job_dir: &Path, prompt: &str, read_only: bool) -> Result<Outcome> {
    std::fs::create_dir_all(job_dir)?;
    std::fs::write(job_dir.join("prompt.md"), prompt)?;
    write_agent_git_config(job_dir, wt);
    let bin = std::env::var("NIGHTPORTER_AGENT_BIN").unwrap_or_else(|_| kind.bin().into());
    let mut cmd = Command::new(&bin);
    cmd.current_dir(wt).env_clear().envs(agent_env(job_dir)).kill_on_drop(true).stdin(std::process::Stdio::null());
    // Own process group, killed when the run ends: nothing the agent backgrounds can keep
    // writing to the worktree after we've checked it.
    #[cfg(unix)]
    cmd.process_group(0);
    match kind {
        AgentKind::Claude => {
            let settings = job_dir.join("claude-settings.json");
            std::fs::write(&settings, serde_json::to_vec_pretty(&claude_settings(s, wt, job_dir, read_only))?)?;
            // `--setting-sources user`: ignore the repo's own .claude/settings*.json (hooks,
            // permission allows) which an untrusted branch could plant.
            // The prompt goes in on stdin: not visible in `ps`, and no argv size limit.
            cmd.stdin(std::process::Stdio::piped());
            cmd.args([
                "-p",
                "Follow the instructions given on standard input.",
                "--output-format",
                "json",
                "--permission-mode",
                "dontAsk",
                "--strict-mcp-config",
                "--setting-sources",
                "user",
                "--settings",
            ])
            .arg(&settings);
            if let Some(b) = s.max_budget_usd {
                cmd.args(["--max-budget-usd", &b.to_string()]);
            }
        }
        AgentKind::OpenCode => {
            let cfg = job_dir.join("opencode.json");
            std::fs::write(&cfg, serde_json::to_vec_pretty(&opencode_config(s, read_only))?)?;
            cmd.env("OPENCODE_CONFIG", &cfg).args(["run", "--pure", prompt]);
        }
    }
    cmd.args(&s.agent_args);
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("start `{bin}` (is it installed?)"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let p = prompt.to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(p.as_bytes()).await;
        });
    }
    let pid = child.id();
    let res = tokio::time::timeout(s.timeout, child.wait_with_output()).await;
    kill_group(pid);
    let out = match res {
        Ok(r) => r?,
        Err(_) => bail!("agent timed out after {}", crate::config::format_duration(s.timeout)),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    std::fs::write(job_dir.join("agent.out"), format!("{stdout}\n--- stderr ---\n{stderr}"))?;
    if !out.status.success() {
        bail!("agent exited with {}: {}", out.status, tail(&stderr, 400).trim());
    }
    parse_output(kind, &stdout)
}

/// Kill whatever is left in the agent's process group.
fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|p| libc::pid_t::try_from(p).ok()) {
        // SAFETY: plain syscall; the group was created for this child by `process_group(0)`.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = pid;
}

pub fn parse_output(kind: AgentKind, stdout: &str) -> Result<Outcome> {
    let (text, cost) = match kind {
        AgentKind::Claude => {
            #[derive(Deserialize)]
            struct R {
                #[serde(default)]
                result: String,
                #[serde(default)]
                is_error: bool,
                #[serde(default)]
                total_cost_usd: Option<f64>,
            }
            let r: R = serde_json::from_str(stdout.trim()).context("claude output was not JSON")?;
            if r.is_error {
                bail!("agent reported an error: {}", tail(&r.result, 400));
            }
            (r.result, r.total_cost_usd)
        }
        AgentKind::OpenCode => (stdout.to_string(), None),
    };
    #[derive(Deserialize)]
    struct Block {
        #[serde(default)]
        summary: String,
        #[serde(default)]
        replies: Vec<Reply>,
        #[serde(default)]
        verdict: Option<String>,
    }
    let block = last_block(&text).context("agent did not end with a ```nightporter block")?;
    let b: Block = serde_json::from_str(block).context("bad JSON in the nightporter block")?;
    Ok(Outcome { summary: b.summary, verdict: b.verdict, replies: b.replies, cost_usd: cost })
}

fn last_block(text: &str) -> Option<&str> {
    let start = text.rfind("```nightporter")?;
    let body = &text[start + "```nightporter".len()..];
    let end = body.find("```")?;
    Some(body[..end].trim())
}

fn tail(s: &str, n: usize) -> &str {
    let i = s.len().saturating_sub(n);
    let i = (i..s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len());
    &s[i..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_is_scrubbed() {
        // SAFETY: tests in this module don't read these vars concurrently.
        unsafe {
            std::env::set_var("GH_TOKEN", "x");
            std::env::set_var("MY_SERVICE_TOKEN", "x");
            std::env::set_var("ANTHROPIC_API_KEY", "keep");
        }
        let d = tempfile::tempdir().unwrap();
        let env = agent_env(d.path());
        let has = |k: &str| env.iter().any(|(n, _)| n == k);
        assert!(!has("GH_TOKEN") && !has("MY_SERVICE_TOKEN") && !has("SSH_AUTH_SOCK"));
        assert!(has("ANTHROPIC_API_KEY") && has("GH_CONFIG_DIR") && has("PATH"));
        assert!(env.iter().any(|(k, v)| k == "GIT_SSH_COMMAND" && v == "/usr/bin/false"));
    }

    #[test]
    fn claude_settings_deny_push_and_protect() {
        let s = crate::config::Config::parse("").unwrap().resolve(None).unwrap();
        let v = claude_settings(&s, Path::new("/w"), Path::new("/j"), false).to_string();
        assert!(v.contains("Bash(git push:*)") && v.contains("Edit(.github/workflows/**)") && v.contains("WebFetch"));
        assert!(v.contains("Bash(git commit:*)") && v.contains("Read(~/.ssh/**)"));
        assert!(!v.contains("Bash(git log:*)"), "git log accepts --output");
        let sb = &claude_settings(&s, Path::new("/w"), Path::new("/j"), false)["sandbox"];
        assert_eq!(
            (sb["enabled"].as_bool(), sb["failIfUnavailable"].as_bool(), sb["allowUnsandboxedCommands"].as_bool()),
            (Some(true), Some(true), Some(false))
        );
        let off = crate::config::Config::parse("[defaults]\nsandbox = false").unwrap().resolve(None).unwrap();
        assert!(claude_settings(&off, Path::new("/w"), Path::new("/j"), false).get("sandbox").is_none());
        let fs = &claude_settings(&s, Path::new("/w"), Path::new("/j"), false)["sandbox"]["filesystem"];
        assert_eq!(fs["denyRead"][0], "~/", "home folder closed to commands by default");
        assert!(
            fs["allowRead"].to_string().contains("\"/w\"")
                && fs["allowRead"].to_string().contains("~/.cargo")
                && fs["allowRead"].to_string().contains("/j/git")
        );
        assert!(fs["denyRead"].to_string().contains("~/.cargo/credentials.toml"), "secrets inside allowed toolchains stay denied");
        let v = claude_settings(&s, Path::new("/w"), Path::new("/j"), false);
        assert_eq!(
            (v["disableAllHooks"].as_bool(), v["permissions"]["blockReadsOutsideWorkingDirectories"].as_bool()),
            (Some(true), Some(true))
        );
        let ro = claude_settings(&s, Path::new("/w"), Path::new("/j"), true);
        let allow = ro["permissions"]["allow"].to_string();
        assert!(!allow.contains("git commit") && !allow.contains("\"Edit\""));
    }

    #[test]
    fn parses_last_block() {
        let out = r#"{"result":"did it\n```nightporter\n{\"summary\":\"old\"}\n```\nmore\n```nightporter\n{\"summary\":\"ok\",\"replies\":[{\"event\":\"gh-rc-1\",\"body\":\"Fixed in abc\"}]}\n```","total_cost_usd":0.12}"#;
        let o = parse_output(AgentKind::Claude, out).unwrap();
        assert_eq!(o.summary, "ok");
        assert_eq!(o.replies[0].event.as_deref(), Some("gh-rc-1"));
        assert_eq!(o.cost_usd, Some(0.12));
        assert!(parse_output(AgentKind::OpenCode, "no block").is_err());
        assert!(parse_output(AgentKind::Claude, r#"{"result":"x","is_error":true}"#).is_err());
    }
}
