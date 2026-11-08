//! Config: one global `config.toml` plus optional per-repo override files.
//!
//! Layering is done on raw TOML tables (`[defaults]` ⊕ repo file), then deserialised once into
//! [`RepoSettings`], so every setting is automatically overridable per repo.

use crate::forge::{ForgeKind, RepoId, Trigger};
use crate::paths::{Paths, expand_tilde};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default = "default_code_dir")]
    pub code_dir: String,
    #[serde(default = "three")]
    pub scan_depth: usize,
    #[serde(default = "two_min", with = "dur")]
    pub poll_interval: Duration,
    #[serde(default = "ten_min", with = "dur")]
    pub sweep_interval: Duration,
    #[serde(default = "five")]
    pub min_free_gb: u64,
    #[serde(default = "two_usize")]
    pub max_concurrent_jobs: usize,
    #[serde(default = "fifty")]
    pub daily_job_cap: u32,
    #[serde(default = "thirty")]
    pub retention_days: u32,
    #[serde(default = "fourteen")]
    pub worktree_idle_days: u32,
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub notifications: Notifications,
    #[serde(default)]
    pub marketplaces: Vec<String>,
    /// Once a day, ask GitHub for the latest release number (anonymous; see `update.rs`).
    #[serde(default = "yes")]
    pub check_updates: bool,
    /// Take `/nightporter approve|dismiss|retry|status` from your own comments on PRs/MRs.
    #[serde(default = "yes")]
    pub pr_commands: bool,
    /// Where `h` / `nightporter handoff` opens a job: `auto`, `herdr`, `rex`, `tmux`, `zellij`, `terminal` or `print`.
    #[serde(default)]
    pub handoff: Handoff,
    /// Raw `[defaults]` table; resolved per repo by [`Config::settings_for`].
    #[serde(default)]
    pub defaults: toml::Table,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub forge: ForgeKind,
    #[serde(default)]
    pub host: Option<String>,
    /// `cli` (gh/glab/tea) or `keyring`.
    #[serde(default)]
    pub token: TokenSource,
}

impl Account {
    pub fn host(&self) -> &str {
        self.host.as_deref().unwrap_or(self.forge.default_host())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Handoff {
    /// The multiplexer you're in, else herdr or Rex if installed, else a new terminal window,
    /// else print the command.
    #[default]
    Auto,
    Herdr,
    Rex,
    Tmux,
    Zellij,
    /// A new terminal window (iTerm2 or Terminal on macOS; `$TERMINAL` or a common one on Linux).
    Terminal,
    /// Print the command to run yourself.
    Print,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TokenSource {
    #[default]
    Cli,
    Keyring,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors the [notifications] TOML table")]
pub struct Notifications {
    #[serde(default = "yes")]
    pub needs_approval: bool,
    #[serde(default = "yes")]
    pub failed: bool,
    #[serde(default = "yes")]
    pub done: bool,
    #[serde(default = "yes")]
    pub new_author: bool,
    /// Webhooks and commands (`[[notifications.targets]]`); the toggles above are for the
    /// desktop popup only.
    #[serde(default)]
    pub targets: Vec<NotifyTarget>,
}

impl Default for Notifications {
    fn default() -> Self {
        Self { needs_approval: true, failed: true, done: true, new_author: true, targets: Vec::new() }
    }
}

/// One `[[notifications.targets]]` entry: a webhook or a command.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyTarget {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: TargetType,
    /// Webhook URL. Never logged or shown: it often holds a secret.
    #[serde(default, skip_serializing)]
    pub url: Option<String>,
    /// Read the webhook URL from this environment variable instead (of the daemon's process).
    #[serde(default)]
    pub url_env: Option<String>,
    #[serde(default)]
    pub format: WebhookFormat,
    /// argv for `type = "command"`; run without a shell, the event as JSON on stdin.
    #[serde(default)]
    pub command: Vec<String>,
    /// Only these kinds; empty = all.
    #[serde(default)]
    pub kinds: Vec<crate::notify::Kind>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TargetType {
    Webhook,
    Command,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebhookFormat {
    #[default]
    Json,
    Slack,
    Discord,
    Ntfy,
}

impl Notifications {
    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for t in &self.targets {
            let n = &t.name;
            if n.is_empty() || n == "desktop" || !seen.insert(n) {
                bail!("notification target name {n:?} is empty, reserved (desktop) or used twice");
            }
            match t.kind {
                TargetType::Webhook => match (&t.url, &t.url_env) {
                    (Some(u), None) if u.starts_with("https://") || u.starts_with("http://") => {}
                    (None, Some(_)) => {}
                    (Some(_), None) => bail!("notification target {n:?}: url must start with https:// or http://"),
                    _ => bail!("notification target {n:?}: a webhook needs exactly one of url or url_env"),
                },
                TargetType::Command if t.command.is_empty() => bail!("notification target {n:?}: command is empty"),
                TargetType::Command => {}
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Observe,
    Suggest,
    Draft,
    Auto,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Observe => "observe",
            Level::Suggest => "suggest",
            Level::Draft => "draft",
            Level::Auto => "auto",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    OpenCode,
    Codex,
    Gemini,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::OpenCode => "OpenCode",
            AgentKind::Codex => "Codex",
            AgentKind::Gemini => "Gemini CLI (experimental)",
        }
    }
    /// Binary name, which is also the config value.
    pub fn bin(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::OpenCode => "opencode",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
        }
    }
    pub const ALL: [AgentKind; 4] = [AgentKind::Claude, AgentKind::OpenCode, AgentKind::Codex, AgentKind::Gemini];
    /// The next agent in [`Self::ALL`] (the TUI's `a` key cycles through them).
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|a| *a == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Escalation {
    Deps,
    Size,
}

/// Effective settings for one repo (defaults with the repo file layered on top).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoSettings {
    #[serde(default)]
    pub subscribed: bool,
    #[serde(default = "claude")]
    pub agent: AgentKind,
    /// Extra args for the agent CLI, e.g. `["--model", "opus"]`.
    #[serde(default)]
    pub agent_args: Vec<String>,
    /// Shell commands the agent may run (e.g. `"npm test"`, `"ddev phpunit"`). Prefix match.
    #[serde(default)]
    pub allow_commands: Vec<String>,
    #[serde(default = "draft")]
    pub level: Level,
    #[serde(default = "all_triggers")]
    pub triggers: Vec<Trigger>,
    #[serde(default = "two_min", with = "dur")]
    pub debounce: Duration,
    #[serde(default)]
    pub max_concurrent: Option<usize>,
    #[serde(default = "three_u32")]
    pub max_ci_attempts: u32,
    /// Let CI-failure jobs run at `auto`. CI output is text from builds and tools that may print
    /// anything (including a stranger's test names or logs), so by default those jobs stop at
    /// `draft` and wait for you.
    #[serde(default)]
    pub ci_auto: bool,
    #[serde(default = "thirty_min", with = "dur")]
    pub timeout: Duration,
    #[serde(default)]
    pub max_budget_usd: Option<f64>,
    /// Claude Code's OS sandbox for the agent's shell commands (Seatbelt on macOS, bubblewrap on
    /// Linux). Fails closed: if it can't start, the job fails instead of running unsandboxed.
    /// Turn off for repos whose test commands need Docker/DDEV sockets.
    #[serde(default = "yes")]
    pub sandbox: bool,
    /// Domains sandboxed commands may reach (none by default).
    #[serde(default)]
    pub sandbox_domains: Vec<String>,
    /// Extra paths under your home folder that sandboxed commands may read (on top of the
    /// worktree and common toolchain folders). `["~/"]` lifts the home-folder restriction.
    #[serde(default)]
    pub sandbox_read_allow: Vec<String>,
    /// Extra instructions file (your own, outside the repo).
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub packs: Vec<String>,
    #[serde(default)]
    pub trust: Trust,
    #[serde(default)]
    pub schedule: Vec<Window>,
    /// Where notifications go: `desktop` and/or target names. `None` = everywhere.
    #[serde(default)]
    pub notify: Option<Vec<String>>,
    #[serde(default)]
    pub reply: ReplySettings,
    #[serde(default)]
    pub checks: Checks,
    #[serde(default)]
    pub env: Env,
    #[serde(default)]
    pub maintainer: Maintainer,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Trust {
    #[serde(default = "default_roles")]
    pub roles: Vec<crate::forge::Role>,
    #[serde(default)]
    pub allow_users: Vec<String>,
    #[serde(default)]
    pub deny_users: Vec<String>,
    #[serde(default)]
    pub allow_teams: Vec<String>,
    #[serde(default)]
    pub deny_teams: Vec<String>,
    #[serde(default)]
    pub allow_bots: Vec<String>,
    /// Accept everyone. Capped at `suggest`.
    #[serde(default)]
    pub open: bool,
}

impl Default for Trust {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    /// `mon-fri`, `sat,sun`, `daily`, `tue`.
    pub days: String,
    /// `HH:MM-HH:MM`; may cross midnight (`22:00-06:00`).
    pub windows: Vec<String>,
    /// IANA time zone; defaults to the system zone.
    #[serde(default)]
    pub tz: Option<String>,
    /// While this window is the first active one: its level, agent and notify list replace the
    /// repo's. The safety caps still apply on top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<Vec<String>>,
}

impl Window {
    /// Sets more than days and times (the TUI's one-line schedule field can't keep these).
    pub fn is_plain(&self) -> bool {
        self.tz.is_none() && self.level.is_none() && self.agent.is_none() && self.notify.is_none()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplySettings {
    /// `concise`, `detailed-tech-lead`, `non-technical`, a pack style, or a path to a `.md` file.
    #[serde(default = "concise")]
    pub style: String,
    #[serde(default = "en")]
    pub language: String,
    #[serde(default = "yes")]
    pub footer: bool,
    #[serde(default)]
    pub resolve_threads: bool,
}

impl Default for ReplySettings {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Checks {
    /// Extra protected globs, added to the built-in list (which can't be removed).
    #[serde(default)]
    pub protected: Vec<String>,
    #[serde(default = "default_escalations")]
    pub escalate_on: Vec<Escalation>,
    #[serde(default = "five_hundred")]
    pub max_diff_lines: usize,
}

impl Default for Checks {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Env {
    #[serde(default)]
    pub setup: Vec<String>,
    #[serde(default)]
    pub teardown: Vec<String>,
    #[serde(default)]
    pub cleanup: Vec<String>,
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Maintainer {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "draft")]
    pub level: Level,
    #[serde(default = "np_label")]
    pub label: String,
}

impl Default for Maintainer {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        Self::load_file(&paths.config_file())
    }

    fn load_file(file: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("read {}", file.display())),
        };
        Self::parse(&text).with_context(|| format!("in {}", file.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text)?;
        if cfg.version > VERSION {
            bail!("config version {} is newer than this nightporter ({VERSION}); upgrade nightporter", cfg.version);
        }
        cfg.notifications.validate()?;
        // Validate defaults eagerly so typos surface at load, not at the first event.
        cfg.resolve(None)?;
        Ok(cfg)
    }

    pub fn code_dir(&self) -> PathBuf {
        expand_tilde(&self.code_dir)
    }

    /// Effective settings for `repo` right now, reading its override file if present.
    pub fn settings_for(&self, paths: &Paths, repo: &RepoId) -> Result<RepoSettings> {
        Ok(self.base_settings_for(paths, repo)?.at(&jiff::Zoned::now()))
    }

    /// `repo`'s settings before any schedule window applies: what its files say.
    pub fn base_settings_for(&self, paths: &Paths, repo: &RepoId) -> Result<RepoSettings> {
        let file = paths.repo_file(repo);
        let over = match std::fs::read_to_string(&file) {
            Ok(t) => Some(t.parse::<toml::Table>().with_context(|| format!("in {}", file.display()))?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("read {}", file.display())),
        };
        self.resolve(over.as_ref()).with_context(|| format!("settings for {repo}"))
    }

    pub fn resolve(&self, over: Option<&toml::Table>) -> Result<RepoSettings> {
        let mut merged = self.defaults.clone();
        // `subscribed` only makes sense per repo.
        merged.remove("subscribed");
        if let Some(o) = over {
            merge(&mut merged, o);
        }
        let s: RepoSettings = merged.try_into()?;
        crate::schedule::validate(&s.schedule)?;
        let known = |n: &String| n == "desktop" || self.notifications.targets.iter().any(|t| &t.name == n);
        if let Some(n) = s.notify.iter().chain(s.schedule.iter().filter_map(|w| w.notify.as_ref())).flatten().find(|n| !known(n)) {
            bail!("unknown notification target {n:?} in notify (use desktop or a [[notifications.targets]] name)");
        }
        Ok(s.clamped())
    }
}

impl RepoSettings {
    /// Apply the safety caps that config can't override.
    fn clamped(mut self) -> Self {
        if self.trust.open {
            self.level = self.level.min(Level::Suggest);
            self.maintainer.level = self.maintainer.level.min(Level::Suggest);
        }
        self.maintainer.level = self.maintainer.level.min(Level::Draft);
        self
    }

    /// Apply the first schedule window active at `now` (its `level`, `agent`, `notify`), then
    /// the caps again: a window can't lift `trust.open`'s cap or raise maintainer jobs, and
    /// CI-failure jobs still stop at draft unless `ci_auto`.
    pub fn at(mut self, now: &jiff::Zoned) -> Self {
        if let Ok(Some(w)) = crate::schedule::matching(&self.schedule, now) {
            let w = w.clone();
            if let Some(l) = w.level {
                self.level = l;
                self.maintainer.level = self.maintainer.level.min(l);
            }
            if let Some(a) = w.agent {
                self.agent = a;
            }
            if w.notify.is_some() {
                self.notify = w.notify;
            }
        }
        self.clamped()
    }

    pub fn concurrency(&self) -> usize {
        self.max_concurrent.unwrap_or(usize::MAX).max(1)
    }
}

/// Arrays that combine (global ∪ repo) instead of being replaced: a repo file adding one trusted
/// user must not drop the global lists, and must never be able to drop a global deny.
const UNION: &[&str] =
    &["trust.allow_users", "trust.deny_users", "trust.allow_teams", "trust.deny_teams", "trust.allow_bots", "checks.protected"];

/// Deep-merge `over` into `base`: tables merge, [`UNION`] arrays combine, everything else
/// (including other arrays) replaces.
pub fn merge(base: &mut toml::Table, over: &toml::Table) {
    merge_at(base, over, "");
}

fn merge_at(base: &mut toml::Table, over: &toml::Table, prefix: &str) {
    for (k, v) in over {
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge_at(b, o, &path),
            (Some(toml::Value::Array(b)), toml::Value::Array(o)) if UNION.contains(&path.as_str()) => {
                for x in o {
                    if !b.contains(x) {
                        b.push(x.clone());
                    }
                }
            }
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Edit a TOML file in place, keeping comments and layout. Every edit re-reads the file right
/// before writing, so edits from several TUIs (or an editor) don't overwrite each other.
/// Validates the result and writes atomically.
pub fn edit_file(path: &Path, f: impl FnOnce(&mut toml_edit::DocumentMut)) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("parse {}", path.display()))?;
    f(&mut doc);
    let out = doc.to_string();
    // Validate before writing, so a bad edit can't brick the daemon.
    validate(path, &out)?;
    write_atomic(path, out.as_bytes())
}

/// Check `text` as the new contents of `path`: `config.toml` or a repo file.
pub fn validate(path: &Path, text: &str) -> Result<()> {
    if path.file_name().is_some_and(|n| n == "config.toml") {
        Config::parse(text)?;
    } else {
        // Against the real config.toml (above repos/), so notify can name its targets.
        let global = path.ancestors().map(|d| d.join("config.toml")).find(|p| p.is_file());
        let cfg = global.and_then(|p| Config::load_file(&p).ok()).map_or_else(|| Config::parse(""), Ok)?;
        cfg.resolve(Some(&text.parse::<toml::Table>()?))?;
    }
    Ok(())
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Parse `90s`, `2m`, `1h30m`, `1d`.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (mut total, mut num) = (0u64, String::new());
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num.parse().with_context(|| format!("bad duration {s:?}"))?;
        num.clear();
        total += n * match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86400,
            _ => bail!("bad duration unit {c:?} in {s:?} (use s, m, h, d)"),
        };
    }
    if !num.is_empty() || s.is_empty() {
        bail!("bad duration {s:?} (e.g. 90s, 2m, 1h30m)");
    }
    Ok(Duration::from_secs(total))
}

pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        _ if s.is_multiple_of(3600) && s > 0 => format!("{}h", s / 3600),
        _ if s.is_multiple_of(60) && s > 0 => format!("{}m", s / 60),
        _ => format!("{s}s"),
    }
}

mod dur {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        super::parse_duration(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
    pub fn serialize<S: Serializer>(v: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::format_duration(*v))
    }
}

/// Starter config written by `nightporter setup`.
pub const TEMPLATE: &str = include_str!("../assets/config.example.toml");

fn one() -> u32 {
    1
}
fn three() -> usize {
    3
}
fn three_u32() -> u32 {
    3
}
fn five() -> u64 {
    5
}
fn two_usize() -> usize {
    2
}
fn fifty() -> u32 {
    50
}
fn thirty() -> u32 {
    30
}
fn fourteen() -> u32 {
    14
}
fn five_hundred() -> usize {
    500
}
fn yes() -> bool {
    true
}
fn two_min() -> Duration {
    Duration::from_secs(120)
}
fn ten_min() -> Duration {
    Duration::from_secs(600)
}
fn thirty_min() -> Duration {
    Duration::from_secs(1800)
}
fn default_code_dir() -> String {
    "~/coding".into()
}
fn claude() -> AgentKind {
    AgentKind::Claude
}
fn draft() -> Level {
    Level::Draft
}
fn concise() -> String {
    "concise".into()
}
fn en() -> String {
    "en".into()
}
fn np_label() -> String {
    "nightporter".into()
}
fn all_triggers() -> Vec<Trigger> {
    use Trigger::{CiFailure, Comment, MergeConflict, Review, ReviewComment};
    vec![ReviewComment, Review, Comment, CiFailure, MergeConflict]
}
fn default_roles() -> Vec<crate::forge::Role> {
    use crate::forge::Role::{Collaborator, Maintainer, Member, Owner};
    vec![Owner, Maintainer, Member, Collaborator]
}
fn default_escalations() -> Vec<Escalation> {
    vec![Escalation::Deps, Escalation::Size]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_has_sane_defaults() {
        let c = Config::parse("").unwrap();
        let s = c.resolve(None).unwrap();
        assert!(!s.subscribed);
        assert_eq!(s.level, Level::Draft);
        assert_eq!(s.debounce, Duration::from_secs(120));
        assert_eq!(s.trust.roles.len(), 4);
        assert!(s.reply.footer);
        assert!(!s.reply.resolve_threads);
        assert_eq!(c.poll_interval, Duration::from_secs(120));
    }

    #[test]
    fn template_parses() {
        Config::parse(TEMPLATE).unwrap();
    }

    #[test]
    fn repo_overrides_merge_deeply() {
        let c = Config::parse(
            r#"
            [defaults]
            level = "auto"
            [defaults.trust]
            allow_users = ["alice"]
            [defaults.reply]
            style = "detailed-tech-lead"
            "#,
        )
        .unwrap();
        let over: toml::Table = toml::from_str(
            r"
            subscribed = true
            max_ci_attempts = 5
            [reply]
            footer = false
            ",
        )
        .unwrap();
        let s = c.resolve(Some(&over)).unwrap();
        assert!(s.subscribed);
        assert_eq!(s.level, Level::Auto);
        assert_eq!(s.max_ci_attempts, 5);
        assert_eq!(s.reply.style, "detailed-tech-lead");
        assert!(!s.reply.footer);
        assert_eq!(s.trust.allow_users, vec!["alice"]);
    }

    #[test]
    fn trust_lists_combine() {
        let c = Config::parse("[defaults.trust]\nallow_users = [\"alice\"]\ndeny_users = [\"mallory\"]\nroles = [\"owner\"]").unwrap();
        let over: toml::Table = toml::from_str("[trust]\nallow_users = [\"carol\"]\ndeny_users = []\nroles = [\"maintainer\"]").unwrap();
        let s = c.resolve(Some(&over)).unwrap();
        assert_eq!(s.trust.allow_users, vec!["alice", "carol"]);
        assert_eq!(s.trust.deny_users, vec!["mallory"], "a repo can't drop a global deny");
        assert_eq!(s.trust.roles, vec![crate::forge::Role::Maintainer], "other arrays still replace");
    }

    #[test]
    fn subscribed_cannot_be_set_globally() {
        let c = Config::parse("[defaults]\nsubscribed = true").unwrap();
        assert!(!c.resolve(None).unwrap().subscribed);
    }

    #[test]
    fn caps_cannot_be_overridden() {
        let c = Config::parse("[defaults]\nlevel = \"auto\"\n[defaults.maintainer]\nlevel = \"auto\"").unwrap();
        let s = c.resolve(None).unwrap();
        assert_eq!(s.maintainer.level, Level::Draft);
        let open: toml::Table = toml::from_str("[trust]\nopen = true").unwrap();
        let s = c.resolve(Some(&open)).unwrap();
        assert_eq!(s.level, Level::Suggest);
        assert_eq!(s.maintainer.level, Level::Suggest);
    }

    #[test]
    fn first_active_window_overrides_level_agent_notify() {
        let c = Config::parse(
            r#"
            [[notifications.targets]]
            name = "phone"
            type = "command"
            command = ["true"]
            [defaults]
            level = "suggest"
            notify = ["desktop", "phone"]
            [[defaults.schedule]]
            days = "mon-fri"
            windows = ["09:00-18:00"]
            tz = "Europe/Zurich"
            level = "draft"
            notify = ["desktop"]
            [[defaults.schedule]]
            days = "daily"
            windows = ["00:00-24:00"]
            tz = "Europe/Zurich"
            level = "auto"
            agent = "codex"
            "#,
        )
        .unwrap();
        let at = |t: &str| c.resolve(None).unwrap().at(&format!("{t}[Europe/Zurich]").parse().unwrap());
        // Thursday 10:00: both windows match, the first wins; agent falls back to the repo's.
        let day = at("2026-11-05T10:00");
        assert_eq!((day.level, day.agent, day.notify.as_deref()), (Level::Draft, AgentKind::Claude, Some(&["desktop".to_string()][..])));
        // Thursday 23:00: only the second; notify falls back to the repo's list.
        let night = at("2026-11-05T23:00");
        assert_eq!((night.level, night.agent), (Level::Auto, AgentKind::Codex));
        assert_eq!(night.notify.unwrap(), ["desktop", "phone"]);
        assert_eq!(night.maintainer.level, Level::Draft, "a window never raises maintainer jobs");
        // The caps still win: trust.open keeps it at suggest whatever the window says.
        let open: toml::Table = toml::from_str("[trust]\nopen = true").unwrap();
        let s = c.resolve(Some(&open)).unwrap().at(&"2026-11-05T23:00[Europe/Zurich]".parse().unwrap());
        assert_eq!(s.level, Level::Suggest);
    }

    #[test]
    fn notify_names_must_exist() {
        assert!(Config::parse("[defaults]\nnotify = [\"desktop\"]").is_ok());
        assert!(Config::parse("[defaults]\nnotify = [\"phone\"]").is_err());
        assert!(Config::parse("[[defaults.schedule]]\ndays = \"daily\"\nwindows = [\"00:00-24:00\"]\nnotify = [\"phone\"]").is_err());
        // A repo file is checked against the config.toml it sits under.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("config.toml"),
            "[[notifications.targets]]\nname = \"phone\"\ntype = \"command\"\ncommand = [\"true\"]",
        )
        .unwrap();
        let repo = d.path().join("repos/github/o/r.toml");
        assert!(validate(&repo, "notify = [\"phone\"]").is_ok());
        assert!(validate(&repo, "notify = [\"pager\"]").is_err());
    }

    #[test]
    fn typos_are_errors() {
        assert!(Config::parse("[defaults]\nlevle = \"auto\"").is_err());
        assert!(Config::parse("pol_interval = \"2m\"").is_err());
        assert!(Config::parse("version = 99").is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("").is_err());
        assert_eq!(format_duration(Duration::from_secs(120)), "2m");
    }

    #[test]
    fn edit_file_preserves_comments_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.toml");
        std::fs::write(&p, "# keep me\nlevel = \"draft\"\n").unwrap();
        edit_file(&p, |d| d["subscribed"] = toml_edit::value(true)).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# keep me") && text.contains("subscribed = true"));
        // Another writer's change survives the next edit.
        std::fs::write(&p, "level = \"auto\"\n").unwrap();
        edit_file(&p, |d| d["subscribed"] = toml_edit::value(false)).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("auto"));
        // Invalid edits are rejected before writing.
        assert!(edit_file(&p, |d| d["level"] = toml_edit::value("yolo")).is_err());
        assert!(std::fs::read_to_string(&p).unwrap().contains("auto"));
    }
}
