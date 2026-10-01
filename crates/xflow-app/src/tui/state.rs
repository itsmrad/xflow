use super::{
    integrations::Provider,
    theme::{ColorMode, Theme, BUILTINS},
};
use crate::{
    config_edit::ConfigFile,
    gsettings::{Kind, Setting},
};
use anyhow::{anyhow, bail, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::{collections::VecDeque, path::PathBuf};
use xflow_core::{
    config::{Config, Replacement, Snippet, Style},
    ipc::{Delivery, HistoryEntry, Request, Response, Stats, Timings},
    CleanupMode, Mode, State,
};
use zeroize::Zeroizing;

pub const PAGE_SIZE: usize = 30;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    History,
    Dictionary,
    Snippets,
    Styles,
    Providers,
    Settings,
    Overlay,
    Stats,
    Doctor,
}
pub const PAGES: [Page; 10] = [
    Page::Home,
    Page::History,
    Page::Dictionary,
    Page::Snippets,
    Page::Styles,
    Page::Providers,
    Page::Settings,
    Page::Overlay,
    Page::Stats,
    Page::Doctor,
];
impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::History => "History",
            Self::Dictionary => "Dictionary",
            Self::Snippets => "Snippets",
            Self::Styles => "Styles",
            Self::Providers => "Providers",
            Self::Settings => "Settings",
            Self::Overlay => "Overlay",
            Self::Stats => "Stats",
            Self::Doctor => "Doctor",
        }
    }
    pub fn index(self) -> usize {
        PAGES.iter().position(|p| *p == self).unwrap_or(0)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Go(Page),
    Toggle,
    Command,
    Cancel,
    Last,
    Copy,
    Paste,
    Add,
    AddReplacement,
    Edit,
    Delete,
    Export,
    NextPage,
    PrevPage,
    Save,
    Revert,
    Unset,
    Theme,
    Help,
    Palette,
    Refresh,
    Search,
    ProviderKind,
    Model,
    Key,
    DeleteKey,
    CheckProvider,
    Devices,
    ScrollUp,
    ScrollDown,
    Quit,
}
#[derive(Clone)]
pub struct Command {
    pub action: Action,
    pub title: String,
    pub key: &'static str,
}
pub fn commands() -> Vec<Command> {
    let mut out: Vec<_> = PAGES
        .iter()
        .enumerate()
        .map(|(i, p)| Command {
            action: Action::Go(*p),
            title: format!("Go to {}", p.title()),
            key: [
                "1/h", "2/H", "3/d", "4/n", "5/y", "6/v", "7/s", "8/o", "9/a", "0/D",
            ][i],
        })
        .collect();
    for (action, title, key) in [
        (Action::Toggle, "Toggle dictation", "Space"),
        (Action::Command, "Start command mode", "C"),
        (Action::Cancel, "Cancel recording", "x"),
        (Action::Last, "Load last transcript", "l"),
        (Action::Copy, "Copy transcript", "c"),
        (Action::Paste, "Paste transcript", "p"),
        (Action::Add, "Add entry", "N"),
        (Action::AddReplacement, "Add replacement", "A"),
        (Action::Edit, "Edit or activate selection", "Enter"),
        (Action::Delete, "Delete selected entry", "Delete"),
        (
            Action::Export,
            "Export history selection / config draft",
            "e",
        ),
        (Action::NextPage, "Next history page", "PgDn"),
        (Action::PrevPage, "Previous history page", "PgUp"),
        (Action::Save, "Save config and reload daemon", "Ctrl+S"),
        (Action::Revert, "Revert unsaved edits", "Ctrl+R"),
        (Action::Unset, "Reset setting to default", "u"),
        (Action::Theme, "Switch theme", "t"),
        (Action::Help, "Show help", "?"),
        (Action::Palette, "Command palette", "Ctrl+P / :"),
        (Action::Refresh, "Refresh current screen", "r"),
        (Action::Search, "Search history", "/"),
        (Action::ProviderKind, "Switch STT / cleanup catalog", "b"),
        (Action::Model, "Choose provider model", "m"),
        (Action::Key, "Set provider key", "k"),
        (Action::DeleteKey, "Delete provider key", "K"),
        (Action::CheckProvider, "Check provider connection", "T"),
        (Action::Devices, "Choose microphone", "i"),
        (Action::ScrollUp, "Scroll detail up", "["),
        (Action::ScrollDown, "Scroll detail down", "]"),
        (Action::Quit, "Quit", "q"),
    ] {
        out.push(Command {
            action,
            title: title.into(),
            key,
        });
    }
    out
}
pub fn fuzzy(query: &str, text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut chars = lower.chars();
    query
        .to_lowercase()
        .chars()
        .all(|q| chars.by_ref().any(|c| c == q))
}
#[derive(Clone, Debug)]
pub enum Tag {
    Ordinary,
    History(u64),
    Detail(i64),
    Mutation,
    Stats,
    Activity,
    Reload,
}
pub enum Effect {
    Request(Request, Tag),
    Save(ConfigFile, Option<String>),
    Load(PathBuf),
    Overlay,
    OverlaySet(String, String),
    Providers(Config, bool),
    SaveKey(String, Zeroizing<String>),
    DeleteKey(String),
    CheckProvider(Config, bool),
    Devices,
    Export(PathBuf, HistoryEntry),
    ExportDraft(PathBuf, String),
    Doctor,
}
#[derive(Clone, Debug)]
pub enum EditTarget {
    Setting(String),
    Word(Option<usize>),
    Replacement(Option<usize>),
    Snippet(Option<usize>),
    Style(Option<usize>),
    Overlay(String),
    Model,
    Key(String),
    Export(HistoryEntry),
    Draft,
    Theme,
    Device,
}
pub struct Field {
    pub label: String,
    pub value: Zeroizing<String>,
    pub choices: Vec<String>,
    pub secret: bool,
    pub cursor: usize,
}
impl Field {
    fn new(label: &str, value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self {
            label: label.into(),
            value: Zeroizing::new(value),
            choices: vec![],
            secret: false,
            cursor,
        }
    }
    fn choices(label: &str, value: impl Into<String>, choices: Vec<String>) -> Self {
        Self {
            choices,
            ..Self::new(label, value)
        }
    }
    pub fn display(&self) -> String {
        if self.secret {
            "•".repeat(self.value.chars().count())
        } else {
            self.value.to_string()
        }
    }
    pub fn caret(&self) -> String {
        let mut text = self.display();
        let index = if self.secret {
            self.value[..self.cursor].chars().count() * '•'.len_utf8()
        } else {
            self.cursor
        };
        text.insert(index, '▏');
        text
    }
    fn left(&mut self) {
        self.cursor = self.value[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }
    fn right(&mut self) {
        if let Some(c) = self.value[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }
    fn insert(&mut self, c: char) {
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }
    fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.value.replace_range(self.cursor..end, "");
    }
    fn delete(&mut self) {
        let start = self.cursor;
        self.right();
        let end = self.cursor;
        self.value.replace_range(start..end, "");
        self.cursor = start;
    }
}
pub struct Editor {
    pub target: EditTarget,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
}
pub enum Modal {
    Help,
    Palette { query: String, selected: usize },
    Search,
    Editor(Box<Editor>),
    Confirm { action: Action, prompt: String },
}
pub struct App {
    pub page: Page,
    pub selected: [usize; 10],
    pub file: Option<ConfigFile>,
    pub config: Config,
    pub path: PathBuf,
    pub disk: Option<String>,
    pub dirty: bool,
    pub saving: bool,
    pub connected: bool,
    pub connection: String,
    pub status: Response,
    pub transcript: String,
    pub timings: Option<Timings>,
    pub levels: VecDeque<u64>,
    pub phase: usize,
    pub history: Vec<HistoryEntry>,
    pub detail: Option<HistoryEntry>,
    pub query: String,
    pub offset: usize,
    pub total: u64,
    pub generation: u64,
    pub stats: Stats,
    pub stats_loaded: bool,
    pub activity: Vec<u64>,
    pub overlay: Vec<Setting>,
    pub providers: Vec<Provider>,
    pub cleanup: bool,
    pub diagnostics: Vec<String>,
    pub modal: Option<Modal>,
    pub toast: Option<String>,
    pub quit: bool,
    pub colors: ColorMode,
    pub theme: Theme,
    pub scroll: u16,
}
impl App {
    pub fn new(path: PathBuf, colors: ColorMode) -> Self {
        let config = Config::default();
        let theme = Theme::resolve("paper", &config.ui.themes, colors).expect("built-in theme");
        Self {
            page: Page::Home,
            selected: [0; 10],
            file: None,
            config,
            path,
            disk: None,
            dirty: false,
            saving: false,
            connected: false,
            connection: "Connecting…".into(),
            status: Response::status(State::Idle, 0.0),
            transcript: String::new(),
            timings: None,
            levels: VecDeque::from(vec![0; 48]),
            phase: 0,
            history: vec![],
            detail: None,
            query: String::new(),
            offset: 0,
            total: 0,
            generation: 0,
            stats: Stats::default(),
            stats_loaded: false,
            activity: vec![],
            overlay: vec![],
            providers: vec![],
            cleanup: false,
            diagnostics: vec![],
            modal: None,
            toast: None,
            quit: false,
            colors,
            theme,
            scroll: 0,
        }
    }
    pub fn active(&self) -> bool {
        self.connected && matches!(self.status.state, State::Listening | State::Processing)
    }
    pub fn load(&mut self, file: ConfigFile, disk: Option<String>) -> Result<()> {
        let config = file.config()?;
        let theme = Theme::resolve(&config.ui.theme, &config.ui.themes, self.colors);
        self.theme = theme.unwrap_or_else(|error| {
            self.toast = Some(error.to_string());
            Theme::resolve("paper", &Default::default(), self.colors).expect("paper theme")
        });
        self.config = config;
        self.disk = disk;
        self.file = Some(file);
        self.dirty = false;
        self.saving = false;
        Ok(())
    }
    pub fn selected(&self) -> usize {
        self.selected[self.page.index()]
    }
    pub fn rows(&self) -> Vec<(String, String)> {
        match self.page {
            Page::History => self
                .history
                .iter()
                .map(|h| {
                    (
                        h.text.lines().next().unwrap_or("").into(),
                        format!("#{}  {}  {}ms", h.id, h.provider, h.latency_ms.unwrap_or(0)),
                    )
                })
                .collect(),
            Page::Dictionary => self
                .config
                .dictionary
                .words
                .iter()
                .map(|w| (w.clone(), "Recognition hint".into()))
                .chain(
                    self.config
                        .dictionary
                        .replacements
                        .iter()
                        .map(|r| (format!("{} → {}", r.from, r.to), "Replacement".into())),
                )
                .collect(),
            Page::Snippets => self
                .config
                .snippets
                .iter()
                .map(|s| (s.trigger.clone(), s.text.clone()))
                .collect(),
            Page::Styles => self
                .config
                .styles
                .iter()
                .map(|s| (s.name.clone(), s.apps.join(", ")))
                .collect(),
            Page::Providers => self
                .providers
                .iter()
                .map(|p| (p.name.clone(), p.key.clone()))
                .collect(),
            Page::Settings => self.settings(),
            Page::Overlay => self
                .overlay
                .iter()
                .map(|s| (s.key.clone(), s.value.clone()))
                .collect(),
            Page::Doctor => self
                .diagnostics
                .iter()
                .map(|s| (s.clone(), String::new()))
                .collect(),
            _ => vec![],
        }
    }
    pub fn settings(&self) -> Vec<(String, String)> {
        let mut rows = self
            .file
            .as_ref()
            .and_then(|f| f.list_effective().ok())
            .unwrap_or_default();
        for key in [
            "stt.model",
            "stt.endpoint",
            "stt.protocol",
            "stt.api_key_env",
            "stt.language",
            "cleanup.provider",
            "cleanup.endpoint",
            "cleanup.model",
            "cleanup.prompt",
            "recording.device",
            "sounds.start",
            "sounds.stop",
            "sounds.error",
        ] {
            if !rows.iter().any(|(k, _)| k == key) {
                rows.push((key.into(), "(unset)".into()));
            }
        }
        rows.sort();
        rows
    }
    pub fn select(&mut self, delta: isize) -> Vec<Effect> {
        let count = self.rows().len();
        let pos = self.selected();
        self.selected[self.page.index()] = pos
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        self.scroll = 0;
        if self.page == Page::History {
            self.detail = None;
            if let Some(h) = self.history.get(self.selected()) {
                return vec![Effect::Request(
                    Request::HistoryGet { id: h.id },
                    Tag::Detail(h.id),
                )];
            }
        }
        vec![]
    }
    pub fn refresh(&mut self) -> Vec<Effect> {
        match self.page {
            Page::History => {
                self.generation += 1;
                vec![Effect::Request(
                    Request::History {
                        limit: PAGE_SIZE,
                        offset: self.offset,
                        query: Some(self.query.clone()).filter(|s| !s.is_empty()),
                    },
                    Tag::History(self.generation),
                )]
            }
            Page::Stats => vec![
                Effect::Request(Request::Stats, Tag::Stats),
                Effect::Request(
                    Request::History {
                        limit: PAGE_SIZE,
                        offset: 0,
                        query: None,
                    },
                    Tag::Activity,
                ),
            ],
            Page::Providers => vec![Effect::Providers(self.config.clone(), self.cleanup)],
            Page::Overlay => vec![Effect::Overlay],
            Page::Doctor => vec![Effect::Doctor],
            _ => vec![Effect::Request(Request::Status, Tag::Ordinary)],
        }
    }
    pub fn response(&mut self, response: Response, tag: Tag) -> Vec<Effect> {
        if let Tag::History(generation) = tag {
            if generation != self.generation {
                return vec![];
            }
        }
        if let Tag::Detail(id) = tag {
            if self.history.get(self.selected()).map(|h| h.id) != Some(id) {
                return vec![];
            }
        }
        if !response.ok {
            self.toast = Some(
                response
                    .message
                    .unwrap_or_else(|| "Daemon rejected the request".into()),
            );
            return vec![];
        }
        match tag {
            Tag::History(_) => {
                self.total = response.total.unwrap_or(response.history.len() as u64);
                self.history = response.history;
                self.selected[Page::History.index()] =
                    self.selected[Page::History.index()].min(self.history.len().saturating_sub(1));
                self.detail = None;
                if self.offset > 0 && self.offset as u64 >= self.total {
                    self.offset = ((self.total.saturating_sub(1) as usize) / PAGE_SIZE) * PAGE_SIZE;
                    return self.refresh();
                }
            }
            Tag::Detail(_) => self.detail = response.entry,
            Tag::Activity => {
                self.activity = response
                    .history
                    .iter()
                    .rev()
                    .map(|h| h.text.split_whitespace().count() as u64)
                    .collect()
            }
            Tag::Stats => {
                if let Some(stats) = response.stats {
                    self.stats = stats;
                    self.stats_loaded = true;
                }
            }
            Tag::Mutation => return self.refresh(),
            Tag::Reload => self.toast = Some("Saved and reloaded".into()),
            Tag::Ordinary => {
                // The subscription owns session state; a delayed command reply
                // must never roll a newer state event back to an older state.
                if let Some(text) = response.text {
                    self.transcript = text;
                }
                if let Some(message) = response.message {
                    self.toast = Some(message);
                }
            }
        }
        vec![]
    }
    pub fn event(&mut self, mut response: Response) {
        if let Some(text) = response.text.take() {
            self.transcript = text;
            self.scroll = 0;
        }
        if let Some(timings) = response.timings.take() {
            self.timings = Some(timings);
        }
        if !response.level.is_finite() {
            response.level = 0.0;
        }
        let previous = self.levels.back().copied().unwrap_or(0) as f32;
        let target = if response.state == State::Listening {
            response.level.clamp(0.0, 1.0) * 100.0
        } else {
            0.0
        };
        self.levels.pop_front();
        self.levels
            .push_back((previous * 0.35 + target * 0.65) as u64);
        response.provider = response.provider.or_else(|| self.status.provider.clone());
        response.model = response.model.or_else(|| self.status.model.clone());
        response.version = response.version.or_else(|| self.status.version.clone());
        response.protocol = response.protocol.or(self.status.protocol);
        self.status = response;
    }
    pub fn key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(mut modal) = self.modal.take() {
            let mut keep = true;
            let mut effects = vec![];
            match &mut modal {
                Modal::Help => match key.code {
                    KeyCode::PageDown | KeyCode::Down => {
                        self.scroll = self.scroll.saturating_add(8)
                    }
                    KeyCode::PageUp | KeyCode::Up => self.scroll = self.scroll.saturating_sub(8),
                    _ => {
                        keep = false;
                        self.scroll = 0;
                    }
                },
                Modal::Confirm { action, .. } => match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => {
                        keep = false;
                        effects = self.confirmed(*action);
                    }
                    KeyCode::Char('n') | KeyCode::Esc => keep = false,
                    _ => (),
                },
                Modal::Search => match key.code {
                    KeyCode::Esc | KeyCode::Enter => keep = false,
                    KeyCode::Backspace => {
                        self.query.pop();
                        self.offset = 0;
                        effects = self.refresh();
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && self.query.len() + c.len_utf8() <= 1024 =>
                    {
                        self.query.push(c);
                        self.offset = 0;
                        effects = self.refresh();
                    }
                    _ => (),
                },
                Modal::Palette { query, selected } => match key.code {
                    KeyCode::Esc => keep = false,
                    KeyCode::Backspace => {
                        query.pop();
                        *selected = 0;
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if query.len() < 256 {
                            query.push(c);
                            *selected = 0;
                        }
                    }
                    KeyCode::Down => {
                        *selected = selected.saturating_add(1).min(
                            commands()
                                .iter()
                                .filter(|c| fuzzy(query, &c.title))
                                .count()
                                .saturating_sub(1),
                        )
                    }
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Enter => {
                        let action = commands()
                            .into_iter()
                            .filter(|c| fuzzy(query, &c.title))
                            .nth(*selected)
                            .map(|c| c.action);
                        keep = false;
                        if let Some(action) = action {
                            effects = self.action(action);
                        }
                    }
                    _ => (),
                },
                Modal::Editor(editor) => {
                    if key.code == KeyCode::Esc {
                        keep = false;
                    } else if key.code == KeyCode::Tab {
                        editor.focus = (editor.focus + 1) % editor.fields.len();
                    } else if key.code == KeyCode::BackTab {
                        editor.focus =
                            (editor.focus + editor.fields.len() - 1) % editor.fields.len();
                    } else if key.code == KeyCode::F(2)
                        || (key.code == KeyCode::Char('s')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                        || (key.code == KeyCode::Enter
                            && (editor.fields.len() == 1
                                || key.modifiers.contains(KeyModifiers::CONTROL)))
                    {
                        match self.apply_editor(editor) {
                            Ok(next) => {
                                keep = false;
                                effects = next;
                            }
                            Err(error) => editor.error = Some(format!("{error:#}")),
                        }
                    } else {
                        let field = &mut editor.fields[editor.focus];
                        match key.code {
                            KeyCode::Left | KeyCode::Right if !field.choices.is_empty() => {
                                let pos = field
                                    .choices
                                    .iter()
                                    .position(|c| c == field.value.as_str())
                                    .unwrap_or(0);
                                let count = field.choices.len();
                                let pos = if key.code == KeyCode::Right {
                                    (pos + 1) % count
                                } else {
                                    (pos + count - 1) % count
                                };
                                *field.value = field.choices[pos].clone();
                                field.cursor = field.value.len();
                            }
                            KeyCode::Left => field.left(),
                            KeyCode::Right => field.right(),
                            KeyCode::Home => field.cursor = 0,
                            KeyCode::End => field.cursor = field.value.len(),
                            KeyCode::Delete => field.delete(),
                            KeyCode::Backspace => {
                                field.backspace();
                            }
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                field.value.clear();
                                field.cursor = 0;
                            }
                            KeyCode::Char('j')
                                if key.modifiers.contains(KeyModifiers::CONTROL)
                                    && !field.secret =>
                            {
                                if field.value.len() < 16 * 1024 {
                                    field.insert('\n');
                                }
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if field.value.len() + c.len_utf8() <= 16 * 1024 && !c.is_control()
                                {
                                    field.insert(c);
                                }
                            }
                            KeyCode::Enter => {
                                editor.focus = (editor.focus + 1) % editor.fields.len()
                            }
                            _ => (),
                        }
                    }
                }
            }
            if keep {
                self.modal = Some(modal);
            }
            return effects;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('s') => self.action(Action::Save),
                KeyCode::Char('r') => self.action(Action::Revert),
                KeyCode::Char('p') => self.action(Action::Palette),
                KeyCode::Char('c') => self.action(Action::Quit),
                _ => vec![],
            };
        }
        let action = match key.code {
            KeyCode::Char('1'..='9') => {
                if let KeyCode::Char(c) = key.code {
                    Action::Go(PAGES[(c as u8 - b'1') as usize])
                } else {
                    unreachable!()
                }
            }
            KeyCode::Char('0') => Action::Go(Page::Doctor),
            KeyCode::Char('h') => Action::Go(Page::Home),
            KeyCode::Char('H') => Action::Go(Page::History),
            KeyCode::Char('d') => Action::Go(Page::Dictionary),
            KeyCode::Char('n') => Action::Go(Page::Snippets),
            KeyCode::Char('y') => Action::Go(Page::Styles),
            KeyCode::Char('v') => Action::Go(Page::Providers),
            KeyCode::Char('s') => Action::Go(Page::Settings),
            KeyCode::Char('o') => Action::Go(Page::Overlay),
            KeyCode::Char('a') => Action::Go(Page::Stats),
            KeyCode::Char('D') => Action::Go(Page::Doctor),
            KeyCode::Tab => Action::Go(PAGES[(self.page.index() + 1) % 10]),
            KeyCode::BackTab => Action::Go(PAGES[(self.page.index() + 9) % 10]),
            KeyCode::Up | KeyCode::Char('k') if self.page != Page::Providers => {
                return self.select(-1)
            }
            KeyCode::Down | KeyCode::Char('j') => return self.select(1),
            KeyCode::Up => return self.select(-1),
            KeyCode::Home => return self.select(-(self.selected() as isize)),
            KeyCode::End => return self.select(self.rows().len() as isize),
            KeyCode::PageDown if self.page == Page::History => Action::NextPage,
            KeyCode::PageUp if self.page == Page::History => Action::PrevPage,
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(8);
                return vec![];
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(8);
                return vec![];
            }
            KeyCode::Char(' ') => Action::Toggle,
            KeyCode::Char('C') => Action::Command,
            KeyCode::Esc | KeyCode::Char('x') => Action::Cancel,
            KeyCode::Char('l') => Action::Last,
            KeyCode::Char('c') => Action::Copy,
            KeyCode::Char('p') => Action::Paste,
            KeyCode::Char('N') => Action::Add,
            KeyCode::Char('A') => Action::AddReplacement,
            KeyCode::Enter => Action::Edit,
            KeyCode::Delete => Action::Delete,
            KeyCode::Char('e') => Action::Export,
            KeyCode::Char('u') => Action::Unset,
            KeyCode::Char('t') => Action::Theme,
            KeyCode::Char('?') => Action::Help,
            KeyCode::Char(':') => Action::Palette,
            KeyCode::Char('r') => Action::Refresh,
            KeyCode::Char('/') => Action::Search,
            KeyCode::Char('b') => Action::ProviderKind,
            KeyCode::Char('m') => Action::Model,
            KeyCode::Char('k') => Action::Key,
            KeyCode::Char('K') => Action::DeleteKey,
            KeyCode::Char('T') => Action::CheckProvider,
            KeyCode::Char('i') => Action::Devices,
            KeyCode::Char('[') => Action::ScrollUp,
            KeyCode::Char(']') => Action::ScrollDown,
            KeyCode::Char('q') => Action::Quit,
            _ => return vec![],
        };
        self.action(action)
    }
    pub fn paste(&mut self, text: &str) {
        if let Some(Modal::Editor(editor)) = &mut self.modal {
            let field = &mut editor.fields[editor.focus];
            let multiline = !field.secret;
            if field.value.len() + text.len() <= 16 * 1024 {
                for c in text
                    .chars()
                    .filter(|c| !c.is_control() || (multiline && (*c == '\n' || *c == '\t')))
                {
                    field.insert(c);
                }
            } else {
                editor.error = Some("Input exceeds 16 KiB".into());
            }
        }
    }
    fn edit(&mut self, target: EditTarget, fields: Vec<Field>) {
        self.modal = Some(Modal::Editor(Box::new(Editor {
            target,
            fields,
            focus: 0,
            error: None,
        })));
    }
    fn editable(&mut self) -> bool {
        if self.saving || self.file.is_none() {
            self.toast = Some("Configuration is loading or saving; please wait".into());
            false
        } else {
            true
        }
    }
    pub fn action(&mut self, action: Action) -> Vec<Effect> {
        self.toast = None;
        match action {
            Action::Go(page) => {
                self.page = page;
                self.scroll = 0;
                return self.refresh();
            }
            Action::Toggle => return vec![Effect::Request(Request::toggle(), Tag::Ordinary)],
            Action::Command => {
                return vec![Effect::Request(
                    Request::Start {
                        mode: Mode::Command,
                        context: None,
                        t0_us: None,
                        delivery: Delivery::Inject,
                    },
                    Tag::Ordinary,
                )]
            }
            Action::Cancel => return vec![Effect::Request(Request::Cancel, Tag::Ordinary)],
            Action::Last => return vec![Effect::Request(Request::Last, Tag::Ordinary)],
            Action::Copy | Action::Paste => {
                let request = if self.page == Page::History {
                    match self.history.get(self.selected()) {
                        Some(h) => {
                            if action == Action::Copy {
                                Request::HistoryCopy { id: h.id }
                            } else {
                                Request::HistoryPaste { id: h.id }
                            }
                        }
                        None => return vec![],
                    }
                } else if action == Action::Copy {
                    Request::CopyLast
                } else {
                    Request::PasteLast
                };
                return vec![Effect::Request(request, Tag::Ordinary)];
            }
            Action::Save => {
                if !self.editable() {
                    return vec![];
                }
                if let Some(file) = self.file.take() {
                    self.saving = true;
                    return vec![Effect::Save(file, self.disk.clone())];
                }
            }
            Action::Revert => {
                if self.saving {
                    return vec![];
                }
                if self.dirty {
                    self.modal = Some(Modal::Confirm {
                        action,
                        prompt: "Discard unsaved configuration edits?".into(),
                    });
                } else {
                    return vec![Effect::Load(self.path.clone())];
                }
            }
            Action::Quit => {
                if self.saving {
                    self.toast = Some("Wait for the save to finish before quitting".into());
                } else if self.dirty {
                    self.modal = Some(Modal::Confirm {
                        action,
                        prompt: "Quit and discard unsaved edits? Ctrl+S saves first.".into(),
                    });
                } else {
                    self.quit = true;
                }
            }
            Action::Delete | Action::DeleteKey => {
                if matches!(
                    self.page,
                    Page::History
                        | Page::Dictionary
                        | Page::Snippets
                        | Page::Styles
                        | Page::Providers
                ) && !self.rows().is_empty()
                {
                    self.modal = Some(Modal::Confirm {
                        action,
                        prompt: "Delete the selected entry? This cannot be undone.".into(),
                    });
                }
            }
            Action::Help => {
                self.scroll = 0;
                self.modal = Some(Modal::Help);
            }
            Action::ScrollUp => self.scroll = self.scroll.saturating_sub(4),
            Action::ScrollDown => self.scroll = self.scroll.saturating_add(4),
            Action::Palette => {
                self.modal = Some(Modal::Palette {
                    query: String::new(),
                    selected: 0,
                })
            }
            Action::Search if self.page == Page::History => self.modal = Some(Modal::Search),
            Action::NextPage if self.page == Page::History => {
                if ((self.offset + PAGE_SIZE) as u64) < self.total {
                    self.offset += PAGE_SIZE;
                    self.selected[1] = 0;
                    return self.refresh();
                }
            }
            Action::PrevPage if self.page == Page::History => {
                self.offset = self.offset.saturating_sub(PAGE_SIZE);
                self.selected[1] = 0;
                return self.refresh();
            }
            Action::Refresh => return self.refresh(),
            Action::ProviderKind => {
                self.cleanup = !self.cleanup;
                self.selected[5] = 0;
                self.page = Page::Providers;
                return self.refresh();
            }
            Action::CheckProvider => {
                return vec![Effect::CheckProvider(self.config.clone(), self.cleanup)]
            }
            Action::Devices => return vec![Effect::Devices],
            Action::Theme => {
                let mut choices = BUILTINS.iter().map(|s| s.to_string()).collect::<Vec<_>>();
                choices.extend(self.config.ui.themes.keys().cloned());
                self.edit(
                    EditTarget::Theme,
                    vec![Field::choices(
                        "Theme ←/→",
                        self.config.ui.theme.clone(),
                        choices,
                    )],
                );
            }
            Action::Key => {
                if let Some(p) = self.providers.get(self.selected()) {
                    let mut field = Field::new("API key (masked; never written to config)", "");
                    field.secret = true;
                    self.edit(EditTarget::Key(p.id.clone()), vec![field]);
                }
            }
            Action::Model => {
                if let Some(p) = self.providers.get(self.selected()) {
                    let current = if self.cleanup {
                        self.config.cleanup.model.clone()
                    } else {
                        self.config.stt.model.clone()
                    }
                    .unwrap_or_else(|| p.models.first().cloned().unwrap_or_default());
                    self.edit(
                        EditTarget::Model,
                        vec![Field::choices(
                            "Model ←/→ or type custom id",
                            current,
                            p.models.clone(),
                        )],
                    );
                }
            }
            Action::Export if self.page == Page::History => {
                if let Some(h) = self.history.get(self.selected()) {
                    self.edit(
                        EditTarget::Export(h.clone()),
                        vec![Field::new(
                            "New export file (JSON; existing files are never overwritten)",
                            format!("xflow-history-{}.json", h.id),
                        )],
                    );
                }
            }
            Action::Export => {
                if let Some(file) = &self.file {
                    let _ = file;
                    self.edit(
                        EditTarget::Draft,
                        vec![Field::new(
                            "New file for configuration draft",
                            "xflow-config-draft.toml",
                        )],
                    );
                }
            }
            Action::Unset if self.page == Page::Settings => {
                if !self.editable() {
                    return vec![];
                }
                if let Some((key, _)) = self.settings().get(self.selected()) {
                    let key = key.clone();
                    match self.file.as_mut().unwrap().unset(&key) {
                        Ok(_) => self.sync_config(),
                        Err(e) => self.toast = Some(format!("{e:#}")),
                    };
                }
            }
            Action::Add | Action::AddReplacement | Action::Edit => return self.open_editor(action),
            _ => (),
        }
        vec![]
    }
    fn sync_config(&mut self) {
        if let Some(file) = &self.file {
            if let Ok(config) = file.config() {
                self.dirty = self.disk.as_deref() != Some(file.to_string().as_str());
                if let Ok(theme) = Theme::resolve(&config.ui.theme, &config.ui.themes, self.colors)
                {
                    self.theme = theme;
                }
                self.config = config;
            }
        }
    }
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        if self.saving {
            bail!("Wait for the current save to finish");
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| anyhow!("Configuration unavailable"))?;
        let old = file.get(key);
        file.set(key, value)?;
        let config = file.config()?;
        let validation = (|| -> Result<()> {
            Theme::resolve(&config.ui.theme, &config.ui.themes, self.colors)?;
            for name in config.ui.themes.keys() {
                Theme::resolve(name, &config.ui.themes, self.colors)?;
            }
            Ok(())
        })();
        if let Err(error) = validation {
            match old {
                Some(old) => file.set(key, &old)?,
                None => {
                    file.unset(key)?;
                }
            }
            return Err(error);
        }
        self.sync_config();
        Ok(())
    }
    fn open_editor(&mut self, action: Action) -> Vec<Effect> {
        if !self.editable() {
            return vec![];
        }
        let pos = self.selected();
        let new = action != Action::Edit;
        match self.page {
            Page::Home => {
                self.page = Page::Providers;
                return self.refresh();
            }
            Page::Dictionary => {
                let words = self.config.dictionary.words.len();
                if action == Action::AddReplacement || (!new && pos >= words) {
                    let index = (!new).then_some(pos.saturating_sub(words));
                    let r = index.and_then(|i| self.config.dictionary.replacements.get(i));
                    let fields = vec![
                        Field::new("From", r.map(|r| r.from.as_str()).unwrap_or("")),
                        Field::new(
                            "To (empty deletes the word)",
                            r.map(|r| r.to.as_str()).unwrap_or(""),
                        ),
                    ];
                    self.edit(EditTarget::Replacement(index), fields);
                } else {
                    let index = (!new).then_some(pos);
                    self.edit(
                        EditTarget::Word(index),
                        vec![Field::new(
                            "Word",
                            index
                                .and_then(|i| self.config.dictionary.words.get(i))
                                .cloned()
                                .unwrap_or_default(),
                        )],
                    );
                }
            }
            Page::Snippets => {
                let index = (!new).then_some(pos);
                let s = index.and_then(|i| self.config.snippets.get(i));
                let fields = vec![
                    Field::new("Trigger", s.map(|s| s.trigger.as_str()).unwrap_or("")),
                    Field::new(
                        "Text (Ctrl+J adds a new line)",
                        s.map(|s| s.text.as_str()).unwrap_or(""),
                    ),
                ];
                self.edit(EditTarget::Snippet(index), fields);
            }
            Page::Styles => {
                let index = (!new).then_some(pos);
                let s = index.and_then(|i| self.config.styles.get(i));
                let fields = vec![
                    Field::new("Name", s.map(|s| s.name.as_str()).unwrap_or("")),
                    Field::new(
                        "Apps (comma separated)",
                        s.map(|s| s.apps.join(", ")).unwrap_or_default(),
                    ),
                    Field::choices(
                        "Mode ←/→",
                        s.and_then(|s| s.mode).map(mode_name).unwrap_or("inherit"),
                        ["inherit", "raw", "light", "polished", "custom"]
                            .map(String::from)
                            .to_vec(),
                    ),
                    Field::new(
                        "Prompt (Ctrl+J adds a new line)",
                        s.and_then(|s| s.prompt.as_deref()).unwrap_or(""),
                    ),
                ];
                self.edit(EditTarget::Style(index), fields);
            }
            Page::Settings => {
                if let Some((key, value)) = self.settings().get(pos) {
                    let field = setting_field(key, value);
                    self.edit(EditTarget::Setting(key.clone()), vec![field]);
                }
            }
            Page::Overlay => {
                if let Some(s) = self.overlay.get(pos) {
                    let choices = match &s.kind {
                        Kind::Bool => vec!["true".into(), "false".into()],
                        Kind::Choice(c) => c.clone(),
                        _ => vec![],
                    };
                    let hint = match s.kind {
                        Kind::Int { min, max } => format!("{} ({min}..{max})", s.key),
                        Kind::Double { min, max } => format!("{} ({min}..{max})", s.key),
                        _ => s.key.clone(),
                    };
                    self.edit(
                        EditTarget::Overlay(s.key.clone()),
                        vec![Field::choices(&hint, s.value.clone(), choices)],
                    );
                }
            }
            Page::Providers => {
                if let Some(p) = self.providers.get(pos) {
                    let key = if self.cleanup {
                        "cleanup.provider"
                    } else {
                        "stt.provider"
                    };
                    let id = p.id.clone();
                    match self.set(key, &id) {
                        Ok(()) => {
                            self.toast = Some("Provider selected; Ctrl+S saves and reloads".into())
                        }
                        Err(error) => self.toast = Some(format!("{error:#}")),
                    };
                }
            }
            Page::Doctor => {
                self.toast =
                    Some("Fix hints are shown beside each check; r runs checks again".into())
            }
            _ => (),
        }
        vec![]
    }
    fn apply_editor(&mut self, editor: &mut Editor) -> Result<Vec<Effect>> {
        // Move secrets directly into a zeroizing payload; never clone them into
        // ordinary form strings or surface provider errors containing the key.
        if let EditTarget::Key(id) = &editor.target {
            if editor.fields[0].value.trim().is_empty() {
                bail!("API key cannot be empty");
            }
            return Ok(vec![Effect::SaveKey(
                id.clone(),
                Zeroizing::new(std::mem::take(&mut *editor.fields[0].value)),
            )]);
        }
        let values: Vec<_> = editor.fields.iter().map(|f| f.value.to_string()).collect();
        let value = &values[0];
        let mut config = self.config.clone();
        let key = match &editor.target {
            EditTarget::Setting(key) => {
                if key.starts_with("ui.") {
                    let mut candidate = config.clone();
                    if key == "ui.theme" {
                        candidate.ui.theme = value.trim_matches('"').into();
                        Theme::resolve(&candidate.ui.theme, &candidate.ui.themes, self.colors)?;
                    }
                }
                self.set(key, value)?;
                return Ok(vec![]);
            }
            EditTarget::Theme => {
                Theme::resolve(value, &config.ui.themes, self.colors)?;
                self.set("ui.theme", value)?;
                return Ok(vec![]);
            }
            EditTarget::Device => {
                self.set("recording.device", value)?;
                return Ok(vec![]);
            }
            EditTarget::Model => {
                self.set(
                    if self.cleanup {
                        "cleanup.model"
                    } else {
                        "stt.model"
                    },
                    value,
                )?;
                return Ok(vec![]);
            }
            EditTarget::Key(id) => {
                if value.trim().is_empty() {
                    bail!("API key cannot be empty");
                }
                return Ok(vec![Effect::SaveKey(
                    id.clone(),
                    Zeroizing::new(std::mem::take(&mut *editor.fields[0].value)),
                )]);
            }
            EditTarget::Overlay(key) => {
                return Ok(vec![Effect::OverlaySet(key.clone(), value.clone())])
            }
            EditTarget::Export(entry) => {
                if value.trim().is_empty() {
                    bail!("Choose a file path");
                }
                return Ok(vec![Effect::Export(PathBuf::from(value), entry.clone())]);
            }
            EditTarget::Draft => {
                if value.trim().is_empty() {
                    bail!("Choose a file path");
                }
                let file = self
                    .file
                    .as_ref()
                    .ok_or_else(|| anyhow!("Wait for save to finish"))?;
                return Ok(vec![Effect::ExportDraft(
                    PathBuf::from(value),
                    file.to_string(),
                )]);
            }
            EditTarget::Word(index) => {
                replace(&mut config.dictionary.words, *index, value.trim().into())?;
                "dictionary.words"
            }
            EditTarget::Replacement(index) => {
                replace(
                    &mut config.dictionary.replacements,
                    *index,
                    Replacement {
                        from: value.trim().into(),
                        to: values[1].clone(),
                    },
                )?;
                "dictionary.replacements"
            }
            EditTarget::Snippet(index) => {
                replace(
                    &mut config.snippets,
                    *index,
                    Snippet {
                        trigger: value.trim().into(),
                        text: values[1].clone(),
                    },
                )?;
                "snippets"
            }
            EditTarget::Style(index) => {
                let mode = match values[2].as_str() {
                    "inherit" => None,
                    "raw" => Some(CleanupMode::Raw),
                    "light" => Some(CleanupMode::Light),
                    "polished" => Some(CleanupMode::Polished),
                    "custom" => Some(CleanupMode::Custom),
                    _ => bail!("Mode must be inherit/raw/light/polished/custom"),
                };
                replace(
                    &mut config.styles,
                    *index,
                    Style {
                        name: value.trim().into(),
                        apps: values[1]
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .collect(),
                        mode,
                        prompt: Some(values[3].clone()).filter(|s| !s.trim().is_empty()),
                    },
                )?;
                "styles"
            }
        };
        config.validate()?;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| anyhow!("Wait for configuration to load"))?;
        match key {
            "dictionary.words" => file.set_typed(key, &config.dictionary.words)?,
            "dictionary.replacements" => file.set_typed(key, &config.dictionary.replacements)?,
            "snippets" => file.set_typed(key, &config.snippets)?,
            "styles" => file.set_typed(key, &config.styles)?,
            _ => unreachable!(),
        }
        self.sync_config();
        self.toast = Some("Edit staged; Ctrl+S saves and reloads".into());
        Ok(vec![])
    }
    fn confirmed(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Quit => self.quit = true,
            Action::Revert => return vec![Effect::Load(self.path.clone())],
            Action::DeleteKey => {
                if let Some(p) = self.providers.get(self.selected()) {
                    return vec![Effect::DeleteKey(p.id.clone())];
                }
            }
            Action::Delete => {
                let pos = self.selected();
                if self.page == Page::History {
                    if let Some(h) = self.history.get(pos) {
                        return vec![Effect::Request(
                            Request::HistoryDelete { id: h.id },
                            Tag::Mutation,
                        )];
                    }
                }
                if !self.editable() {
                    return vec![];
                }
                let file = self.file.as_mut().unwrap();
                let result = match self.page {
                    Page::Dictionary => {
                        let mut dictionary = self.config.dictionary.clone();
                        if pos < dictionary.words.len() {
                            dictionary.words.remove(pos);
                            file.set_typed("dictionary.words", &dictionary.words)
                        } else if pos - dictionary.words.len() < dictionary.replacements.len() {
                            dictionary.replacements.remove(pos - dictionary.words.len());
                            file.set_typed("dictionary.replacements", &dictionary.replacements)
                        } else {
                            Ok(())
                        }
                    }
                    Page::Snippets => {
                        let mut rows = self.config.snippets.clone();
                        if pos < rows.len() {
                            rows.remove(pos);
                        }
                        file.set_typed("snippets", &rows)
                    }
                    Page::Styles => {
                        let mut rows = self.config.styles.clone();
                        if pos < rows.len() {
                            rows.remove(pos);
                        }
                        file.set_typed("styles", &rows)
                    }
                    _ => Ok(()),
                };
                match result {
                    Ok(()) => self.sync_config(),
                    Err(e) => self.toast = Some(format!("{e:#}")),
                };
                let _ = self.select(0);
            }
            _ => (),
        }
        vec![]
    }
    pub fn devices(&mut self, devices: Vec<String>) {
        self.edit(
            EditTarget::Device,
            vec![Field::choices(
                "Microphone ←/→",
                self.config.recording.device.clone().unwrap_or_default(),
                devices,
            )],
        );
    }
}
fn replace<T>(rows: &mut Vec<T>, index: Option<usize>, value: T) -> Result<()> {
    match index {
        Some(i) => {
            let item = rows
                .get_mut(i)
                .ok_or_else(|| anyhow!("Selection no longer exists"))?;
            *item = value;
        }
        None => rows.push(value),
    };
    Ok(())
}
fn mode_name(mode: CleanupMode) -> &'static str {
    match mode {
        CleanupMode::Raw => "raw",
        CleanupMode::Light => "light",
        CleanupMode::Polished => "polished",
        CleanupMode::Custom => "custom",
    }
}
fn setting_field(key: &str, value: &str) -> Field {
    let value = if value == "(unset)" {
        String::new()
    } else if let Ok(doc) = format!("value = {value}").parse::<toml::Table>() {
        doc.get("value")
            .and_then(toml::Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| value.into())
    } else {
        value.into()
    };
    let choices = match key {
        "cleanup.mode" => vec!["raw", "light", "polished", "custom"],
        "injection.method" => vec!["paste", "type", "clipboard"],
        _ if value == "true" || value == "false" => vec!["true", "false"],
        _ => vec![],
    };
    let hint = match key {
        "recording.max_seconds" => "1..600",
        "recording.min_ms" => "0..5000",
        "recording.auto_stop_secs" => "0..60",
        "recording.keep_warm_secs" => "0..600",
        "recording.silence_threshold" | "sounds.volume" => "0..1",
        "injection.restore_delay_ms" => "0..10000",
        "stt.timeout_secs" => "1..300",
        "cleanup.timeout_secs" => "1..120",
        "privacy.history_limit" => "0..100000",
        _ => "text / TOML; u resets",
    };
    Field::choices(
        &format!("{key} ({hint})"),
        value,
        choices.into_iter().map(String::from).collect(),
    )
}
