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
    /// Which agent a handoff starts: `job` (the one that ran it) or any interactive agent,
    /// including ones Night Porter doesn't run jobs with (pi, Cursor, Copilot, ...).
    #[serde(default)]
    pub handoff_agent: HandoffAgent,
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
    /// Other names for this host in clone remotes: SSH `Host` aliases from `~/.ssh/config`
    /// (`github-work`) or mirror names. `git.drupal.org` is built in for `git.drupalcode.org`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// Hosts that serve the same repos under another name.
const BUILTIN_ALIASES: &[(&str, &str)] = &[("git.drupal.org", "git.drupalcode.org")];

impl Account {
    pub fn host(&self) -> &str {
        self.host.as_deref().unwrap_or(self.forge.default_host())
    }

    /// Whether a clone remote's host (lowercase) belongs to this account.
    pub fn matches_host(&self, host: &str) -> bool {
        let me = self.host();
        host == me
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(host))
            || BUILTIN_ALIASES.iter().any(|(alias, canon)| *alias == host && canon.eq_ignore_ascii_case(me))
    }
}

/// An agent you can hand a job to. The first four also run jobs; the rest are for taking over
/// only, because Night Porter can't fence them in for unattended runs the same way.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HandoffAgent {
    /// The agent the job ran with.
    #[default]
    Job,
    Claude,
    OpenCode,
    Codex,
    Gemini,
    /// pi (<https://github.com/badlogic/pi-mono>).
    Pi,
    /// Cursor CLI (`cursor-agent`).
    Cursor,
    /// GitHub Copilot CLI.
    Copilot,
    Goose,
    Qwen,
    /// Factory Droid.
    Droid,
    /// Kiro CLI.
    Kiro,
}

impl HandoffAgent {
    pub const NAMES: &[&str] =
        &["job", "claude", "opencode", "codex", "gemini", "pi", "cursor", "copilot", "goose", "qwen", "droid", "kiro"];

    /// `job` → the job's own agent.
    pub fn or_job(self, job: AgentKind) -> Self {
        match (self, job) {
            (Self::Job, AgentKind::Claude) => Self::Claude,
            (Self::Job, AgentKind::OpenCode) => Self::OpenCode,
            (Self::Job, AgentKind::Codex) => Self::Codex,
            (Self::Job, AgentKind::Gemini) => Self::Gemini,
            (a, _) => a,
        }
    }

    pub fn bin(self) -> &'static str {
        match self {
            Self::Job | Self::Claude => "claude",
            Self::OpenCode => "opencode",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Pi => "pi",
            Self::Cursor => "cursor-agent",
            Self::Copilot => "copilot",
            Self::Goose => "goose",
            Self::Qwen => "qwen",
            Self::Droid => "droid",
            Self::Kiro => "kiro-cli",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Job | Self::Claude => "Claude Code",
            Self::OpenCode => "OpenCode",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Pi => "pi",
            Self::Cursor => "Cursor CLI",
            Self::Copilot => "Copilot CLI",
            Self::Goose => "Goose",
            Self::Qwen => "Qwen Code",
            Self::Droid => "Droid",
            Self::Kiro => "Kiro CLI",
        }
    }

    /// The interactive command line with `brief` as the first prompt (the session stays open).
    pub fn argv(self, brief: &str) -> Vec<String> {
        let pre: &[&str] = match self {
            // A bare prompt makes `gemini` (and its fork `qwen`) answer once and exit; `-i` doesn't.
            Self::Gemini | Self::Qwen | Self::Copilot => &["-i"],
            Self::OpenCode => &["--prompt"],
            // `-s`: stay in the session after the first message.
            Self::Goose => &["run", "-s", "-t"],
            Self::Kiro => &["chat"],
            Self::Job | Self::Claude | Self::Codex | Self::Pi | Self::Cursor | Self::Droid => &[],
        };
        std::iter::once(self.bin()).chain(pre.iter().copied()).map(String::from).chain([brief.to_string()]).collect()
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

/// `agent_args`: one list for whichever agent runs, or a list per agent (so a schedule window
/// that switches Claude to Codex doesn't hand Codex Claude's flags).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum AgentArgs {
    All(Vec<String>),
    Per(std::collections::BTreeMap<AgentKind, Vec<String>>),
}

impl Default for AgentArgs {
    fn default() -> Self {
        Self::All(Vec::new())
    }
}

impl AgentArgs {
    pub fn for_agent(&self, a: AgentKind) -> &[String] {
        match self {
            Self::All(v) => v,
            Self::Per(m) => m.get(&a).map_or(&[], Vec::as_slice),
        }
    }
}

/// Keys of `trigger_agent_args`: a trigger, or a maintainer verb. `review` is both (a review on
/// your PR, and reviewing someone else's). `plan` (the plan step of `plan_first`) and `learn`
/// (`nightporter learn`) are steps, not triggers.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    ReviewComment,
    Review,
    Comment,
    CiFailure,
    MergeConflict,
    Triage,
    Reproduce,
    Fix,
    Plan,
    Learn,
}

impl JobKind {
    /// The one kind a job is: its verb, else the trigger all its events share.
    pub fn of(verb: Option<crate::forge::Verb>, triggers: impl IntoIterator<Item = crate::forge::Trigger>) -> Option<Self> {
        use crate::forge::{Trigger as T, Verb as V};
        if let Some(v) = verb {
            return Some(match v {
                V::Triage => Self::Triage,
                V::Reproduce => Self::Reproduce,
                V::Fix => Self::Fix,
                V::Review => Self::Review,
            });
        }
        let mut ts = triggers.into_iter();
        let first = ts.next()?;
        if ts.any(|t| t != first) {
            return None;
        }
        Some(match first {
            T::ReviewComment => Self::ReviewComment,
            T::Review => Self::Review,
            T::Comment => Self::Comment,
            T::CiFailure => Self::CiFailure,
            T::MergeConflict => Self::MergeConflict,
        })
    }
}

impl RepoSettings {
    /// Swap in the `trigger_agent_args` entry for a job of `kind`, if it has one for this agent.
    pub fn for_job(&mut self, kind: Option<JobKind>) {
        let Some(o) = kind.and_then(|k| self.trigger_agent_args.get(&k)) else { return };
        if matches!(o, AgentArgs::All(_)) || matches!(o, AgentArgs::Per(m) if m.contains_key(&self.agent)) {
            self.agent_args = AgentArgs::All(o.for_agent(self.agent).to_vec());
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
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
    /// Extra args for the agent CLI: a list for every agent (`["--model", "opus"]`) or one list
    /// per agent (`{ claude = ["--model", "opus"], codex = ["-m", "gpt-5"] }`). Empty: the agent's
    /// own default model and settings.
    #[serde(default)]
    pub agent_args: AgentArgs,
    /// `agent_args` for one kind of job, e.g. a cheaper model for `ci_failure` or `triage`. Used
    /// when every event in the job is that kind (a maintainer job goes by its verb); a mixed batch,
    /// or an agent the per-agent map doesn't name, keeps `agent_args`.
    #[serde(default)]
    pub trigger_agent_args: std::collections::BTreeMap<JobKind, AgentArgs>,
    /// Kinds of job that write a plan first and wait for you to approve it (even at `auto`)
    /// before they change any code, e.g. `["fix"]`.
    #[serde(default)]
    pub plan_first: Vec<JobKind>,
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
    /// Old switch: `false` means `attribution = "marker"`. `attribution` wins when both are set.
    #[serde(default = "yes")]
    pub footer: bool,
    #[serde(default)]
    pub attribution: Option<Attribution>,
    #[serde(default)]
    pub resolve_threads: bool,
}

/// What marks a reply as Night Porter's.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Attribution {
    /// Visible "via Night Porter" footer plus the hidden marker.
    Footer,
    /// Hidden `<!-- nightporter -->` marker only.
    Marker,
    /// Nothing: replies read as if you typed them.
    None,
}

impl Attribution {
    pub const NAMES: &[&str] = &["footer", "marker", "none"];

    /// For short status posts (bot re-review asks, command replies): never the visible footer.
    pub fn quiet(self) -> Self {
        if self == Self::Footer { Self::Marker } else { self }
    }
}

impl ReplySettings {
    pub fn attribution(&self) -> Attribution {
        self.attribution.unwrap_or(if self.footer { Attribution::Footer } else { Attribution::Marker })
    }
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
    /// Let jobs edit `.env.*` files (`.env.example` and friends). A bare `.env` stays protected.
    #[serde(default)]
    pub allow_env_variants: bool,
}

impl Default for Checks {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Env {
    /// Install dependencies from the lockfiles in the repo root before the agent starts
    /// (`composer install`, `npm ci`, …). Runs outside the sandbox, like `setup`.
    #[serde(default)]
    pub install: bool,
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
#[allow(clippy::struct_excessive_bools, reason = "mirrors the [maintainer] TOML table")]
pub struct Maintainer {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "draft")]
    pub level: Level,
    #[serde(default = "np_label")]
    pub label: String,
    /// Queue a review when someone requests yours on a PR (GitHub).
    #[serde(default)]
    pub review_requests: bool,
    /// Put each ready (non-draft) PR/MR by someone else in the inbox, so you can queue a review
    /// of PRs nobody asked you to look at.
    #[serde(default)]
    pub watch_prs: bool,
    /// Outcomes a review job may post as a real review, after you approve the job: `comment`,
    /// `request_changes`, `approve`. Empty: reviews are posted as a plain comment.
    #[serde(default)]
    pub review_verdicts: Vec<crate::forge::ReviewVerdict>,
    /// Reviews of someone else's PR may run its tests and env setup, after you approve running
    /// it. Off: they only read the code and don't wait for that approval (Claude Code, Gemini CLI).
    #[serde(default)]
    pub review_runs_code: bool,
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
        if let Some(k) = s.plan_first.iter().find(|k| matches!(k, JobKind::Plan | JobKind::Learn)) {
            bail!("plan_first: {:?} is a step, not a kind of job", crate::state::enum_str(k));
        }
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
        assert_eq!(s.reply.attribution(), Attribution::Marker);
        assert_eq!(s.trust.allow_users, vec!["alice"]);
    }

    #[test]
    fn attribution_wins_over_footer() {
        let c = Config::parse("[defaults.reply]\nfooter = true").unwrap();
        assert_eq!(c.resolve(None).unwrap().reply.attribution(), Attribution::Footer);
        let over: toml::Table = toml::from_str("[reply]\nattribution = \"none\"").unwrap();
        assert_eq!(c.resolve(Some(&over)).unwrap().reply.attribution(), Attribution::None);
        assert!(Config::parse("[defaults.reply]\nattribution = \"loud\"").is_err());
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
    fn agent_args_per_agent() {
        use AgentKind::*;
        // A plain list goes to whichever agent runs (and an empty one means the agent's defaults).
        let c = Config::parse("[defaults]\nagent_args = [\"--model\", \"opus\"]").unwrap();
        assert_eq!(c.resolve(None).unwrap().agent_args.for_agent(Codex), ["--model", "opus"]);
        assert_eq!(Config::parse("").unwrap().resolve(None).unwrap().agent_args.for_agent(Claude), [] as [String; 0]);
        // Per agent: defaults and the repo file merge key by key.
        let c = Config::parse("[defaults.agent_args]\nclaude = [\"--model\", \"opus\"]\ncodex = [\"-m\", \"o3\"]").unwrap();
        let over: toml::Table = toml::from_str("[agent_args]\nclaude = [\"--model\", \"sonnet\"]").unwrap();
        let s = c.resolve(Some(&over)).unwrap();
        assert_eq!(s.agent_args.for_agent(Claude), ["--model", "sonnet"]);
        assert_eq!(s.agent_args.for_agent(Codex), ["-m", "o3"]);
        assert_eq!(s.agent_args.for_agent(Gemini), [] as [String; 0]);
        // A repo list replaces a per-agent table.
        let over: toml::Table = toml::from_str("agent_args = [\"--x\"]").unwrap();
        assert_eq!(c.resolve(Some(&over)).unwrap().agent_args.for_agent(Gemini), ["--x"]);
        assert!(Config::parse("[defaults.agent_args]\nnope = []").and_then(|c| c.resolve(None)).is_err());
    }

    #[test]
    fn trigger_agent_args_pick_by_job_kind() {
        use crate::forge::{Trigger as T, Verb};
        let c = Config::parse(
            "[defaults]\nagent_args = [\"--model\", \"opus\"]\n[defaults.trigger_agent_args]\nci_failure = [\"--model\", \"haiku\"]\ntriage = { codex = [\"-m\", \"mini\"] }",
        )
        .unwrap();
        let over: toml::Table = toml::from_str("[trigger_agent_args]\nmerge_conflict = [\"--model\", \"sonnet\"]").unwrap();
        let base = c.resolve(Some(&over)).unwrap();
        let args = |kind| {
            let mut s = base.clone();
            s.for_job(kind);
            s.agent_args.for_agent(AgentKind::Claude).join(" ")
        };
        assert_eq!(args(JobKind::of(None, [T::CiFailure, T::CiFailure])), "--model haiku");
        assert_eq!(args(JobKind::of(None, [T::MergeConflict])), "--model sonnet", "repo and global maps merge");
        assert_eq!(args(JobKind::of(None, [T::CiFailure, T::Comment])), "--model opus", "mixed batch");
        assert_eq!(args(JobKind::of(Some(Verb::Triage), [T::Comment])), "--model opus", "no claude entry: keep agent_args");
        assert!(Config::parse("[defaults.trigger_agent_args]\nnope = []").and_then(|c| c.resolve(None)).is_err());
        assert!(Config::parse("[defaults]\nplan_first = [\"plan\"]").is_err(), "plan is a step, not a kind of job");
        assert!(Config::parse("[defaults]\nplan_first = [\"fix\", \"ci_failure\"]").is_ok());
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
