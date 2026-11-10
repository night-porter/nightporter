//! Running the coding agent: no forge credentials, restricted tools, bounded time.

use crate::checks::protected_globs;
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

/// Git commands the agent may run. `git log/diff/show/blame/grep` and `git stash show` are
/// deliberately absent: they accept `--output=<file>`, which would let the agent write anywhere
/// (e.g. a git config into the clone). It can read files instead.
const GIT_ALLOWED: &[&str] = &[
    "git status",
    "git add",
    "git commit",
    "git rev-parse",
    "git ls-files",
    "git merge",
    "git checkout --",
    "git restore",
    "git stash push",
    "git stash pop",
];
/// Git commands that change the worktree; dropped for read-only steps.
const WRITE_GIT: &[&str] = &["git add", "git commit", "git merge", "git checkout --", "git restore", "git stash push", "git stash pop"];
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
    "tea",
    "fj",
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
    "~/.config/tea/**",
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
    "GOOGLE_APPLICATION_CREDENTIALS",
];
const MODEL_PREFIXES: &[&str] = &["ANTHROPIC_", "CLAUDE_", "OPENCODE_", "CODEX_"];

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
            let forge =
                ["GH_", "GITHUB_", "GITLAB_", "GLAB_", "GITEA_", "FORGEJO_", "CI_JOB_TOKEN", "NIGHTPORTER_", "SSH_AUTH_SOCK", "GIT_"]
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
    // `allow_commands = ["*"]`: any command. The deny list below still wins, and with the
    // sandbox on every command stays inside it (worktree writes, no credentials, listed domains).
    allow.extend(
        git.chain(s.allow_commands.iter().map(String::as_str)).map(|c| if c == "*" { "Bash".into() } else { format!("Bash({c}:*)") }),
    );
    let mut deny: Vec<String> = DENIED.iter().map(|c| format!("Bash({c}:*)")).collect();
    if read_only {
        deny.extend(["Edit", "Write", "MultiEdit", "NotebookEdit"].map(String::from));
    } else {
        allow.extend(["Edit", "Write", "MultiEdit", "NotebookEdit"].map(String::from));
    }
    deny.extend(["WebFetch", "WebSearch"].map(String::from));
    // Credentials and Night Porter's own state on disk. With the sandbox on, these also bind
    // the agent's shell commands.
    for p in SECRET_PATHS.iter().map(std::string::ToString::to_string).chain(own_paths()) {
        deny.push(format!("Read({p})"));
    }
    for g in protected_globs(&s.checks) {
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
        allow_read.extend(TOOLCHAIN_READS.iter().map(std::string::ToString::to_string));
        allow_read.extend(s.sandbox_read_allow.iter().cloned());
        let deny_read: Vec<String> =
            std::iter::once("~/".to_string()).chain(TOOLCHAIN_SECRETS.iter().map(std::string::ToString::to_string)).collect();
        sb["filesystem"] = json!({ "denyRead": deny_read, "allowRead": allow_read });
        v["sandbox"] = sb;
    }
    v
}

/// `OpenCode` config (passed via `OPENCODE_CONFIG`).
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
    // The agent can write to the clone's git dir (it commits); its config is read by our own git
    // calls outside the sandbox. Any change is undone and fails the job.
    let guard = crate::git::ConfigGuard::take(wt);
    let r = run_unguarded(kind, s, wt, job_dir, prompt, read_only).await;
    guard.verify()?;
    r
}

async fn run_unguarded(kind: AgentKind, s: &RepoSettings, wt: &Path, job_dir: &Path, prompt: &str, read_only: bool) -> Result<Outcome> {
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
        AgentKind::Codex => {
            cmd.stdin(std::process::Stdio::piped()).args(codex_args(s, job_dir, read_only));
        }
        AgentKind::Gemini => {
            // Fail closed: no run on a version whose sandbox we haven't checked, or on a branch
            // carrying Gemini config we can't keep out.
            let v = Command::new(&bin).arg("--version").output().await.with_context(|| format!("start `{bin}` (is it installed?)"))?;
            let v = String::from_utf8_lossy(&v.stdout);
            if !v.lines().any(gemini_version_ok) {
                bail!("Gemini CLI {} is too old: Night Porter needs {}.{} or newer for its sandbox", v.trim(), GEMINI_MIN.0, GEMINI_MIN.1);
            }
            if let Some(f) = GEMINI_REPO_CONFIG.iter().find(|f| wt.join(f).exists()) {
                bail!("the branch has {f}, which Gemini CLI would load as its own configuration; use another agent for this repo");
            }
            if let Some(d) = GEMINI_SYSTEM_POLICY_DIRS.iter().map(Path::new).find(|d| has_toml(d)) {
                bail!("{} holds Gemini CLI policies, which make it ignore Night Porter's policy; use another agent", d.display());
            }
            let settings = job_dir.join("gemini-settings.json");
            std::fs::write(&settings, serde_json::to_vec_pretty(&gemini_settings(s, job_dir))?)?;
            let policy = job_dir.join("gemini-policy.toml");
            std::fs::write(&policy, gemini_policy(s, read_only))?;
            cmd.env("GEMINI_CLI_SYSTEM_SETTINGS_PATH", &settings)
                .env("GEMINI_SANDBOX", "false")
                .env_remove("SANDBOX")
                .stdin(std::process::Stdio::piped())
                .args(gemini_args(&policy));
        }
    }
    cmd.args(s.agent_args.for_agent(kind));
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
    if kind == AgentKind::Codex {
        // The final message is in the file `-o` wrote; stdout is the progress stream.
        let last = std::fs::read_to_string(job_dir.join(CODEX_LAST)).unwrap_or_default();
        return parse_output(kind, &last);
    }
    parse_output(kind, &stdout)
}

const CODEX_LAST: &str = "codex-last-message.md";

/// `codex exec` arguments. Codex has no per-command allow list: its OS sandbox (Seatbelt /
/// Landlock) is the boundary, with writes limited to the worktree and the network off. Your
/// `~/.codex/config.toml` (MCP servers, hooks, profiles) and execpolicy rules are not loaded;
/// login still comes from `CODEX_HOME`. The prompt is read from stdin (`-`).
fn codex_args(s: &RepoSettings, job_dir: &Path, read_only: bool) -> Vec<String> {
    let sandbox = if read_only {
        "read-only"
    } else if s.sandbox {
        "workspace-write"
    } else {
        "danger-full-access"
    };
    let mut a: Vec<String> =
        ["exec", "--sandbox", sandbox, "--ignore-user-config", "--ignore-rules", "--ephemeral", "--skip-git-repo-check"]
            .map(String::from)
            .to_vec();
    for c in ["approval_policy=\"never\"", "sandbox_workspace_write.network_access=false", "tools.web_search=false"] {
        a.extend(["-c".into(), c.into()]);
    }
    a.extend(["-o".into(), job_dir.join(CODEX_LAST).to_string_lossy().into(), "-".into()]);
    a
}

/// Oldest Gemini CLI whose tool sandbox and policy engine we rely on. Older versions ignore
/// `security.toolSandboxing` without a word, so they're refused rather than run unsandboxed.
const GEMINI_MIN: (u32, u32) = (0, 67);

/// `gemini --version` prints e.g. `0.67.0` or `0.69.0-nightly.20261104.g89e9d25d6`.
fn gemini_version_ok(out: &str) -> bool {
    let mut v = out.trim().split(['.', '-']).map(str::parse::<u32>);
    matches!((v.next(), v.next()), (Some(Ok(major)), Some(Ok(minor))) if (major, minor) >= GEMINI_MIN)
}

/// Repo files Gemini CLI would load as configuration in a trusted workspace (headless runs must
/// trust it). Their settings merge into ours (`context.includeDirectories` widens the sandbox) and
/// `.gemini/.env` is read even with `ignoreLocalEnv`, so a branch carrying them isn't run.
const GEMINI_REPO_CONFIG: &[&str] = &[".gemini/settings.json", ".gemini/.env"];

/// System policy folders: any `.toml` in them makes Gemini CLI drop `--admin-policy` (its own
/// defaults then still allow `google_web_search`, a way out for data), so the run is refused.
const GEMINI_SYSTEM_POLICY_DIRS: &[&str] = &["/etc/gemini-cli/policies", "/Library/Application Support/GeminiCli/policies"];

fn has_toml(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|it| it.flatten().any(|e| e.path().extension().is_some_and(|x| x == "toml")))
}

/// Gemini CLI system settings (via `GEMINI_CLI_SYSTEM_SETTINGS_PATH`, which overrides user and
/// workspace settings): shell commands and file tools run in its tool sandbox (Seatbelt /
/// bubblewrap: reads limited to the worktree, its git dir and PATH folders, writes to the
/// worktree, no network), no hooks, no repo `.env`, no auto-update.
pub fn gemini_settings(s: &RepoSettings, job_dir: &Path) -> serde_json::Value {
    json!({
        // Redaction off: Night Porter already scrubbed the environment, and Gemini's redaction
        // would drop the `GIT_CONFIG_KEY_*` overrides that keep git hooks and helpers off.
        "security": { "toolSandboxing": s.sandbox, "environmentVariableRedaction": { "enabled": false } },
        "tools": { "sandbox": false, "sandboxNetworkAccess": false, "sandboxAllowedPaths": [] },
        // Sandboxed git reads the agent's git config (name and email only).
        "context": { "includeDirectories": [job_dir.join("git")] },
        "hooksConfig": { "enabled": false },
        "advanced": { "ignoreLocalEnv": true },
        "general": { "enableAutoUpdate": false, "enableAutoUpdateNotification": false }
    })
}

/// Gemini CLI admin policy (`--admin-policy`; admin rules beat user ones). Shell commands are
/// denied unless allow-listed, deny-listed ones always, and so is any call that asks to widen
/// the sandbox (`additional_permissions`: network or extra paths). No web tools, no memory file.
pub fn gemini_policy(s: &RepoSettings, read_only: bool) -> String {
    let git = GIT_ALLOWED.iter().copied().filter(|c| !read_only || !WRITE_GIT.contains(c));
    let allowed: Vec<&str> = git.chain(s.allow_commands.iter().map(String::as_str)).collect();
    let rule = |tool: serde_json::Value, extra: serde_json::Value, decision: &str, priority: u32| {
        let mut r = json!({ "toolName": tool, "decision": decision, "priority": priority });
        r.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        r
    };
    let shell = json!("run_shell_command");
    let edits = json!(["write_file", "replace"]);
    let rules = vec![
        rule(shell.clone(), json!({}), "deny", 1),
        rule(shell.clone(), json!({ "commandPrefix": allowed }), "allow", 100),
        rule(edits, json!({}), if read_only { "deny" } else { "allow" }, if read_only { 900 } else { 100 }),
        rule(shell.clone(), json!({ "commandPrefix": DENIED }), "deny", 900),
        rule(json!(["web_fetch", "google_web_search", "save_memory"]), json!({}), "deny", 900),
        rule(shell, json!({ "argsPattern": "\"additional_permissions\"" }), "deny", 950),
    ];
    toml::to_string(&json!({ "rule": rules })).unwrap_or_default()
}

/// `gemini` arguments. The prompt arrives on stdin, which Gemini CLI puts before `-p`'s text.
/// No extensions, no MCP servers (the allow list names one that can't exist), no YOLO.
fn gemini_args(policy: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "Follow the instructions given on standard input.",
        "--output-format",
        "json",
        "--approval-mode",
        "default",
        "--skip-trust",
        "--extensions",
        "none",
        "--allowed-mcp-server-names",
        "nightporter-none",
        "--admin-policy",
    ]
    .map(String::from)
    .to_vec();
    a.push(policy.to_string_lossy().into());
    a
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
    #[derive(Deserialize)]
    struct Block {
        #[serde(default)]
        summary: String,
        #[serde(default)]
        replies: Vec<Reply>,
        #[serde(default)]
        verdict: Option<String>,
    }
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
        AgentKind::Gemini => {
            #[derive(Deserialize)]
            struct R {
                #[serde(default)]
                response: String,
                #[serde(default)]
                error: Option<serde_json::Value>,
            }
            let r: R = serde_json::from_str(stdout.trim()).context("gemini output was not JSON")?;
            if let Some(e) = r.error {
                bail!("agent reported an error: {}", tail(&e.to_string(), 400));
            }
            (r.response, None)
        }
        AgentKind::OpenCode | AgentKind::Codex => (stdout.to_string(), None),
    };
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
    fn codex_runs_sandboxed_without_user_config() {
        let s = crate::config::Config::parse("").unwrap().resolve(None).unwrap();
        let a = codex_args(&s, Path::new("/j"), false).join(" ");
        assert!(a.starts_with("exec --sandbox workspace-write --ignore-user-config"), "{a}");
        assert!(a.contains("sandbox_workspace_write.network_access=false") && a.ends_with("-o /j/codex-last-message.md -"), "{a}");
        assert!(codex_args(&s, Path::new("/j"), true).join(" ").contains("--sandbox read-only"));
        let off = crate::config::Config::parse("[defaults]\nsandbox = false").unwrap().resolve(None).unwrap();
        assert!(codex_args(&off, Path::new("/j"), false).join(" ").contains("danger-full-access"));
        let out = "done\n```nightporter\n{\"summary\":\"ok\",\"replies\":[]}\n```";
        assert_eq!(parse_output(AgentKind::Codex, out).unwrap().summary, "ok");
        assert_eq!(AgentKind::Claude.next().next(), AgentKind::Codex);
        assert_eq!(AgentKind::Codex.next(), AgentKind::Gemini);
        assert_eq!(AgentKind::Gemini.next(), AgentKind::Claude);
    }

    #[test]
    fn gemini_runs_sandboxed_with_admin_policy() {
        let s = crate::config::Config::parse("[defaults]\nallow_commands = [\"npm test\"]").unwrap().resolve(None).unwrap();
        let v = gemini_settings(&s, Path::new("/j"));
        assert_eq!((v["security"]["toolSandboxing"].as_bool(), v["tools"]["sandboxNetworkAccess"].as_bool()), (Some(true), Some(false)));
        assert_eq!((v["hooksConfig"]["enabled"].as_bool(), v["advanced"]["ignoreLocalEnv"].as_bool()), (Some(false), Some(true)));
        assert_eq!(v["context"]["includeDirectories"][0], "/j/git");
        let off = crate::config::Config::parse("[defaults]\nsandbox = false").unwrap().resolve(None).unwrap();
        assert_eq!(gemini_settings(&off, Path::new("/j"))["security"]["toolSandboxing"].as_bool(), Some(false));

        let p: toml::Table = gemini_policy(&s, false).parse().unwrap();
        let rules = p["rule"].as_array().unwrap();
        let find = |pred: &dyn Fn(&toml::Table) -> bool| rules.iter().filter_map(toml::Value::as_table).find(|r| pred(r)).cloned().unwrap();
        let allow = find(&|r| r["decision"].as_str() == Some("allow") && r.contains_key("commandPrefix"));
        let prefixes = allow["commandPrefix"].to_string();
        assert!(prefixes.contains("git commit") && prefixes.contains("npm test") && !prefixes.contains("git push"), "{prefixes}");
        let deny = find(&|r| r["decision"].as_str() == Some("deny") && r.contains_key("commandPrefix"));
        assert!(deny["commandPrefix"].to_string().contains("git push") && deny["priority"].as_integer() > allow["priority"].as_integer());
        let widen = find(&|r| r.contains_key("argsPattern"));
        assert_eq!((widen["decision"].as_str(), widen["priority"].as_integer()), (Some("deny"), Some(950)));
        assert!(gemini_policy(&s, false).contains("google_web_search"));
        let ro: toml::Table = gemini_policy(&s, true).parse().unwrap();
        let ro = ro["rule"].to_string();
        assert!(!ro.contains("\"git commit\"") && ro.contains("write_file"), "{ro}");

        let a = gemini_args(Path::new("/j/gemini-policy.toml")).join(" ");
        assert!(
            a.contains("--approval-mode default") && a.contains("--extensions none") && a.ends_with("--admin-policy /j/gemini-policy.toml"),
            "{a}"
        );
        assert!(!a.contains("yolo"));

        assert!(gemini_version_ok("0.67.0\n") && gemini_version_ok("0.69.0-nightly.20261104.g89e9d25d6") && gemini_version_ok("1.0.0"));
        assert!(!gemini_version_ok("0.66.2") && !gemini_version_ok("") && !gemini_version_ok("gemini"));
        let tmp = tempfile::tempdir().unwrap();
        assert!(!has_toml(tmp.path()) && !has_toml(&tmp.path().join("missing")));
        std::fs::write(tmp.path().join("org.toml"), "").unwrap();
        assert!(has_toml(tmp.path()));

        let out = r#"{"session_id":"x","response":"done\n```nightporter\n{\"summary\":\"ok\",\"replies\":[]}\n```","stats":{}}"#;
        assert_eq!(parse_output(AgentKind::Gemini, out).unwrap().summary, "ok");
        assert!(parse_output(AgentKind::Gemini, r#"{"error":{"type":"FatalToolExecutionError","message":"no"}}"#).is_err());
        assert!(parse_output(AgentKind::Gemini, "not json").is_err());
    }

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
        let any = crate::config::Config::parse("[defaults]\nallow_commands = [\"*\"]").unwrap().resolve(None).unwrap();
        let any = claude_settings(&any, Path::new("/w"), Path::new("/j"), false)["permissions"].clone();
        assert!(any["allow"].as_array().unwrap().iter().any(|a| a == "Bash"), "any command");
        assert!(any["deny"].to_string().contains("Bash(git push:*)"), "the deny list still applies");
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
