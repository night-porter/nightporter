//! Terminal UI. Keyboard-only, text labels for every state (colour is decoration, never the only
//! signal), honours `NO_COLOR`. Reads/writes the same SQLite state as the daemon.

use crate::actions::{self, Scope};
use crate::config::{AgentKind, Config};
use crate::forge::Event;
use crate::paths::Paths;
use crate::state::{Db, Disposition, Job, JobStatus};
use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Queue,
    Inbox,
    History,
    Repos,
    Settings,
    Help,
}
const TABS: [(Tab, &str); 6] = [
    (Tab::Queue, "1 Queue"),
    (Tab::Inbox, "2 Inbox"),
    (Tab::History, "3 History"),
    (Tab::Repos, "4 Repos"),
    (Tab::Settings, "5 Settings"),
    (Tab::Help, "? Help"),
];

/// Work that needs the real terminal: the main loop leaves the alternate screen for it.
enum Suspend {
    Setup,
    Editor(std::path::PathBuf),
}

/// A pending yes/no question and what to do on "y".
type Confirm = Box<dyn FnOnce(&mut App) -> Result<String>>;

#[derive(Clone)]
struct RepoRow {
    id: crate::forge::RepoId,
    subscribed: bool,
    level: String,
    agent: String,
    /// What an open schedule window changes right now, e.g. `now auto, from window`. `l` and `a`
    /// cycle the repo's own level and agent (shown), not the window's.
    window: String,
}

#[allow(clippy::struct_excessive_bools, reason = "independent UI flags, not a state machine")]
struct App {
    paths: Paths,
    db: Db,
    tab: Tab,
    sel: ListState,
    queue: Vec<Job>,
    history: Vec<Job>,
    inbox: Vec<Event>,
    repos: Vec<RepoRow>,
    detail: Option<i64>,
    /// Rendered once per refresh, not per frame (a job can carry a large diff).
    detail_text: String,
    scroll: u16,
    msg: String,
    confirm: Option<(String, Confirm)>,
    color: bool,
    daemon_up: bool,
    paused: bool,
    suspend: Option<Suspend>,
    settings: Vec<crate::settings::Row>,
    /// Why the Settings tab is empty: `config.toml` doesn't load.
    settings_err: String,
    /// Text being typed for the selected setting.
    input: Option<String>,
    /// Unfiltered Repos and Settings rows; `repos` / `settings` are what `filter` lets through.
    repos_all: Vec<RepoRow>,
    settings_all: Vec<crate::settings::Row>,
    /// `/` search on the Repos and Settings tabs; `filtering` while it's being typed.
    filter: String,
    filtering: bool,
    history_all: Vec<Job>,
    /// Enter on the Repos tab: that repo's settings, shown in the detail pane.
    repo_view: Option<crate::forge::RepoId>,
    /// This session's dismissals and ignores, newest last, for `u` (undo).
    undo_jobs: Vec<i64>,
    undo_events: Vec<String>,
    update: Option<String>,
}

/// No forge account configured yet: nothing can be watched, so the wizard is the useful first step.
fn needs_setup(paths: &Paths) -> bool {
    Config::load(paths).is_ok_and(|c| c.accounts.is_empty())
}

/// Run the wizard outside the TUI, then wait so its last lines can be read.
fn setup_then_pause(paths: &Paths) {
    if let Err(e) = crate::setup::run(paths) {
        println!("\nsetup stopped: {e:#}");
    }
    print!("\nPress Enter to continue to Night Porter… ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let _ = std::io::stdin().read_line(&mut String::new());
}

pub fn run(paths: &Paths) -> Result<()> {
    if needs_setup(paths) && crate::setup::yes("Night Porter isn't set up yet. Run the setup wizard now?", true)? {
        setup_then_pause(paths);
    }
    let mut app = App::new(paths, Db::open(&paths.db())?);
    app.refresh(true);
    app.update = crate::update::banner(&app.db);
    if !app.daemon_up {
        app.msg = "Night Porter's daemon isn't running, so nothing is being watched. Press D to start it.".into();
    } else if let Some(u) = &app.update {
        app.msg.clone_from(u);
    }
    let mut term = ratatui::init();
    let res = app.main_loop(&mut term);
    ratatui::restore();
    res
}

impl App {
    fn new(paths: &Paths, db: Db) -> Self {
        App {
            paths: paths.clone(),
            db,
            tab: Tab::Queue,
            sel: ListState::default().with_selected(Some(0)),
            queue: vec![],
            history: vec![],
            inbox: vec![],
            repos: vec![],
            detail: None,
            detail_text: String::new(),
            scroll: 0,
            msg: "Press ? for help.".into(),
            confirm: None,
            color: std::env::var_os("NO_COLOR").is_none(),
            daemon_up: false,
            paused: false,
            suspend: None,
            settings: vec![],
            settings_err: String::new(),
            input: None,
            repos_all: vec![],
            settings_all: vec![],
            filter: String::new(),
            filtering: false,
            history_all: vec![],
            repo_view: None,
            undo_jobs: vec![],
            undo_events: vec![],
            update: None,
        }
    }

    fn main_loop(&mut self, term: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut last = Instant::now();
        loop {
            term.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(500))?
                && let CEvent::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
                && !self.key(k)
            {
                return Ok(());
            }
            if let Some(job) = self.suspend.take() {
                ratatui::restore();
                self.msg = match job {
                    Suspend::Setup => {
                        setup_then_pause(&self.paths);
                        "setup finished".into()
                    }
                    Suspend::Editor(path) => crate::settings::edit_in_editor(&path).unwrap_or_else(|e| format!("error: {e:#}")),
                };
                *term = ratatui::init();
                self.refresh(true);
            }
            if last.elapsed() > Duration::from_secs(2) {
                self.refresh(false);
                last = Instant::now();
            }
        }
    }

    fn refresh(&mut self, repos: bool) {
        let jobs = self.db.recent_jobs(500).unwrap_or_default();
        let day_ago = crate::state::now() - 86400;
        let recent_failure = |j: &Job| j.status == JobStatus::Failed && j.updated_at > day_ago;
        self.queue = jobs.iter().filter(|j| j.status.needs_user() || j.status.is_active() || recent_failure(j)).cloned().collect();
        // Needs-you first, then today's failures, then running.
        self.queue.sort_by_key(|j| (!j.status.needs_user(), j.status != JobStatus::Failed, j.id));
        if let Some(id) = self.detail {
            self.detail_text = self.db.job(id).ok().flatten().map_or_else(|| "job not found".into(), |j| job_text(&self.db, &j));
        } else if let Some(r) = &self.repo_view {
            self.detail_text = Config::load(&self.paths)
                .and_then(|cfg| {
                    let clone = crate::repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts).remove(r);
                    crate::settings::repo_text(&self.paths, &cfg, r, clone.as_deref())
                })
                .unwrap_or_else(|e| format!("error: {e:#}"));
        }
        self.history_all = jobs;
        self.history.clone_from(&self.history_all);
        self.inbox = self.db.events_with(Disposition::Inbox).unwrap_or_default().into_iter().map(|(e, _)| e).collect();
        self.paused = actions::paused(&self.db);
        self.daemon_up = self.paths.daemon_running();
        if repos {
            (self.settings_all, self.settings_err) = match crate::settings::rows(&self.paths) {
                Ok(r) => (r, String::new()),
                Err(e) => (vec![], format!("{e:#}")),
            };
            self.repos_all = match Config::load(&self.paths) {
                Ok(cfg) => crate::repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts)
                    .into_keys()
                    .map(|id| match cfg.base_settings_for(&self.paths, &id) {
                        Ok(s) => RepoRow {
                            subscribed: s.subscribed,
                            level: s.level.as_str().into(),
                            agent: s.agent.label().into(),
                            window: window_note(&s, &s.clone().at(&jiff::Zoned::now())),
                            id,
                        },
                        Err(e) => RepoRow { subscribed: false, level: "ERROR".into(), agent: format!("{e:#}"), window: String::new(), id },
                    })
                    .collect(),
                Err(e) => {
                    self.msg = format!("config error: {e:#}");
                    vec![]
                }
            };
            // Watched repos first; the scan is already sorted by name.
            self.repos_all.sort_by_key(|r| !r.subscribed);
        }
        self.apply_filter();
        let n = self.len();
        if self.sel.selected().is_some_and(|i| i >= n) {
            self.sel.select(Some(n.saturating_sub(1)));
        }
    }

    /// Case-insensitive substring match on the repo path or the setting key.
    fn apply_filter(&mut self) {
        let f = self.filter.to_lowercase();
        self.repos =
            self.repos_all.iter().filter(|r| format!("{}/{}", r.id.host, r.id.path).to_lowercase().contains(&f)).cloned().collect();
        self.settings = self.settings_all.iter().filter(|r| r.setting.key.contains(&f)).cloned().collect();
        self.history = self.history_all.iter().filter(|j| job_line(j).to_lowercase().contains(&f)).cloned().collect();
        if self.sel.selected().is_some_and(|i| i >= self.len()) {
            self.sel.select(Some(0));
        }
    }

    fn filter_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char(c) => self.filter.push(c),
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Esc => {
                self.filter.clear();
                self.filtering = false;
            }
            KeyCode::Enter | KeyCode::Down | KeyCode::Up => self.filtering = false,
            _ => return,
        }
        self.apply_filter();
    }

    fn len(&self) -> usize {
        match self.tab {
            Tab::Queue => self.queue.len(),
            Tab::Inbox => self.inbox.len(),
            Tab::History => self.history.len(),
            Tab::Repos => self.repos.len(),
            Tab::Settings => self.settings.len(),
            Tab::Help => 0,
        }
    }

    fn selected_job(&self) -> Option<&Job> {
        let i = self.sel.selected()?;
        match self.tab {
            Tab::Queue => self.queue.get(i),
            Tab::History => self.history.get(i),
            _ => None,
        }
    }

    fn style(&self, s: Style) -> Style {
        if self.color { s } else { Style::default().add_modifier(s.add_modifier) }
    }

    /// Returns false to quit.
    fn key(&mut self, k: KeyEvent) -> bool {
        if let Some((_, f)) = self.confirm.take() {
            if matches!(k.code, KeyCode::Char('y' | 'Y')) {
                self.msg = f(self).unwrap_or_else(|e| format!("error: {e:#}"));
                self.refresh(true);
            } else {
                self.msg = "cancelled".into();
            }
            return true;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return false;
        }
        if self.input.is_some() {
            self.input_key(k);
            return true;
        }
        if self.filtering {
            self.filter_key(k);
            return true;
        }
        if self.detail.is_some() || self.repo_view.is_some() {
            self.detail_key(k);
            return true;
        }
        match k.code {
            // Esc first clears a search, then quits.
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.apply_filter();
            }
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Char('1') => self.switch(Tab::Queue),
            KeyCode::Char('2') => self.switch(Tab::Inbox),
            KeyCode::Char('3') => self.switch(Tab::History),
            KeyCode::Char('4') => self.switch(Tab::Repos),
            KeyCode::Char('5') => self.switch(Tab::Settings),
            KeyCode::Char('?') | KeyCode::F(1) => self.switch(Tab::Help),
            KeyCode::Tab | KeyCode::Right => {
                let i = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
                self.switch(TABS[(i + 1) % TABS.len()].0);
            }
            KeyCode::BackTab | KeyCode::Left => {
                let i = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
                self.switch(TABS[(i + TABS.len() - 1) % TABS.len()].0);
            }
            KeyCode::Down | KeyCode::Char('j') if self.tab == Tab::Help => {
                self.scroll = (self.scroll + 1).min(u16::try_from(HELP.lines().count()).unwrap_or(0));
            }
            KeyCode::Up | KeyCode::Char('k') if self.tab == Tab::Help => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel.select(Some((self.sel.selected().unwrap_or(0) + 1).min(self.len().saturating_sub(1))));
            }
            KeyCode::Up | KeyCode::Char('k') => self.sel.select(Some(self.sel.selected().unwrap_or(0).saturating_sub(1))),
            KeyCode::Home | KeyCode::Char('g') => self.sel.select(Some(0)),
            KeyCode::PageDown => self.sel.select(Some((self.sel.selected().unwrap_or(0) + 10).min(self.len().saturating_sub(1)))),
            KeyCode::PageUp => self.sel.select(Some(self.sel.selected().unwrap_or(0).saturating_sub(10))),
            KeyCode::Char('/') if matches!(self.tab, Tab::Repos | Tab::Settings | Tab::History) => {
                self.filtering = true;
                self.sel.select(Some(0));
            }

            KeyCode::End | KeyCode::Char('G') => self.sel.select(Some(self.len().saturating_sub(1))),
            KeyCode::Char('p') => {
                let p = !self.paused;
                self.msg = match actions::set_paused(&self.db, p) {
                    Ok(()) if p => "paused: no new jobs will start".into(),
                    Ok(()) => "resumed".into(),
                    Err(e) => format!("error: {e:#}"),
                };
                self.paused = p;
            }
            KeyCode::Char('S') => self.suspend = Some(Suspend::Setup),
            KeyCode::Char('D') => self.toggle_daemon(),
            KeyCode::Char('R') if self.tab == Tab::Repos => {
                self.refresh(true);
                self.msg = "rescanned".into();
            }
            KeyCode::Enter if self.tab == Tab::Repos => {
                if let Some(r) = self.sel.selected().and_then(|i| self.repos.get(i)) {
                    self.repo_view = Some(r.id.clone());
                    self.refresh(false);
                }
            }
            KeyCode::Enter if matches!(self.tab, Tab::Queue | Tab::History) => {
                if let Some(j) = self.selected_job() {
                    self.detail = Some(j.id);
                    self.refresh(false);
                }
            }
            _ => match self.tab {
                Tab::Queue | Tab::History => self.job_key(k),
                Tab::Inbox => self.inbox_key(k),
                Tab::Repos => self.repo_key(k),
                Tab::Settings => self.settings_key(k),
                Tab::Help => {}
            },
        }
        true
    }

    /// Keys in the job or repo detail pane.
    fn detail_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                self.detail = None;
                self.repo_view = None;
                self.scroll = 0;
            }
            KeyCode::Char('e') if self.repo_view.is_some() => {
                self.suspend = self.repo_view.as_ref().map(|r| Suspend::Editor(self.paths.repo_file(r)));
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1).min(self.max_scroll()),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = self.scroll.saturating_add(20).min(self.max_scroll()),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(20),
            _ if self.detail.is_some() => self.job_key(k),
            _ => {}
        }
    }

    /// D: start the daemon, or stop it after asking.
    fn toggle_daemon(&mut self) {
        if self.daemon_up {
            self.confirm = Some((
                "Stop Night Porter? Nothing is watched until you start it again (D). y/N".into(),
                Box::new(|a: &mut App| crate::service::stop(&a.paths)),
            ));
        } else {
            self.msg = crate::service::start(&self.paths).unwrap_or_else(|e| format!("error: {e:#}"));
            self.refresh(false);
        }
    }

    fn max_scroll(&self) -> u16 {
        u16::try_from(self.detail_text.lines().count().saturating_sub(5)).unwrap_or(u16::MAX)
    }

    fn switch(&mut self, t: Tab) {
        self.tab = t;
        self.scroll = 0;
        // A search belongs to one list.
        if !self.filter.is_empty() || self.filtering {
            self.filter.clear();
            self.filtering = false;
            self.apply_filter();
        }
        self.sel.select(Some(0));
        if t == Tab::Repos {
            self.refresh(true);
        }
    }

    fn current_job_id(&self) -> Option<i64> {
        self.detail.or_else(|| self.selected_job().map(|j| j.id))
    }

    fn job_key(&mut self, k: KeyEvent) {
        // u still works with an empty list: it brings back the last dismissed job.
        let Some(id) = self.current_job_id().or_else(|| self.undo_jobs.last().copied()) else { return };
        let r = match k.code {
            KeyCode::Char('a') => actions::approve(&self.db, id, false).map(String::from),
            KeyCode::Char('A') => {
                self.confirm = Some((
                    format!("Override blocking checks for job {id}? y/N"),
                    Box::new(move |a: &mut App| actions::approve(&a.db, id, true).map(String::from)),
                ));
                return;
            }
            KeyCode::Char('r') => {
                self.confirm = Some((
                    format!("Reject job {id}? Nothing is pushed or posted. y/N"),
                    Box::new(move |a: &mut App| actions::reject(&a.db, id).map(|()| format!("job {id} rejected"))),
                ));
                return;
            }
            KeyCode::Char('t') => actions::retry(&self.db, id).map(|n| format!("re-queued {n} event(s)")),
            KeyCode::Char('x') => actions::dismiss(&self.db, id).map(|()| {
                self.undo_jobs.push(id);
                self.detail = None;
                format!("job {id} dismissed: nothing is pushed or posted. u undoes; History keeps it")
            }),
            KeyCode::Char('u') => {
                let selected_dismissed = self.db.job(id).ok().flatten().is_some_and(|j| j.status == JobStatus::Dismissed);
                match if selected_dismissed { Some(id) } else { self.undo_jobs.pop() } {
                    Some(u) => {
                        self.undo_jobs.retain(|x| *x != u);
                        actions::restore(&self.db, u).map(|s| format!("job {u} restored: {}", s.label()))
                    }
                    None => Ok("nothing to undo".into()),
                }
            }
            KeyCode::Char('X') => {
                self.confirm = Some((
                    format!("Delete job {id} and its log for good? Can't be undone. y/N"),
                    Box::new(move |a: &mut App| {
                        a.detail = None;
                        actions::purge(&mut a.db, id).map(|()| format!("job {id} deleted"))
                    }),
                ));
                return;
            }
            KeyCode::Char('h') => match self.db.job(id) {
                Ok(Some(j)) => crate::handoff::take_over(&self.paths, &self.db, &j),
                _ => Ok("job not found".into()),
            },
            KeyCode::Char('c') => {
                let cmd = self.db.job(id).ok().flatten().map(|j| crate::job::manual_commands(&j)).unwrap_or_default();
                if cmd.is_empty() { Ok("nothing to push for this job".into()) } else { copy(&cmd).map(|()| format!("copied: {cmd}")) }
            }
            KeyCode::Char('y') => {
                let text = self
                    .db
                    .job(id)
                    .ok()
                    .flatten()
                    .map(|j| j.replies.iter().map(|r| r.body.clone()).collect::<Vec<_>>().join("\n\n---\n\n"))
                    .unwrap_or_default();
                copy(&text).map(|()| "replies copied".into())
            }
            _ => return,
        };
        self.msg = r.unwrap_or_else(|e| format!("error: {e:#}"));
        self.refresh(false);
    }

    fn inbox_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Char('u') {
            self.msg = match self.undo_events.pop() {
                Some(id) => actions::unignore(&self.db, &id).map_or_else(|e| format!("error: {e:#}"), |()| "back in the inbox".into()),
                None => "nothing to undo (`nightporter inbox list --ignored` shows older ones)".into(),
            };
            self.refresh(false);
            return;
        }
        let Some(e) = self.sel.selected().and_then(|i| self.inbox.get(i)).cloned() else { return };
        let r = match k.code {
            KeyCode::Char('o') => actions::allow_once(&self.db, &e.id).map(|()| "allowed once".into()),
            KeyCode::Char('i') => actions::ignore(&self.db, &e.id).map(|()| {
                self.undo_events.push(e.id.clone());
                "ignored (u undoes)".into()
            }),
            KeyCode::Char('a') => actions::allow_always(&self.paths, &self.db, &e.author, Scope::Repo(&e.repo))
                .map(|n| format!("@{} trusted on {} ({n} released)", e.author, e.repo)),
            KeyCode::Char('A') => {
                let (author, paths) = (e.author.clone(), self.paths.clone());
                self.confirm = Some((
                    format!("Trust @{author} on ALL repos? y/N"),
                    Box::new(move |a: &mut App| {
                        actions::allow_always(&paths, &a.db, &author, Scope::Global)
                            .map(|n| format!("@{author} trusted everywhere ({n} released)"))
                    }),
                ));
                return;
            }
            KeyCode::Char('d') => {
                let (author, repo, paths) = (e.author.clone(), e.repo.clone(), self.paths.clone());
                self.confirm = Some((
                    format!("Deny @{author} on {repo}? y/N"),
                    Box::new(move |a: &mut App| {
                        actions::deny(&paths, &a.db, &author, Scope::Repo(&repo))
                            .map(|n| format!("@{author} denied on {repo} ({n} dropped)"))
                    }),
                ));
                return;
            }
            _ => return,
        };
        self.msg = r.unwrap_or_else(|e| format!("error: {e:#}"));
        self.refresh(false);
    }

    fn repo_key(&mut self, k: KeyEvent) {
        let Some(r) = self.sel.selected().and_then(|i| self.repos.get(i)) else { return };
        let (id, path) = (r.id.clone(), self.paths.repo_file(&r.id));
        let set = move |key: &'static str, val: &'static str| {
            crate::config::edit_file(&path, |d| d[key] = toml_edit::value(val)).map(|()| format!("{} {key} = {val}", id.path))
        };
        let r = match k.code {
            KeyCode::Char(' ' | 's') => {
                let on = !r.subscribed;
                actions::set_subscribed(&self.paths, &r.id, on)
                    .map(|()| format!("{} {}", if on { "subscribed to" } else { "unsubscribed from" }, r.id.path))
            }
            KeyCode::Char('l') => {
                let next = match r.level.as_str() {
                    "observe" => "suggest",
                    "suggest" => "draft",
                    "draft" => "auto",
                    _ => "observe",
                };
                if next == "auto" {
                    self.confirm = Some((
                        format!("Let {} push and reply on its own (auto)? y/N", r.id.path),
                        Box::new(move |_: &mut App| set("level", "auto")),
                    ));
                    return;
                }
                set("level", next)
            }
            KeyCode::Char('a') => {
                let now = AgentKind::ALL.into_iter().find(|a| a.label() == r.agent).unwrap_or(AgentKind::Claude);
                set("agent", now.next().bin())
            }
            KeyCode::Char('e') => {
                self.suspend = Some(Suspend::Editor(self.paths.repo_file(&r.id)));
                return;
            }
            _ => return,
        };
        self.msg = r.unwrap_or_else(|e| format!("error: {e:#}"));
        self.refresh(true);
    }

    fn settings_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Char('e') {
            self.suspend = Some(Suspend::Editor(self.paths.config_file()));
            return;
        }
        let Some(row) = self.sel.selected().and_then(|i| self.settings.get(i)) else { return };
        let s = row.setting;
        let r = match k.code {
            KeyCode::Char('d') | KeyCode::Delete => crate::settings::set(&self.paths, s, ""),
            KeyCode::Enter | KeyCode::Char(' ') => match crate::settings::next(s, &row.value) {
                Some(v) if s.confirm == Some(v) => {
                    self.confirm =
                        Some((format!("Set {} = {v}? y/N", s.key), Box::new(move |a: &mut App| crate::settings::set(&a.paths, s, v))));
                    return;
                }
                Some(v) => crate::settings::set(&self.paths, s, v),
                None if s.kind == crate::settings::Kind::Info => Ok(s.help.into()),
                None => {
                    self.input = Some(row.value.clone());
                    return;
                }
            },
            _ => return,
        };
        self.msg = r.unwrap_or_else(|e| format!("not saved: {e:#}"));
        self.refresh(true);
    }

    /// Typing a value for the selected setting. Enter saves (if valid), Esc cancels.
    fn input_key(&mut self, k: KeyEvent) {
        let Some(buf) = self.input.as_mut() else { return };
        match k.code {
            KeyCode::Char(c) => buf.push(c),
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Esc => {
                self.input = None;
                self.msg = "cancelled".into();
            }
            KeyCode::Enter => {
                let Some(s) = self.sel.selected().and_then(|i| self.settings.get(i)).map(|r| r.setting) else { return };
                match crate::settings::set(&self.paths, s, buf) {
                    Ok(m) => {
                        self.input = None;
                        self.msg = m;
                        self.refresh(true);
                    }
                    // Keep the text so it can be fixed.
                    Err(e) => self.msg = format!("not saved: {e:#}"),
                }
            }
            _ => {}
        }
    }

    /// Top line: daemon, pause, and what's waiting, coloured by urgency (and always in words).
    fn header(&self) -> Line<'static> {
        let needs = self.queue.iter().filter(|j| j.status.needs_user()).count();
        let failed = self.queue.iter().filter(|j| j.status == JobStatus::Failed).count();
        let (good, warn, bad, dim) = (
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            Style::new().add_modifier(Modifier::DIM),
        );
        let count = |n: usize, s: Style| self.style(if n > 0 { s } else { dim });
        let mut status = vec![
            Span::styled(" Night Porter ", self.style(Style::new().fg(Color::Black).bg(Color::Magenta).add_modifier(Modifier::BOLD))),
            Span::raw(" daemon "),
            if self.daemon_up {
                Span::styled("running", self.style(good))
            } else {
                Span::styled("NOT RUNNING (D starts it)", self.style(bad))
            },
            Span::raw(" · "),
            if self.paused { Span::styled("PAUSED", self.style(warn)) } else { Span::styled("active", self.style(good)) },
            Span::raw(" · "),
            Span::styled(format!("{needs} need you"), count(needs, warn)),
            Span::raw(" · "),
            Span::styled(format!("{failed} failed today"), count(failed, bad)),
            Span::raw(" · "),
            Span::styled(format!("{} in inbox", self.inbox.len()), count(self.inbox.len(), warn)),
        ];
        if self.update.is_some() {
            status.extend([Span::raw(" · "), Span::styled("update available", self.style(Style::new().fg(Color::Cyan)))]);
        }
        Line::from(status)
    }

    fn draw(&mut self, f: &mut Frame) {
        let [head, tabs, body, foot] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(3), Constraint::Length(2)]).areas(f.area());
        f.render_widget(Paragraph::new(self.header()), head);
        let titles: Vec<String> = TABS.iter().map(|(t, n)| if *t == self.tab { format!("[{n}]") } else { n.to_string() }).collect();
        let idx = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        f.render_widget(
            Tabs::new(titles)
                .select(idx)
                .style(self.style(Style::new().fg(Color::Gray)))
                .highlight_style(self.style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::REVERSED))),
            tabs,
        );

        let detail_title = match (&self.detail, &self.repo_view) {
            (Some(id), _) => Some(format!(" Job {id} — Esc back ")),
            (_, Some(r)) => Some(format!(" {r} — e edit its file, Esc back ")),
            _ => None,
        };
        if let Some(title) = detail_title {
            let p = Paragraph::new(self.detail_text.as_str())
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0))
                .block(Block::default().borders(Borders::ALL).border_style(self.style(Style::new().fg(Color::Cyan))).title(title));
            f.render_widget(p, body);
        } else if self.tab == Tab::Settings {
            self.draw_settings(f, body);
        } else if self.tab == Tab::Help {
            f.render_widget(
                Paragraph::new(HELP)
                    .wrap(Wrap { trim: false })
                    .scroll((self.scroll, 0))
                    .block(Block::default().borders(Borders::ALL).title(" Help (↑↓ scroll) ")),
                body,
            );
        } else {
            let items: Vec<ListItem> = match self.tab {
                Tab::Queue | Tab::History => {
                    let list = if self.tab == Tab::Queue { &self.queue } else { &self.history };
                    list.iter().map(|j| ListItem::new(job_line(j)).style(self.style(status_style(j.status)))).collect()
                }
                Tab::Inbox => self
                    .inbox
                    .iter()
                    .map(|e| {
                        ListItem::new(format!(
                            "@{:<18} {:?} on {}#{}: {}",
                            e.author,
                            e.role,
                            e.repo,
                            e.number,
                            e.body.lines().next().unwrap_or_default()
                        ))
                    })
                    .collect(),
                Tab::Repos => self.repo_items(),
                Tab::Settings | Tab::Help => vec![],
            };
            let empty = items.is_empty();
            let title = match self.tab {
                Tab::Repos if !self.filter.is_empty() || self.filtering => {
                    &format!(" Repos matching \"{}\" ({} of {}) ", self.filter, self.repos.len(), self.repos_all.len())
                }
                Tab::History if !self.filter.is_empty() || self.filtering => {
                    &format!(" History matching \"{}\" ({} of {}) ", self.filter, self.history.len(), self.history_all.len())
                }
                Tab::Queue => " Needs you / running (x dismiss what you don't want to act on) ",
                Tab::Inbox => " Unknown authors waiting ",
                Tab::History => " History: every job, newest first (/ search, e.g. dismissed or a repo) ",
                Tab::Repos => {
                    let on = self.repos_all.iter().filter(|r| r.subscribed).count();
                    &format!(" Repos: {on} watched of {} (Enter details, space subscribe, / search) ", self.repos_all.len())
                }
                Tab::Settings | Tab::Help => "",
            };
            if empty {
                let msg = match self.tab {
                    Tab::Queue => "Nothing needs you. Jobs show up here when your PRs get activity.",
                    Tab::Inbox => "No one is waiting.",
                    Tab::Repos | Tab::History if !self.filter.is_empty() => "Nothing matches the search. Esc clears it.",
                    Tab::Repos => "No clones found. Press S to run the setup wizard (forges, clone folder, repos).",
                    _ => "Nothing yet.",
                };
                f.render_widget(Paragraph::new(msg).block(Block::default().borders(Borders::ALL).title(title)), body);
            } else {
                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).border_style(self.style(Style::new().fg(Color::DarkGray))).title(title))
                    .highlight_symbol("> ")
                    .highlight_style(self.style(Style::new().add_modifier(Modifier::REVERSED)));
                f.render_stateful_widget(list, body, &mut self.sel);
            }
        }
        let (keys, line2) = self.footer();
        f.render_widget(Paragraph::new(vec![Line::from(keys), Line::from(line2)]).style(Style::new().add_modifier(Modifier::DIM)), foot);
    }
}

impl App {
    /// Footer: the keys for this view, then the question or last message.
    fn footer(&self) -> (&'static str, String) {
        let keys = match (self.detail.is_some(), self.tab) {
            // Kept under 80 columns.
            _ if self.repo_view.is_some() => "↑↓ scroll e edit this repo's file Esc back",
            (false, Tab::Repos | Tab::Settings | Tab::History) if self.filtering => "type to search, Enter keep results, Esc clear search",
            (true, _) => "↑↓ scroll a approve r reject x dismiss u undo t retry h take over y copy Esc",
            (_, Tab::Queue) => "Enter open a approve r reject x dismiss u undo t retry h take over ? q quit",
            (_, Tab::History) => "Enter open x dismiss u restore X delete t retry / search ? q quit",
            (_, Tab::Inbox) => "o once a trust A trust-all i ignore u undo d deny ? help q quit",
            (_, Tab::Repos) => "Enter details space subscribe l level a agent e edit / search ? q",
            (_, Tab::Settings) if self.input.is_some() => "type the value, Enter save, Esc cancel (empty = default)",
            (_, Tab::Settings) => "Enter change d default e edit file / search S setup ? q",
            (_, Tab::Help) => "1-5 tabs S setup wizard D start/stop daemon q quit",
        };
        let line2 = match (&self.confirm, &self.input) {
            (Some((q, _)), _) => q.clone(),
            (_, Some(buf)) => {
                let key = self.sel.selected().and_then(|i| self.settings.get(i)).map_or("", |r| r.setting.key);
                format!("{key} = {buf}_   {}", self.msg)
            }
            _ if self.filtering => format!("search: {}_", self.filter),
            _ => self.msg.clone(),
        };
        (keys, line2)
    }

    fn repo_items(&self) -> Vec<ListItem<'static>> {
        // Column as wide as the longest visible name, so levels line up.
        let w = self.repos.iter().map(|r| r.id.host.len() + r.id.path.len() + 1).max().unwrap_or(0);
        self.repos
            .iter()
            .map(|r| {
                let name = Span::styled(
                    format!("{:<w$} ", format!("{}/{}", r.id.host, r.id.path)),
                    if r.subscribed { Style::new().add_modifier(Modifier::BOLD) } else { Style::new().add_modifier(Modifier::DIM) },
                );
                ListItem::new(Line::from(vec![
                    if r.subscribed {
                        Span::styled("[x] ", self.style(Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)))
                    } else {
                        Span::styled("[ ] ", self.style(Style::new().add_modifier(Modifier::DIM)))
                    },
                    name,
                    Span::styled(format!("{:<8} ", r.level), self.style(level_style(&r.level))),
                    Span::styled(r.agent.clone(), self.style(Style::new().fg(Color::Magenta))),
                    Span::styled(
                        if r.window.is_empty() { String::new() } else { format!("  ({})", r.window) },
                        Style::new().add_modifier(Modifier::DIM),
                    ),
                ]))
            })
            .collect()
    }

    fn draw_settings(&mut self, f: &mut Frame, body: Rect) {
        let title = if self.filter.is_empty() && !self.filtering {
            format!(" Settings: {} (each repo file can override defaults.*) ", self.paths.config_file().display())
        } else {
            format!(" Settings matching \"{}\" ({} of {}) ", self.filter, self.settings.len(), self.settings_all.len())
        };
        if !self.settings_err.is_empty() {
            let text = format!(
                "config.toml doesn't load, so Night Porter is using the last good config:\n\n  {}\n\nPress e to fix it in your editor. Your edit is checked before it's saved.",
                self.settings_err
            );
            f.render_widget(
                Paragraph::new(text).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(title)),
                body,
            );
            return;
        }
        let [list_area, help_area] = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(body);
        let items: Vec<ListItem> = self
            .settings
            .iter()
            .map(|r| {
                let value = if r.value.is_empty() { "(none)" } else { r.value.as_str() };
                let shown = match (r.setting.kind, value) {
                    (crate::settings::Kind::Bool, "true") => "[x] on".to_string(),
                    (crate::settings::Kind::Bool, _) => "[ ] off".to_string(),
                    _ => value.to_string(),
                };
                let source = if r.set {
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().add_modifier(Modifier::DIM)
                };
                let value_style = match (r.setting.kind, value) {
                    (crate::settings::Kind::Bool, "true") => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                    (crate::settings::Kind::Bool, _) => Style::new().fg(Color::Red),
                    (_, "(none)") => Style::new().add_modifier(Modifier::DIM),
                    (crate::settings::Kind::Choice(_), v) if level_style(v) != Style::new() => level_style(v),
                    _ => Style::new().fg(Color::White),
                };
                // Global keys and per-repo defaults in different colours, so the two halves read apart.
                let key_style = if r.setting.key.starts_with("defaults.") { Color::Cyan } else { Color::Blue };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<34} ", r.setting.key), self.style(Style::new().fg(key_style))),
                    Span::styled(format!("{:<9} ", if r.set { "[set]" } else { "[default]" }), self.style(source)),
                    Span::styled(shown, self.style(value_style)),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_symbol("> ")
            .highlight_style(self.style(Style::new().add_modifier(Modifier::REVERSED)));
        f.render_stateful_widget(list, list_area, &mut self.sel);
        let help = self.sel.selected().and_then(|i| self.settings.get(i)).map_or("", |r| r.setting.help);
        f.render_widget(Paragraph::new(help).wrap(Wrap { trim: true }).block(Block::default().borders(Borders::TOP)), help_area);
    }
}

/// Riskier levels stand out; the level is always written out too, so colour is only a hint.
/// What the open schedule window changes about `base`, e.g. `now auto, from window`; empty if nothing.
fn window_note(base: &crate::config::RepoSettings, now: &crate::config::RepoSettings) -> String {
    let mut changed = vec![];
    if now.level != base.level {
        changed.push(now.level.as_str().to_string());
    }
    if now.agent != base.agent {
        changed.push(now.agent.label().to_string());
    }
    if changed.is_empty() { String::new() } else { format!("now {}, from window", changed.join(", ")) }
}

fn level_style(level: &str) -> Style {
    match level {
        "auto" => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        "draft" => Style::new().fg(Color::Yellow),
        "suggest" => Style::new().fg(Color::Cyan),
        "observe" => Style::new().add_modifier(Modifier::DIM),
        "ERROR" => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD | Modifier::REVERSED),
        _ => Style::new(),
    }
}

fn status_style(s: JobStatus) -> Style {
    match s {
        JobStatus::AwaitingApproval | JobStatus::AwaitingRunApproval => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        JobStatus::Failed => Style::new().fg(Color::Red),
        JobStatus::Ready => Style::new().fg(Color::Cyan),
        JobStatus::Done => Style::new().fg(Color::Green),
        JobStatus::Running | JobStatus::Approved => Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
        JobStatus::Dismissed | JobStatus::Rejected => Style::new().add_modifier(Modifier::DIM),
        JobStatus::Queued => Style::new(),
    }
}

fn job_line(j: &Job) -> String {
    let when = jiff::Timestamp::from_second(j.updated_at).map(|t| t.strftime("%m-%d %H:%M").to_string()).unwrap_or_default();
    format!(
        "#{:<4} {:<20} {} {}#{:<5} {:<8} {}",
        j.id,
        j.status.label(),
        when,
        j.repo,
        j.number,
        j.level,
        j.summary.lines().next().unwrap_or_default()
    )
}

/// Full text of a job, used by the TUI detail view and `nightporter show`.
pub fn job_text(db: &Db, j: &Job) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let when = |t: i64| jiff::Timestamp::from_second(t).map(|t| t.strftime("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
    let _ = writeln!(
        s,
        "Job {} — {} — {}#{} ({}) — level {} — attempt {}",
        j.id,
        j.status.label(),
        j.repo,
        j.number,
        j.mode,
        j.level,
        j.attempt
    );
    let _ = writeln!(s, "Created {}, updated {}", when(j.created_at), when(j.updated_at));
    if !j.summary.is_empty() {
        let _ = writeln!(s, "\nSummary: {}", j.summary);
    }
    if !j.error.is_empty() {
        let _ = writeln!(s, "\nError: {}", j.error);
    }
    if let Some(c) = j.meta.cost_usd {
        let _ = writeln!(s, "Cost: ${c:.2}");
    }
    if !j.meta.pr_url.is_empty() {
        let _ = writeln!(s, "Draft PR: {}", j.meta.pr_url);
    }
    if j.status == JobStatus::AwaitingRunApproval {
        let _ = writeln!(s, "\nThis job runs code from someone you don't trust. Build files changed vs base:");
        for f in &j.meta.build_files {
            let _ = writeln!(s, "  {f}");
        }
        if j.meta.build_files.is_empty() {
            let _ = writeln!(s, "  (none)");
        }
        let _ = writeln!(s, "Worktree to inspect: {}\nPress a to allow env setup and tests to run.", j.worktree);
    }
    let _ = writeln!(s, "\nRequests:");
    for e in db.job_events(j.id).unwrap_or_default() {
        let _ = writeln!(s, "  [{}] @{} {:?}: {}", e.id, e.author, e.trigger, e.body.lines().next().unwrap_or_default());
    }
    if !j.checks.is_empty() {
        let _ = writeln!(s, "\nChecks:");
        for c in &j.checks {
            let mark = if c.ok {
                "[ok]  "
            } else if c.blocking {
                "[BLOCK]"
            } else {
                "[ASK] "
            };
            let _ = writeln!(s, "  {mark} {} {}", c.name, c.detail.replace('\n', "\n          "));
        }
    }
    if !j.replies.is_empty() {
        let _ = writeln!(s, "\nReplies to post:");
        for r in &j.replies {
            let _ = writeln!(s, "  → {}\n    {}", r.event.as_deref().unwrap_or("(PR comment)"), r.body.replace('\n', "\n    "));
        }
    }
    let cmd = crate::job::manual_commands(j);
    if j.status == JobStatus::Ready && !cmd.is_empty() {
        let _ = writeln!(s, "\nTo push yourself:\n  {cmd}");
    }
    if !j.diff.is_empty() {
        let _ = writeln!(s, "\nDiff:\n{}", j.diff);
    }
    let log = db.job_log(j.id).unwrap_or_default();
    if !log.is_empty() {
        let _ = writeln!(s, "\nLog:");
        for (ts, l) in log {
            let t = jiff::Timestamp::from_second(ts).map(|t| t.strftime("%H:%M:%S").to_string()).unwrap_or_default();
            let _ = writeln!(s, "  {t} {l}");
        }
    }
    s
}

/// Copy to the system clipboard via the platform tool.
fn copy(text: &str) -> Result<()> {
    use std::io::Write;
    let candidates: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        &[&["wl-copy"], &["xclip", "-selection", "clipboard"], &["xsel", "-b", "-i"]]
    };
    for c in candidates {
        if let Ok(mut child) = std::process::Command::new(c[0]).args(&c[1..]).stdin(std::process::Stdio::piped()).spawn() {
            child.stdin.take().map(|mut i| i.write_all(text.as_bytes()));
            child.wait()?;
            return Ok(());
        }
    }
    anyhow::bail!("no clipboard tool found (pbcopy / wl-copy / xclip)")
}

const HELP: &str = "\
Night Porter watches your PRs/MRs and runs your coding agent on review feedback, CI failures
and merge conflicts, within your rules.

Tabs
  1 Queue    jobs that need you (NEEDS APPROVAL, ready to copy) and running jobs
  2 Inbox    comments from people you don't trust yet: allow once, trust, ignore or deny
  3 History  every job, newest first, including done and dismissed ones (/ searches)
  4 Repos    watched repos first; Enter shows everything about one (prompt, reply style, trust, hours)
  5 Settings every config.toml setting, its value and whether it's [set] or [default]

Keys (everywhere): ↑/↓ or j/k move, PgUp/PgDn page, Enter open, Tab/←/→ switch tab, p pause, q quit
       /  search the Repos, Settings or History list (Esc clears)
       S  run the setup wizard again (forges, agent, clone folder, level, hours, repos, service)
       D  start or stop the background daemon (the part that watches your PRs; asks before stopping)
Repos: Enter details   space subscribe/unsubscribe   l next level (observe → suggest → draft → auto)
       a  switch agent (Claude Code → OpenCode → Codex → Gemini CLI, experimental)   e edit the repo's file   R rescan
       l, a set the repo's own value; (now auto, from window) on a row: a schedule window overrides it
Settings: Enter change (switches on/off and choices, otherwise type a value)   d back to default
       e  edit config.toml in $EDITOR. Every change is checked first: an invalid value or file is
          never saved, so a typo can't break the daemon.
Jobs:  a approve   A approve despite blocking checks (asks first)   r reject   t retry
       x dismiss: put it away without replying or pushing (it stays in History)
       u undo a dismiss (or restore the selected dismissed job)   X delete for good (asks first)
       c copy the push command (suggest level)   y copy replies
       h take over in herdr, Rex, tmux, Zellij or a terminal: the job's worktree with a briefed agent
Inbox: o allow once   a trust author on this repo   A trust everywhere   i ignore   u undo ignore   d deny

Status words are always shown as text: NEEDS APPROVAL, NEEDS RUN APPROVAL, FAILED, ready to copy.
Checks: [ok] passed, [ASK] needs your approval, [BLOCK] blocking (secrets, protected paths, history).
Set NO_COLOR=1 to turn colours off. Everything here is also available as CLI commands:
`nightporter status`, `show <id>`, `approve <id>`, `dismiss <id>`, `repo <name>`, `stop`, ...

Turning it off: D here, or `nightporter stop`. If it starts at login and you don't want that,
`nightporter service uninstall`. Your repos and config are never touched by stopping it.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{ForgeKind, RepoId, Target};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn screen(app: &mut App) -> String {
        screen_at(app, 140, 30)
    }
    fn screen_at(app: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<Vec<_>>()
            .chunks(w as usize)
            .map(<[&str]>::concat)
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn press(app: &mut App, c: KeyCode) -> bool {
        app.key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    #[test]
    fn renders_every_view_with_text_labels() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        let mut db = Db::open(&paths.db()).unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&repo, Target::Pr, 4, "my_prs", "draft", &[]).unwrap();
        let mut j = db.job(id).unwrap().unwrap();
        j.status = JobStatus::AwaitingApproval;
        j.summary = "renamed foo".into();
        j.checks = vec![crate::state::CheckResult { name: "secrets".into(), ok: false, blocking: true, detail: "x".into() }];
        db.update_job(&j).unwrap();
        // SAFETY: test-only env; no concurrent readers of NO_COLOR in this module.
        unsafe { std::env::set_var("NO_COLOR", "1") };
        let mut app = App::new(&paths, db);
        unsafe { std::env::remove_var("NO_COLOR") };
        assert!(!app.color);
        app.refresh(true);

        let s = screen(&mut app);
        assert!(s.contains("NEEDS APPROVAL") && s.contains("renamed foo") && s.contains("1 need you"), "{s}");
        assert!(s.contains("daemon NOT RUNNING"));
        assert!(s.contains("> "), "selection marker must not rely on colour");

        assert!(press(&mut app, KeyCode::Enter));
        let s = screen(&mut app);
        assert!(s.contains("[BLOCK] secrets") && s.contains("Esc back"), "{s}");
        // Plain approve refuses a blocking failure; the override asks first.
        press(&mut app, KeyCode::Char('a'));
        assert!(screen(&mut app).contains("blocking check failures"));
        press(&mut app, KeyCode::Char('A'));
        let small = screen_at(&mut app, 80, 24);
        assert!(small.contains("Override blocking checks") && small.contains("y/N"), "prompt must fit 80 columns: {small}");
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.db.job(id).unwrap().unwrap().status, JobStatus::AwaitingApproval);
        press(&mut app, KeyCode::Esc);
        assert!(screen_at(&mut app, 80, 24).contains("q quit"), "footer must fit 80 columns");
        // Reject asks first.
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.db.job(id).unwrap().unwrap().status, JobStatus::AwaitingApproval);

        for (k, want) in [('2', "No one is waiting"), ('3', "History"), ('4', "No clones found"), ('?', "Night Porter watches your PRs")] {
            press(&mut app, KeyCode::Char(k));
            let s = screen(&mut app);
            assert!(s.contains(want), "tab {k}: {s}");
        }
        // Repos tab: l cycles the level in the repo file, auto asks first; a switches agent.
        press(&mut app, KeyCode::Char('4'));
        let row = |level: &str| {
            vec![RepoRow { id: repo.clone(), subscribed: false, level: level.into(), agent: "Claude Code".into(), window: String::new() }]
        };
        app.repos = row("draft");
        let level = |p: &Paths| std::fs::read_to_string(p.repo_file(&repo)).unwrap_or_default();
        press(&mut app, KeyCode::Char('l'));
        assert!(screen(&mut app).contains("(auto)? y/N"));
        press(&mut app, KeyCode::Char('n'));
        assert!(!level(&paths).contains("auto"));
        app.repos = row("auto");
        press(&mut app, KeyCode::Char('l'));
        assert!(level(&paths).contains("level = \"observe\""), "{}", level(&paths));
        app.repos = row("observe");
        press(&mut app, KeyCode::Char('a'));
        assert!(level(&paths).contains("agent = \"opencode\""), "{}", level(&paths));
        press(&mut app, KeyCode::Char('S'));
        assert!(matches!(app.suspend, Some(Suspend::Setup)), "S asks the main loop to run the wizard");
        app.suspend = None;

        // Settings tab: values with their source, typed input validated before saving.
        press(&mut app, KeyCode::Char('5'));
        let s = screen(&mut app);
        assert!(s.contains("poll_interval") && s.contains("[default]") && s.contains("Folder where your clones live"), "{s}");
        app.sel.select(app.settings.iter().position(|r| r.setting.key == "poll_interval"));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        for c in "soon".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app).contains("not saved") && app.input.is_some(), "bad value is refused, text kept");
        press(&mut app, KeyCode::Esc);
        app.sel.select(app.settings.iter().position(|r| r.setting.key == "defaults.sandbox"));
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app).contains("defaults.sandbox = false? y/N"), "turning the sandbox off asks first");
        press(&mut app, KeyCode::Char('n'));
        // `/` searches the list; Esc clears the search before it would quit.
        press(&mut app, KeyCode::Char('/'));
        for c in "sandbox".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(app.settings.iter().all(|r| r.setting.key.contains("sandbox")) && app.settings.len() == 3);
        assert!(screen(&mut app).contains("Settings matching \"sandbox\" (3 of"), "{}", screen(&mut app));
        assert!(screen(&mut app).contains("[x] on"), "bools show as checkboxes");
        assert!(press(&mut app, KeyCode::Esc), "Esc clears the search instead of quitting");
        assert_eq!(app.settings.len(), app.settings_all.len());
        std::fs::write(paths.config_file(), "nope = 1\n").unwrap();
        app.refresh(true);
        assert!(screen(&mut app).contains("Press e to fix it"));
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.suspend, Some(Suspend::Editor(_))));
        app.suspend = None;

        press(&mut app, KeyCode::Char('p'));
        assert!(screen(&mut app).contains("PAUSED"));
        assert!(!press(&mut app, KeyCode::Char('q')));
    }

    #[test]
    fn repos_tab_shows_the_repo_level_and_notes_the_window() {
        let cfg = crate::config::Config::parse(
            "[defaults]\nlevel = \"draft\"\n[[defaults.schedule]]\ndays = \"daily\"\nwindows = [\"00:00-24:00\"]\nlevel = \"auto\"",
        )
        .unwrap();
        let base = cfg.resolve(None).unwrap();
        let now = base.clone().at(&jiff::Zoned::now());
        assert_eq!(window_note(&base, &now), "now auto, from window");
        assert_eq!(window_note(&base, &base), "");
        // The row shows the repo's own level, so `l` visibly moves it (draft -> auto asks first).
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        let mut app = App::new(&paths, Db::open(&paths.db()).unwrap());
        press(&mut app, KeyCode::Char('4'));
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let window = window_note(&base, &now);
        app.repos = vec![RepoRow { id: repo, subscribed: true, level: base.level.as_str().into(), agent: "Claude Code".into(), window }];
        let s = screen(&mut app);
        assert!(s.contains("draft") && s.contains("(now auto, from window)"), "{s}");
        press(&mut app, KeyCode::Char('l'));
        assert!(screen(&mut app).contains("(auto)? y/N"));
    }

    #[test]
    fn dismiss_undo_purge_repo_view_and_daemon_toggle() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::at(&d.path().join("c"), &d.path().join("d"));
        paths.ensure().unwrap();
        let mut db = Db::open(&paths.db()).unwrap();
        let repo = RepoId::new(ForgeKind::GitHub, "github.com", "o/r");
        let id = db.create_job(&repo, Target::Pr, 4, "my_prs", "draft", &[]).unwrap();
        db.set_status(id, JobStatus::AwaitingApproval).unwrap();
        let mut app = App::new(&paths, db);
        // Dismiss: gone from the Queue, nothing posted; u brings it back; History keeps it.
        app.refresh(true);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.db.job(id).unwrap().unwrap().status, JobStatus::Dismissed);
        assert!(screen(&mut app).contains("Nothing needs you"), "{}", screen(&mut app));
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.db.job(id).unwrap().unwrap().status, JobStatus::AwaitingApproval);
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('/'));
        for c in "dismissed".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app).contains("History matching \"dismissed\" (1 of 1)"), "{}", screen(&mut app));
        assert!(screen_at(&mut app, 80, 24).contains("q quit"), "History footer fits 80 columns");
        press(&mut app, KeyCode::Char('X'));
        press(&mut app, KeyCode::Char('y'));
        assert!(app.db.job(id).unwrap().is_none(), "X deletes for good after asking");
        press(&mut app, KeyCode::Esc);
        // Enter on a repo shows its settings.
        press(&mut app, KeyCode::Char('4'));
        app.repos =
            vec![RepoRow { id: repo.clone(), subscribed: true, level: "draft".into(), agent: "Codex".into(), window: String::new() }];
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app).contains("WHAT IT DOES") || screen(&mut app).contains("error"), "{}", screen(&mut app));
        press(&mut app, KeyCode::Esc);
        assert!(app.repo_view.is_none());
        // D on a stopped daemon tries to start it, so it isn't pressed here; it asks before stopping.
        app.daemon_up = true;
        press(&mut app, KeyCode::Char('D'));
        assert!(screen(&mut app).contains("Stop Night Porter?"));
        press(&mut app, KeyCode::Char('n'));
    }
}
