//! The TUI's Settings tab: every `config.toml` key with its effective value. Every change goes
//! through [`edit_file`], which validates before writing, and the "edit the file" escape hatch
//! only saves a copy that validates. A typo can't stop the daemon.

use crate::config::{Config, edit_file, validate, write_atomic};
use crate::paths::Paths;
use anyhow::{Context, Result, bail};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Choice(&'static [&'static str]),
    /// Any string; durations (`2m`) are strings too and are checked on save.
    Text,
    Int,
    Float,
    /// Comma-separated strings.
    List,
    /// `mon-fri 08:00-09:00,17:00-22:00; sat 10:00-12:00`.
    Schedule,
    /// Read-only here (needs the wizard or the file).
    Info,
}

pub struct Setting {
    /// Dotted path in `config.toml`.
    pub key: &'static str,
    pub kind: Kind,
    pub help: &'static str,
    /// Setting this value asks first (it loosens a safety default).
    pub confirm: Option<&'static str>,
}

const LEVELS: &[&str] = &["observe", "suggest", "draft", "auto"];

const fn s(key: &'static str, kind: Kind, help: &'static str) -> Setting {
    Setting { key, kind, help, confirm: None }
}
const fn risky(key: &'static str, kind: Kind, value: &'static str, help: &'static str) -> Setting {
    Setting { key, kind, help, confirm: Some(value) }
}

use Kind::{Bool, Choice, Float, Info, Int, List, Schedule, Text};

pub static SETTINGS: &[Setting] = &[
    // Global
    s("code_dir", Text, "Folder where your clones live."),
    s("scan_depth", Int, "How many folders deep to look for clones under code_dir."),
    s("accounts", Info, "Forge accounts (GitHub / GitLab and host). Press S for the setup wizard, or e to edit the file."),
    s("poll_interval", Text, "How often to check for new activity, e.g. 2m. Never faster than the forge allows."),
    s("sweep_interval", Text, "How often to sweep open PRs for merge conflicts and CI, e.g. 10m."),
    s("max_concurrent_jobs", Int, "Jobs running at once, across all repos."),
    s("daily_job_cap", Int, "Most jobs started per 24 hours."),
    s("min_free_gb", Int, "Jobs wait while free disk space is below this many GB."),
    s("retention_days", Int, "Days of job history kept."),
    s("worktree_idle_days", Int, "Idle worktrees are removed after this many days."),
    s("handoff", Choice(&["auto", "herdr", "rex"]), "Where h (take over) opens a job."),
    s(
        "check_updates",
        Bool,
        "Once a day, ask GitHub for the latest release number and tell you when there's a new one. Sends nothing about you or your repos.",
    ),
    s("marketplaces", List, "Extra pack marketplace git URLs."),
    s("notifications.needs_approval", Bool, "Desktop notification when a job needs your approval."),
    s("notifications.failed", Bool, "Desktop notification when a job fails."),
    s("notifications.done", Bool, "Desktop notification when a job is done."),
    s("notifications.new_author", Bool, "Desktop notification when someone untrusted lands in the inbox."),
    // Defaults for every repo (each repo file can override them)
    s("defaults.agent", Choice(&["claude", "opencode", "codex", "gemini"]), "Coding agent: Claude Code, OpenCode, Codex or Gemini CLI."),
    s("defaults.agent_args", List, "Extra agent CLI arguments, e.g. --model, opus"),
    risky(
        "defaults.level",
        Choice(LEVELS),
        "auto",
        "observe = notify, suggest = commit locally, draft = you approve each push, auto = pushes on its own.",
    ),
    s("defaults.triggers", List, "What starts a job: review_comment, review, comment, ci_failure, merge_conflict."),
    s("defaults.debounce", Text, "Quiet time before a batch of activity becomes a job, e.g. 2m."),
    s("defaults.allow_commands", List, "Commands the agent may run (prefix match), e.g. npm test, cargo test"),
    s("defaults.timeout", Text, "Time limit per agent run and per env command, e.g. 30m."),
    s("defaults.max_concurrent", Int, "Running jobs per repo. Empty = unlimited."),
    s("defaults.max_ci_attempts", Int, "Automatic CI-fix attempts before giving up."),
    risky("defaults.ci_auto", Bool, "true", "Let CI-failure jobs run at auto. CI output is untrusted text, so off by default."),
    s("defaults.max_budget_usd", Float, "Spending cap per run in USD (Claude Code only). Empty = none."),
    risky("defaults.sandbox", Bool, "false", "OS sandbox for the agent's shell commands. Only turn off for Docker/DDEV repos."),
    s("defaults.sandbox_domains", List, "Domains sandboxed commands may reach."),
    s("defaults.sandbox_read_allow", List, "Extra folders in your home folder sandboxed commands may read, e.g. ~/.config/mytool"),
    s("defaults.prompt", Text, "Your own extra instructions file, relative to the config folder."),
    s("defaults.packs", List, "Enabled packs."),
    s("defaults.schedule", Schedule, "Active hours, e.g. mon-fri 08:00-09:00,17:00-22:00; sat 10:00-12:00. Empty = any time."),
    s("defaults.trust.roles", List, "Trusted roles: owner, maintainer, member, collaborator, contributor, none."),
    s("defaults.trust.allow_users", List, "Always-trusted usernames."),
    s("defaults.trust.deny_users", List, "Never-trusted usernames (repo files can't undo these)."),
    s("defaults.trust.allow_teams", List, "Trusted teams: org/team on GitHub, group on GitLab."),
    s("defaults.trust.deny_teams", List, "Never-trusted teams."),
    s("defaults.trust.allow_bots", List, "Trusted bots, e.g. coderabbitai[bot]. Bots are never trusted by role."),
    risky("defaults.trust.open", Bool, "true", "Trust everyone. Caps the level at suggest."),
    s("defaults.reply.style", Text, "concise, detailed-tech-lead, non-technical, a pack style or a .md file."),
    s("defaults.reply.language", Text, "Reply language, e.g. en."),
    s("defaults.reply.footer", Bool, "Add the AI-disclosure footer to replies."),
    s("defaults.reply.resolve_threads", Bool, "Resolve review threads after replying."),
    s("defaults.checks.protected", List, "Extra protected path globs (added to the built-in list)."),
    s("defaults.checks.escalate_on", List, "Changes that always need your approval: deps, size."),
    s("defaults.checks.max_diff_lines", Int, "Diff size that counts as large (for escalate_on = size)."),
    s("defaults.env.setup", List, "Commands run before the agent, e.g. ddev start"),
    s("defaults.env.teardown", List, "Commands run after the job."),
    s("defaults.env.cleanup", List, "Commands run when the worktree is removed."),
    s("defaults.env.prompt", Text, "Extra instructions file for the environment."),
    s("defaults.maintainer.enabled", Bool, "Maintainer mode: triage, reproduce, fix or review others' issues and MRs on request."),
    s("defaults.maintainer.level", Choice(&["observe", "suggest", "draft"]), "Level for maintainer jobs (capped at draft)."),
    s("defaults.maintainer.label", Text, "Label that asks Night Porter to look at an issue or MR."),
];

#[derive(Clone)]
pub struct Row {
    pub setting: &'static Setting,
    /// Effective value, as editable text.
    pub value: String,
    /// Set in the file (vs. the built-in default).
    pub set: bool,
}

fn lookup<'a>(t: &'a toml::Table, key: &str) -> Option<&'a toml::Value> {
    let (mut cur, mut parts) = (None, key.split('.'));
    let mut table = t;
    for p in parts.by_ref() {
        cur = table.get(p);
        match cur {
            Some(toml::Value::Table(t)) => table = t,
            _ => break,
        }
    }
    if parts.next().is_some() { None } else { cur }
}

fn text(v: Option<&toml::Value>, kind: Kind) -> String {
    let str_of = |v: &toml::Value| v.as_str().map_or_else(|| v.to_string(), String::from);
    match v {
        None => String::new(),
        Some(toml::Value::Array(a)) if kind == Schedule => a
            .iter()
            .map(|w| {
                let list = |k| lookup(w.as_table().unwrap_or(&toml::Table::new()), k).map(|v| text(Some(v), List)).unwrap_or_default();
                format!("{} {}", list("days"), list("windows").replace(", ", ","))
            })
            .collect::<Vec<_>>()
            .join("; "),
        Some(toml::Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                toml::Value::Table(t) => t.values().map(str_of).collect::<Vec<_>>().join(" "),
                x => str_of(x),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(v) => str_of(v),
    }
}

/// All settings with their effective values. Fails if `config.toml` doesn't load.
pub fn rows(paths: &Paths) -> Result<Vec<Row>> {
    let cfg = Config::load(paths)?;
    let raw: toml::Table = std::fs::read_to_string(paths.config_file()).unwrap_or_default().parse()?;
    let mut eff = toml::Table::try_from(&cfg)?;
    eff.insert("defaults".into(), toml::Value::try_from(cfg.resolve(None)?)?);
    Ok(SETTINGS.iter().map(|s| Row { setting: s, value: text(lookup(&eff, s.key), s.kind), set: lookup(&raw, s.key).is_some() }).collect())
}

/// The value Enter switches to, for `Bool` and `Choice` settings.
pub fn next(s: &Setting, current: &str) -> Option<&'static str> {
    let opts: &[&'static str] = match s.kind {
        Bool => &["true", "false"],
        Choice(o) => o,
        _ => return None,
    };
    let i = opts.iter().position(|o| *o == current).map_or(0, |i| i + 1);
    Some(opts[i % opts.len()])
}

/// Parse `input` for `s`; `None` means "back to the default" (empty input).
fn item(s: &Setting, input: &str) -> Result<Option<toml_edit::Item>> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let v = match s.kind {
        Info => bail!("{} can't be changed here: {}", s.key, s.help),
        Bool => toml_edit::value(input.parse::<bool>().map_err(|_| anyhow::anyhow!("use true or false"))?),
        Choice(o) if !o.contains(&input) => bail!("use one of {}", o.join(", ")),
        Choice(_) | Text => toml_edit::value(input),
        Int => toml_edit::value(input.parse::<i64>().map_err(|_| anyhow::anyhow!("{input:?} is not a whole number"))?),
        Float => toml_edit::value(input.parse::<f64>().map_err(|_| anyhow::anyhow!("{input:?} is not a number"))?),
        // ponytail: comma-separated, so a single item can't contain a comma; use e for those.
        List => toml_edit::value(input.split(',').map(str::trim).filter(|x| !x.is_empty()).collect::<toml_edit::Array>()),
        Schedule => {
            let mut aot = toml_edit::ArrayOfTables::new();
            for part in input.split(';').filter(|p| !p.trim().is_empty()) {
                let w = crate::setup::parse_schedule(part)?.context("empty schedule window")?;
                let mut t = toml_edit::Table::new();
                t["days"] = toml_edit::value(&w.days);
                t["windows"] = toml_edit::value(w.windows.iter().map(String::as_str).collect::<toml_edit::Array>());
                aot.push(t);
            }
            toml_edit::Item::ArrayOfTables(aot)
        }
    };
    Ok(Some(v))
}

/// Write `input` for `s` into `config.toml` (empty = remove the key, back to the default).
/// Nothing is written unless the whole file still validates.
pub fn set(paths: &Paths, s: &Setting, input: &str) -> Result<String> {
    if s.kind == Schedule && std::fs::read_to_string(paths.config_file()).unwrap_or_default().contains("tz =") {
        bail!("your schedule has time zones, which this field would drop; press e to edit the file");
    }
    let new = item(s, input)?;
    let (parents, last) = s.key.rsplit_once('.').map_or((None, s.key), |(p, l)| (Some(p), l));
    let msg = match &new {
        Some(_) => format!("{} = {}", s.key, input.trim()),
        None => format!("{} back to the default", s.key),
    };
    edit_file(&paths.config_file(), |d| {
        let mut at = d.as_item_mut();
        for p in parents.into_iter().flat_map(|p| p.split('.')) {
            // Real `[table]`s: indexing a missing key makes an inline table, and toml_edit
            // silently drops an array of tables (`[[defaults.schedule]]`) put inside one.
            if !at.get(p).is_some_and(toml_edit::Item::is_table) {
                let t = at.get(p).cloned().and_then(|i| i.into_table().ok()).unwrap_or_else(|| {
                    let mut t = toml_edit::Table::new();
                    t.set_implicit(true);
                    t
                });
                at[p] = toml_edit::Item::Table(t);
            }
            at = &mut at[p];
        }
        match new {
            Some(v) => at[last] = v,
            None => {
                if let Some(t) = at.as_table_like_mut() {
                    t.remove(last);
                }
            }
        }
    })?;
    Ok(msg)
}

/// Edit `path` in `$VISUAL` / `$EDITOR` (else `vi`), on a copy. The copy replaces the file only
/// if it validates; otherwise you can fix it or throw it away. Runs outside the TUI.
pub fn edit_in_editor(path: &Path) -> Result<String> {
    let read = || match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        r => r.with_context(|| format!("read {}", path.display())),
    };
    let orig = read()?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("config.toml");
    // Outside the config folder, so the daemon never sees a half-edited file.
    let tmp = std::env::temp_dir().join(format!("nightporter-{}-{name}", std::process::id()));
    std::fs::write(&tmp, &orig)?;
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".into());
    let result = loop {
        // Through the shell so EDITOR="code --wait" works.
        let ok = std::process::Command::new("sh").arg("-c").arg(format!("{editor} \"$1\"")).arg("sh").arg(&tmp).status()?.success();
        let text = std::fs::read_to_string(&tmp)?;
        if !ok || text == orig {
            break Ok("no changes saved".into());
        }
        match validate(path, &text) {
            Ok(()) if read()? != orig => {
                let keep = tmp.with_extension("toml.mine");
                std::fs::rename(&tmp, &keep)?;
                return Ok(format!(
                    "{} changed while you were editing; nothing saved, your version is in {}",
                    path.display(),
                    keep.display()
                ));
            }
            Ok(()) => break write_atomic(path, text.as_bytes()).map(|()| format!("saved {} (checked: valid)", path.display())),
            Err(e) => {
                println!("\nThat file isn't valid, so it wasn't saved:\n  {e:#}\n");
                if !crate::setup::yes("Fix it now?", true)? {
                    break Ok("invalid edit discarded; nothing saved".into());
                }
            }
        }
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Dotted keys a repo file sets, e.g. `level`, `reply.style`.
fn set_keys(t: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (k, v) in t {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            toml::Value::Table(inner) => set_keys(inner, &key, out),
            _ => out.push(key),
        }
    }
}

/// Everything that shapes Night Porter's work on one repo, in plain words: what the agent is
/// told, how it replies, when it runs, who it listens to. Used by `nightporter repo show` and
/// Enter on the TUI's Repos tab.
pub fn repo_text(paths: &Paths, cfg: &Config, repo: &crate::forge::RepoId, clone: Option<&Path>) -> Result<String> {
    use std::fmt::Write;
    let s = cfg.settings_for(paths, repo)?;
    let file = paths.repo_file(repo);
    let mut over = Vec::new();
    if let Ok(t) = std::fs::read_to_string(&file).unwrap_or_default().parse::<toml::Table>() {
        set_keys(&t, "", &mut over);
    }
    let on = |b: bool| if b { "on" } else { "off" };
    let list = |v: &[String]| if v.is_empty() { "(none)".to_string() } else { v.join(", ") };
    let mut o = String::new();
    let _ = writeln!(o, "{}/{}", repo.host, repo.path);
    if let Some(c) = clone {
        let _ = writeln!(o, "Clone:      {}", c.display());
    }
    let _ = writeln!(o, "Repo file:  {}{}", file.display(), if file.exists() { "" } else { "  (not created: every value is the default)" });
    if !over.is_empty() {
        let _ = writeln!(o, "Set here:   {} (everything else comes from [defaults] in config.toml)", over.join(", "));
    }

    let _ = writeln!(o, "\nWHAT IT DOES");
    let _ = writeln!(o, "  watching       {}", if s.subscribed { "yes" } else { "no (not subscribed)" });
    let _ = writeln!(o, "  level          {}", s.level.as_str());
    let args = if s.agent_args.is_empty() { String::new() } else { format!("  args: {}", s.agent_args.join(" ")) };
    let _ = writeln!(o, "  agent          {}{args}", s.agent.label());
    let triggers: Vec<String> = s.triggers.iter().map(crate::state::enum_str).collect();
    let _ = writeln!(o, "  starts on      {}", triggers.join(", "));
    let _ = writeln!(
        o,
        "  maintainer     {} (level {}, label {:?})",
        on(s.maintainer.enabled),
        s.maintainer.level.as_str(),
        s.maintainer.label
    );

    let _ = writeln!(o, "\nREPLIES");
    let builtin = crate::prompts::STYLES.iter().any(|(n, _)| *n == s.reply.style);
    let _ = writeln!(o, "  style          {}{}", s.reply.style, if builtin { "  (built in)" } else { "" });
    let _ = writeln!(o, "  language       {}", s.reply.language);
    let _ = writeln!(o, "  footer         {}   resolve threads {}", on(s.reply.footer), on(s.reply.resolve_threads));
    match crate::prompts::style(paths, &s.reply.style) {
        Ok(text) => {
            let _ = writeln!(o, "  style text:\n{}", indent(&text, 12));
        }
        Err(e) => {
            let _ = writeln!(o, "  style ERROR: {e:#}");
        }
    }

    let _ = writeln!(o, "\nINSTRUCTIONS THE AGENT GETS (besides the request itself)");
    if let Some(c) = clone {
        let found: Vec<&str> = ["CLAUDE.md", "AGENTS.md", "CONTRIBUTING.md"].into_iter().filter(|f| c.join(f).is_file()).collect();
        let _ = writeln!(o, "  from the repo  {}", if found.is_empty() { "(no CLAUDE.md / AGENTS.md)".into() } else { found.join(", ") });
    }
    for (name, p) in [("prompt", &s.prompt), ("env.prompt", &s.env.prompt)] {
        match p {
            None => {
                let _ = writeln!(o, "  {name:<14} (none)");
            }
            Some(p) => match crate::prompts::read_user_file(paths, p) {
                Ok(text) => {
                    let _ = writeln!(o, "  {name:<14} {p}:\n{}", indent(&text, 12));
                }
                Err(e) => {
                    let _ = writeln!(o, "  {name:<14} {p}  ERROR: {e:#}");
                }
            },
        }
    }
    let _ = writeln!(o, "  packs          {}", list(&s.packs));

    let _ = writeln!(o, "\nWHEN AND HOW MUCH");
    let sched: Vec<String> = s.schedule.iter().map(|w| format!("{} {}", w.days, w.windows.join(","))).collect();
    let _ = writeln!(o, "  hours          {}", if sched.is_empty() { "any time".into() } else { sched.join("; ") });
    let _ = writeln!(
        o,
        "  debounce       {}   timeout {}",
        crate::config::format_duration(s.debounce),
        crate::config::format_duration(s.timeout)
    );
    let _ = writeln!(o, "  CI-fix tries   {}   CI jobs at auto {}", s.max_ci_attempts, on(s.ci_auto));
    if let Some(b) = s.max_budget_usd {
        let _ = writeln!(o, "  budget         ${b:.2} per run");
    }

    let _ = writeln!(o, "\nWHO IT LISTENS TO");
    let roles: Vec<String> = s.trust.roles.iter().map(crate::state::enum_str).collect();
    let _ = writeln!(o, "  roles          {}", roles.join(", "));
    let _ = writeln!(o, "  always trust   {}", list(&s.trust.allow_users));
    let _ = writeln!(o, "  never trust    {}", list(&s.trust.deny_users));
    let _ = writeln!(o, "  bots           {}", list(&s.trust.allow_bots));
    if s.trust.open {
        let _ = writeln!(o, "  OPEN: everyone is trusted (level capped at suggest)");
    }

    let _ = writeln!(o, "\nSAFETY");
    let _ = writeln!(o, "  sandbox        {}   network: {}", on(s.sandbox), list(&s.sandbox_domains));
    let _ = writeln!(o, "  may run        {}", list(&s.allow_commands));
    let esc: Vec<String> = s.checks.escalate_on.iter().map(crate::state::enum_str).collect();
    let _ = writeln!(o, "  always ask on  {}   protected extra: {}", esc.join(", "), list(&s.checks.protected));
    let _ = writeln!(o, "  env setup      {}", list(&s.env.setup));

    let _ = writeln!(o, "\nChange: `e` in the TUI's Repos tab, or edit {}", file.display());
    Ok(o)
}

fn indent(text: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    text.trim_end().lines().map(|l| format!("{pad}{l}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::at(&d.path().join("c"), &d.path().join("d"));
        p.ensure().unwrap();
        (d, p)
    }
    fn get(p: &Paths, key: &str) -> Row {
        rows(p).unwrap().into_iter().find(|r| r.setting.key == key).unwrap()
    }
    fn setting(key: &str) -> &'static Setting {
        SETTINGS.iter().find(|s| s.key == key).unwrap()
    }

    #[test]
    fn repo_text_says_what_the_repo_gets() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        std::fs::write(paths.config_dir.join("mine.md"), "Always run the linter.").unwrap();
        std::fs::write(paths.config_file(), "[defaults]\nprompt = \"mine.md\"\n").unwrap();
        let repo = crate::forge::RepoId::new(crate::forge::ForgeKind::GitHub, "github.com", "o/r");
        edit_file(&paths.repo_file(&repo), |d| {
            d["level"] = toml_edit::value("suggest");
            d["reply"]["style"] = toml_edit::value("non-technical");
        })
        .unwrap();
        let cfg = Config::load(&paths).unwrap();
        let t = super::repo_text(&paths, &cfg, &repo, None).unwrap();
        assert!(t.contains("Set here:   level, reply.style"), "{t}");
        assert!(t.contains("level          suggest") && t.contains("non-technical  (built in)"), "{t}");
        assert!(t.contains("Always run the linter."), "the prompt file's text is shown: {t}");
    }

    #[test]
    fn every_key_shows_and_round_trips() {
        let (_d, p) = paths();
        std::fs::write(p.config_file(), "# keep me\nversion = 1\n").unwrap();
        let rows = rows(&p).unwrap();
        assert_eq!(rows.len(), SETTINGS.len());
        assert_eq!(get(&p, "defaults.level").value, "draft");
        assert!(!get(&p, "defaults.level").set);
        assert_eq!(get(&p, "poll_interval").value, "2m");
        // Saving each shown value back is accepted (the editor prefill round-trips).
        for r in rows.iter().filter(|r| r.setting.kind != Info) {
            set(&p, r.setting, &r.value).unwrap_or_else(|e| panic!("{}: {e:#}", r.setting.key));
        }
        assert!(std::fs::read_to_string(p.config_file()).unwrap().starts_with("# keep me"));
    }

    #[test]
    fn edits_are_validated_and_resettable() {
        let (_d, p) = paths();
        set(&p, setting("defaults.reply.footer"), "false").unwrap();
        assert!(get(&p, "defaults.reply.footer").set);
        set(&p, setting("defaults.schedule"), "mon-fri 08:00-09:00, 17:00-22:00; sat 10:00-12:00").unwrap();
        let file = std::fs::read_to_string(p.config_file()).unwrap();
        assert!(file.contains("[[defaults.schedule]]") && file.contains("[defaults.reply]"), "real tables, not inline: {file}");
        assert_eq!(get(&p, "defaults.schedule").value, "mon-fri 08:00-09:00,17:00-22:00; sat 10:00-12:00");
        set(&p, setting("defaults.trust.allow_bots"), "a[bot], b[bot]").unwrap();
        assert_eq!(get(&p, "defaults.trust.allow_bots").value, "a[bot], b[bot]");

        let before = std::fs::read_to_string(p.config_file()).unwrap();
        for (k, v) in [("poll_interval", "soon"), ("defaults.triggers", "push"), ("defaults.schedule", "mon 9-5"), ("scan_depth", "x")] {
            assert!(set(&p, setting(k), v).is_err(), "{k} = {v} must be refused");
        }
        assert_eq!(std::fs::read_to_string(p.config_file()).unwrap(), before, "a refused edit writes nothing");

        set(&p, setting("defaults.reply.footer"), "").unwrap();
        assert!(!get(&p, "defaults.reply.footer").set);
        assert!(set(&p, setting("accounts"), "x").is_err());
    }

    #[test]
    fn cycles_choices() {
        assert_eq!(next(setting("defaults.level"), "draft"), Some("auto"));
        assert_eq!(next(setting("defaults.level"), "auto"), Some("observe"));
        assert_eq!(next(setting("defaults.sandbox"), "true"), Some("false"));
        assert_eq!(next(setting("code_dir"), "x"), None);
    }

    #[test]
    fn editor_saves_only_valid_files() {
        let (_d, p) = paths();
        std::fs::write(p.config_file(), "version = 1\n").unwrap();
        // SAFETY: test-only env; the editor tests don't run concurrently with other EDITOR readers.
        unsafe { std::env::set_var("VISUAL", "printf 'scan_depth = 4\\n' >>") };
        assert!(edit_in_editor(&p.config_file()).unwrap().contains("valid"));
        assert!(std::fs::read_to_string(p.config_file()).unwrap().contains("scan_depth = 4"));
        unsafe { std::env::set_var("VISUAL", "true") };
        assert_eq!(edit_in_editor(&p.config_file()).unwrap(), "no changes saved");
        unsafe { std::env::remove_var("VISUAL") };
    }
}
