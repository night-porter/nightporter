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

struct RepoRow {
    id: crate::forge::RepoId,
    subscribed: bool,
    level: String,
    agent: String,
}

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
        }
        self.history = jobs;
        self.inbox = self.db.events_with(Disposition::Inbox).unwrap_or_default().into_iter().map(|(e, _)| e).collect();
        self.paused = actions::paused(&self.db);
        self.daemon_up = self.paths.daemon_running();
        if repos {
            (self.settings, self.settings_err) = match crate::settings::rows(&self.paths) {
                Ok(r) => (r, String::new()),
                Err(e) => (vec![], format!("{e:#}")),
            };
            self.repos = match Config::load(&self.paths) {
                Ok(cfg) => crate::repos::scan(&cfg.code_dir(), cfg.scan_depth, &cfg.accounts)
                    .into_keys()
                    .map(|id| match cfg.settings_for(&self.paths, &id) {
                        Ok(s) => RepoRow { subscribed: s.subscribed, level: s.level.as_str().into(), agent: s.agent.label().into(), id },
                        Err(e) => RepoRow { subscribed: false, level: "ERROR".into(), agent: format!("{e:#}"), id },
                    })
                    .collect(),
                Err(e) => {
                    self.msg = format!("config error: {e:#}");
                    vec![]
                }
            };
        }
        let n = self.len();
        if self.sel.selected().is_some_and(|i| i >= n) {
            self.sel.select(Some(n.saturating_sub(1)));
        }
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
        if self.detail.is_some() {
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                    self.detail = None;
                    self.scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1).min(self.max_scroll()),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = self.scroll.saturating_add(20).min(self.max_scroll()),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(20),
                _ => self.job_key(k),
            }
            return true;
        }
        match k.code {
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
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel.select(Some((self.sel.selected().unwrap_or(0) + 1).min(self.len().saturating_sub(1))));
            }
            KeyCode::Up | KeyCode::Char('k') => self.sel.select(Some(self.sel.selected().unwrap_or(0).saturating_sub(1))),
            KeyCode::Home | KeyCode::Char('g') => self.sel.select(Some(0)),
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
            KeyCode::Char('R') if self.tab == Tab::Repos => {
                self.refresh(true);
                self.msg = "rescanned".into();
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

    fn max_scroll(&self) -> u16 {
        u16::try_from(self.detail_text.lines().count().saturating_sub(5)).unwrap_or(u16::MAX)
    }

    fn switch(&mut self, t: Tab) {
        self.tab = t;
        self.sel.select(Some(0));
        if t == Tab::Repos {
            self.refresh(true);
        }
    }

    fn current_job_id(&self) -> Option<i64> {
        self.detail.or_else(|| self.selected_job().map(|j| j.id))
    }

    fn job_key(&mut self, k: KeyEvent) {
        let Some(id) = self.current_job_id() else { return };
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
        let Some(e) = self.sel.selected().and_then(|i| self.inbox.get(i)).cloned() else { return };
        let r = match k.code {
            KeyCode::Char('o') => actions::allow_once(&self.db, &e.id).map(|()| "allowed once".into()),
            KeyCode::Char('i') => actions::ignore(&self.db, &e.id).map(|()| "ignored".into()),
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
            KeyCode::Char(' ' | 's') | KeyCode::Enter => {
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
            KeyCode::Char('a') => set("agent", if r.agent == AgentKind::Claude.label() { "opencode" } else { "claude" }),
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

    fn draw(&mut self, f: &mut Frame) {
        let [head, tabs, body, foot] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(3), Constraint::Length(2)]).areas(f.area());
        let needs = self.queue.iter().filter(|j| j.status.needs_user()).count();
        let failed = self.queue.iter().filter(|j| j.status == JobStatus::Failed).count();
        let status = format!(
            " Night Porter — daemon {} — {} — {} need you — {} failed today — {} in inbox",
            if self.daemon_up { "running" } else { "NOT RUNNING" },
            if self.paused { "PAUSED" } else { "active" },
            needs,
            failed,
            self.inbox.len()
        );
        f.render_widget(Paragraph::new(status).style(self.style(Style::new().add_modifier(Modifier::BOLD))), head);
        let titles: Vec<String> = TABS.iter().map(|(t, n)| if *t == self.tab { format!("[{n}]") } else { n.to_string() }).collect();
        let idx = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        f.render_widget(Tabs::new(titles).select(idx).highlight_style(self.style(Style::new().add_modifier(Modifier::REVERSED))), tabs);

        if let Some(id) = self.detail {
            let p = Paragraph::new(self.detail_text.as_str())
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0))
                .block(Block::default().borders(Borders::ALL).title(format!(" Job {id} — Esc back ")));
            f.render_widget(p, body);
        } else if self.tab == Tab::Settings {
            self.draw_settings(f, body);
        } else if self.tab == Tab::Help {
            f.render_widget(
                Paragraph::new(HELP).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(" Help ")),
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
                Tab::Repos => self
                    .repos
                    .iter()
                    .map(|r| {
                        ListItem::new(format!(
                            "{} {:<50} {:<8} {}",
                            if r.subscribed { "[on] " } else { "[off]" },
                            format!("{}/{}", r.id.host, r.id.path),
                            r.level,
                            r.agent
                        ))
                    })
                    .collect(),
                Tab::Settings | Tab::Help => vec![],
            };
            let empty = items.is_empty();
            let title = match self.tab {
                Tab::Queue => " Needs you / running ",
                Tab::Inbox => " Unknown authors waiting ",
                Tab::History => " History ",
                Tab::Repos => " Repos (space subscribe, l level, a agent, e edit file, R rescan) ",
                Tab::Settings | Tab::Help => "",
            };
            if empty {
                let msg = match self.tab {
                    Tab::Queue => "Nothing needs you. Jobs show up here when your PRs get activity.",
                    Tab::Inbox => "No one is waiting.",
                    Tab::Repos => "No clones found. Press S to run the setup wizard (forges, clone folder, repos).",
                    _ => "Nothing yet.",
                };
                f.render_widget(Paragraph::new(msg).block(Block::default().borders(Borders::ALL).title(title)), body);
            } else {
                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(title))
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
            (true, _) => "↑↓ scroll a approve A override r reject t retry h take over c/y copy Esc",
            (_, Tab::Queue | Tab::History) => "↑↓ Enter open a approve r reject t retry h take over p pause ? q quit",
            (_, Tab::Inbox) => "o once a trust A trust-all i ignore d deny ? help q quit",
            (_, Tab::Repos) => "space subscribe l level a agent e edit S setup ? help q quit",
            (_, Tab::Settings) if self.input.is_some() => "type the value, Enter save, Esc cancel (empty = default)",
            (_, Tab::Settings) => "Enter change d default e edit file S setup ? help q quit",
            (_, Tab::Help) => "1-5 tabs S setup wizard q quit",
        };
        let line2 = match (&self.confirm, &self.input) {
            (Some((q, _)), _) => q.clone(),
            (_, Some(buf)) => {
                let key = self.sel.selected().and_then(|i| self.settings.get(i)).map_or("", |r| r.setting.key);
                format!("{key} = {buf}_   {}", self.msg)
            }
            _ => self.msg.clone(),
        };
        (keys, line2)
    }

    fn draw_settings(&mut self, f: &mut Frame, body: Rect) {
        let title = format!(" Settings: {} (each repo file can override defaults.*) ", self.paths.config_file().display());
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
                ListItem::new(format!("{:<34} {:<9} {value}", r.setting.key, if r.set { "[set]" } else { "[default]" }))
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

fn status_style(s: JobStatus) -> Style {
    match s {
        JobStatus::AwaitingApproval | JobStatus::AwaitingRunApproval => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        JobStatus::Failed => Style::new().fg(Color::Red),
        JobStatus::Ready => Style::new().fg(Color::Cyan),
        JobStatus::Done => Style::new().fg(Color::Green),
        _ => Style::new(),
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
  3 History  every job, newest first
  4 Repos    subscribe your local clones, set each one's level and agent
  5 Settings every config.toml setting, its value and whether it's [set] or [default]

Keys (everywhere): ↑/↓ or j/k move, Enter open, Tab/←/→ switch tab, p pause/resume, q quit
       S  run the setup wizard again (forges, agent, clone folder, level, hours, repos, service)
Repos: space subscribe/unsubscribe   l next level (observe → suggest → draft → auto, asks before auto)
       a  switch agent (Claude Code / OpenCode)   e edit the repo's file   R rescan
Settings: Enter change (switches on/off and choices, otherwise type a value)   d back to default
       e  edit config.toml in $EDITOR. Every change is checked first: an invalid value or file is
          never saved, so a typo can't break the daemon.
Jobs:  a approve   A approve despite blocking checks (asks first)   r reject   t retry
       c copy the push command (suggest level)   y copy replies
       h take over in herdr or Rex: opens the job's worktree with a briefed agent
Inbox: o allow once   a trust author on this repo   A trust everywhere   i ignore   d deny

Status words are always shown as text: NEEDS APPROVAL, NEEDS RUN APPROVAL, FAILED, ready to copy.
Checks: [ok] passed, [ASK] needs your approval, [BLOCK] blocking (secrets, protected paths, history).
Set NO_COLOR=1 to turn colours off. Everything here is also available as CLI commands:
`nightporter status`, `show <id>`, `approve <id>`, `inbox list`, ...";

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

        for (k, want) in
            [('2', "No one is waiting"), ('3', "History"), ('4', "No clones found"), ('?', "Status words are always shown as text")]
        {
            press(&mut app, KeyCode::Char(k));
            let s = screen(&mut app);
            assert!(s.contains(want), "tab {k}: {s}");
        }
        // Repos tab: l cycles the level in the repo file, auto asks first; a switches agent.
        press(&mut app, KeyCode::Char('4'));
        let row = |level: &str| vec![RepoRow { id: repo.clone(), subscribed: false, level: level.into(), agent: "Claude Code".into() }];
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
}
