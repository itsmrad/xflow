use super::{
    integrations,
    options::{self, value_text},
    theme::{ColorMode, Theme, BUILTINS},
};
use crate::{
    config_edit::ConfigFile,
    gsettings::{Kind, Setting},
};
use anyhow::{anyhow, bail, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};
use xflow_core::{
    config::{Config, Replacement, Snippet, Style},
    ipc::{Delivery, Grant, HistoryEntry, Permissions, Request, Response, Stats, Timings},
    CleanupMode, Mode, State,
};
use xflow_providers::KeySource;
use zeroize::Zeroizing;

pub const PAGE_SIZE: usize = 30;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    History,
    Dictionary,
    Snippets,
    Styles,
    Settings,
    Overlay,
    Stats,
    Doctor,
}
pub const PAGES: [Page; 9] = [
    Page::Home,
    Page::History,
    Page::Dictionary,
    Page::Snippets,
    Page::Styles,
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
            Self::Settings => "Settings",
            Self::Overlay => "Pill",
            Self::Stats => "Your stats",
            Self::Doctor => "Check-up",
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
    Copy,
    Paste,
    Add,
    AddCorrection,
    Choose,
    Edit,
    Delete,
    Search,
    NextPage,
    PrevPage,
    Save,
    Help,
    Palette,
    Refresh,
    Setup,
    Back,
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
            key: ["1", "2", "3", "4", "5", "6", "7", "8", "9"][i],
        })
        .collect();
    for (action, title, key) in [
        (Action::Toggle, "Start or stop dictation", "Space"),
        (Action::Cancel, "Cancel dictation", "x"),
        (Action::Command, "Edit selected text by voice", "C"),
        (Action::Copy, "Copy text", "c"),
        (Action::Paste, "Paste text into the app you were using", "p"),
        (Action::Choose, "Open or change the selected item", "Enter"),
        (Action::Add, "Add something new", "n"),
        (Action::Delete, "Delete the selected item", "Del"),
        (Action::Search, "Search history", "/"),
        (Action::Setup, "Guided setup", "Settings"),
        (Action::Refresh, "Refresh this screen", "r"),
        (Action::Back, "Go back", "Esc"),
        (Action::Help, "Help", "?"),
        (Action::Palette, "Find an action", "Ctrl+P"),
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
    Session,
    Done(&'static str),
    Last,
    History(u64),
    Detail(i64),
    Mutation,
    Stats,
    Activity,
    Reload,
    /// A permission check; Some(grant) answers `Request::Allow { grant }`.
    Permissions(Option<Grant>),
}
pub enum Effect {
    Request(Request, Tag),
    Save(ConfigFile, Option<String>),
    Load(PathBuf),
    Overlay,
    OverlaySet(String, String),
    KeyStatus(Config),
    SaveKey(String, Zeroizing<String>),
    CheckProvider(Config, bool),
    MicTimeout(std::time::Duration),
    Devices,
    Doctor,
    /// Open a URL, e.g. a System Settings pane.
    Open(&'static str),
    /// macOS asks once per process: restart xflowd, which asks as it starts, then
    /// open the grant's pane. Without launchd, asks the running xflowd instead.
    AskAgain(Grant),
    /// Read "Press 🌐 key to" (macOS).
    Globe,
    /// Re-save these Keychain keys so xflowd reads them without asking (macOS).
    ShareKeys(Vec<String>),
    /// Start xflowd the way `xflow daemon start` does (launchd / systemd).
    StartHelper,
}
/// A step still waiting this long likely faces an old, switched-on xflowd row.
const STALE_AFTER: Duration = Duration::from_secs(10);
/// System Settings › Keyboard, where "Press 🌐 key to" lives.
pub const KEYBOARD_PANE: &str = "x-apple.systempreferences:com.apple.Keyboard-Settings.extension";
/// The guided first-run flow. Every step is a choice except pasting the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Provider,
    Key,
    Microphone,
    MicCheck,
    Shortcut,
    /// macOS grants, one at a time; moves on by itself once each switch is on.
    Permissions,
    /// "Press 🌐 key to: Do Nothing", when Fn is the shortcut; moves on by itself.
    Globe,
    Try,
    Done,
}
#[derive(Clone, Debug)]
pub enum EditTarget {
    Setting(String),
    Word(Option<usize>),
    Replacement(Option<usize>),
    Snippet(Option<usize>),
    Style(Option<usize>),
    Overlay(String),
    Key { id: String, cleanup: bool },
    Device,
    Menu(Vec<Action>),
    Setup(Step),
    Transcript(i64),
}
pub struct Field {
    pub label: String,
    pub value: Zeroizing<String>,
    pub choices: Vec<String>,
    pub labels: Vec<String>,
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
            labels: vec![],
            secret: false,
            cursor,
        }
    }
    /// A closed list; the current value is always listed so nothing is lost.
    fn pick(
        label: &str,
        value: impl Into<String>,
        mut choices: Vec<String>,
        name: impl Fn(&str) -> String,
    ) -> Self {
        let value = value.into();
        if !choices.contains(&value) {
            choices.insert(0, value.clone());
        }
        Self {
            labels: choices.iter().map(|c| name(c)).collect(),
            choices,
            ..Self::new(label, value)
        }
    }
    pub fn position(&self) -> usize {
        self.choices
            .iter()
            .position(|c| c == self.value.as_str())
            .unwrap_or(0)
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
    pub title: String,
    pub intro: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
    pub capturing: bool,
}
impl Editor {
    pub fn picking(&self) -> bool {
        !self.fields[self.focus].choices.is_empty()
    }
}
pub enum Modal {
    Help,
    Palette { query: String, selected: usize },
    Search,
    Editor(Box<Editor>),
    Confirm { action: Action, prompt: String },
    Setup(Step),
}
pub struct App {
    pub page: Page,
    pub selected: [usize; 9],
    pub file: Option<ConfigFile>,
    pub config: Config,
    pub path: PathBuf,
    pub disk: Option<String>,
    pub dirty: bool,
    pub saving: bool,
    /// The daemon was busy when we asked it to reload; ask again once it's idle.
    pub reload_pending: bool,
    /// TUI starts awaiting their reply; one may still begin a recording.
    pub starts: usize,
    /// A recording a TUI start began; cancel it on exit.
    pub session_pending: bool,
    pub connected: bool,
    pub connection: String,
    /// Ask-again restarted xflowd: its disconnect is expected, not a failure.
    pub restarting: bool,
    /// Disconnects in a row since the last connect; the first real one starts xflowd.
    pub downs: u32,
    /// Starting xflowd failed; Enter tries again.
    pub start_failed: bool,
    /// Last key typed here, and when xflowd's permissions last arrived (see tap_blind).
    pub typed: Option<Instant>,
    pub status_at: Option<Instant>,
    /// When xflowd's current tap was (probably) created: connect, or events granted.
    pub armed_at: Option<Instant>,
    /// The current permission step started waiting; Enter asked again.
    pub waiting_since: Option<Instant>,
    pub asked_again: bool,
    pub status: Response,
    pub transcript: String,
    pub timings: Option<Timings>,
    pub levels: VecDeque<u64>,
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
    pub overlay_error: bool,
    pub missing_key: bool,
    pub missing_cleanup_key: bool,
    /// The keychain didn't answer, e.g. its Always Allow prompt is still open.
    pub key_locked: bool,
    pub cleanup_key_locked: bool,
    pub advanced: bool,
    pub setup: Option<Step>,
    pub setup_offered: bool,
    /// xflowd's own grants, from its status.
    pub permissions: Option<Permissions>,
    /// Only known after asking xflowd (`Allow { grant: Microphone }`); None while undecided.
    pub microphone: Option<bool>,
    /// A microphone ask is in flight.
    pub asking: bool,
    /// Open the Microphone pane if the pending ask finds it switched off.
    pub mic_pane: bool,
    /// The grant a screen is waiting for (the permissions step, or a Check-up fix).
    pub grant: Option<Grant>,
    pub permissions_offered: bool,
    /// The once-per-session microphone check on a configured install was sent.
    pub mic_checked: bool,
    /// "Press 🌐 key to" is Do Nothing; None until read (or off macOS).
    pub globe: Option<bool>,
    /// This session already re-saved its Keychain keys for xflowd.
    pub keys_shared: bool,
    /// Step to open once settings load on a configured install: the wizard (`xflow setup`).
    pub start: Option<Step>,
    pub diagnostics: Vec<String>,
    pub modal: Option<Modal>,
    pub toast: Option<String>,
    pub quit: bool,
    pub colors: ColorMode,
    pub theme: Theme,
    pub scroll: u16,
    /// Fixed clock for tests; `None` reads the system clock.
    pub now: Option<i64>,
}
impl App {
    pub fn new(path: PathBuf, colors: ColorMode) -> Self {
        let config = Config::default();
        let theme = Theme::resolve("paper", &config.ui.themes, colors).expect("built-in theme");
        Self {
            page: Page::Home,
            selected: [0; 9],
            file: None,
            config,
            path,
            disk: None,
            dirty: false,
            saving: false,
            connected: false,
            connection: "Connecting…".into(),
            restarting: false,
            downs: 0,
            start_failed: false,
            typed: None,
            status_at: None,
            armed_at: None,
            waiting_since: None,
            asked_again: false,
            status: Response::status(State::Idle, 0.0),
            transcript: String::new(),
            timings: None,
            levels: VecDeque::from(vec![0; 48]),
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
            overlay_error: false,
            missing_key: false,
            missing_cleanup_key: false,
            key_locked: false,
            cleanup_key_locked: false,
            advanced: false,
            setup: None,
            setup_offered: false,
            permissions: None,
            microphone: None,
            asking: false,
            mic_pane: false,
            grant: None,
            permissions_offered: false,
            mic_checked: false,
            globe: None,
            keys_shared: false,
            start: None,
            diagnostics: vec![],
            modal: None,
            toast: None,
            quit: false,
            reload_pending: false,
            starts: 0,
            session_pending: false,
            colors,
            theme,
            scroll: 0,
            now: None,
        }
    }
    pub fn load(&mut self, file: ConfigFile, disk: Option<String>) -> Result<()> {
        let config = file.config()?;
        let theme = Theme::resolve(&config.ui.theme, &config.ui.themes, self.colors);
        self.theme = theme.unwrap_or_else(|_| {
            Theme::resolve("paper", &Default::default(), self.colors).expect("paper theme")
        });
        self.config = config;
        self.missing_key = false;
        self.disk = disk;
        self.file = Some(file);
        self.dirty = false;
        Ok(())
    }
    pub fn now(&self) -> i64 {
        self.now.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        })
    }
    pub fn active(&self) -> bool {
        self.connected && matches!(self.status.state, State::Listening | State::Processing)
    }
    pub fn needs_setup(&self) -> bool {
        self.file.is_some() && (self.disk.is_none() || self.missing_key)
    }
    /// The effective (default-filled) value at a dotted key, as TOML text.
    pub fn effective(&self, key: &str) -> Option<String> {
        self.file.as_ref()?.effective(key).ok().flatten()
    }
    /// Plain "Hold Fn and speak" guidance for the configured shortcut.
    pub fn shortcut(&self) -> Option<String> {
        let key = self.effective("hotkey.key")?;
        (self.effective("hotkey.enabled").as_deref() != Some("false"))
            .then(|| options::key_label(key.trim_matches('"')))
    }
    pub fn selected(&self) -> usize {
        self.selected[self.page.index()]
    }
    pub fn key_status(&mut self, config: &Config, keys: (KeySource, KeySource)) {
        // An older lookup must not replace onboarding for a newly edited route.
        if config.stt == self.config.stt {
            self.missing_key = keys.0 == KeySource::Missing;
            self.key_locked = keys.0 == KeySource::Locked;
        }
        if config.cleanup == self.config.cleanup {
            self.missing_cleanup_key = keys.1 == KeySource::Missing;
            self.cleanup_key_locked = keys.1 == KeySource::Locked;
        }
    }
    /// Leading "+ Add" rows on editable list pages.
    pub fn adds(&self) -> usize {
        match self.page {
            Page::Dictionary => 2,
            Page::Snippets | Page::Styles => 1,
            _ => 0,
        }
    }
    pub fn rows(&self) -> Vec<(String, String)> {
        let add = |label: &str, hint: &str| (format!("+ {label}"), hint.to_string());
        match self.page {
            Page::History => {
                let now = self.now();
                self.history
                    .iter()
                    .map(|h| {
                        let mut meta = options::ago(h.created_at, now);
                        if let Some(app) = &h.app_id {
                            meta = format!("{meta} · {}", options::app_name(app));
                        }
                        (h.text.lines().next().unwrap_or("").into(), meta)
                    })
                    .collect()
            }
            Page::Dictionary => [
                add("Add a word", "Names and terms xflow should know"),
                add("Add a correction", "Fix a word xflow keeps getting wrong"),
            ]
            .into_iter()
            .chain(
                self.config
                    .dictionary
                    .words
                    .iter()
                    .map(|w| (w.clone(), "Word".into())),
            )
            .chain(
                self.config
                    .dictionary
                    .replacements
                    .iter()
                    .map(|r| (format!("{} → {}", r.from, r.to), "Correction".into())),
            )
            .collect(),
            Page::Snippets => {
                std::iter::once(add("Add a snippet", "Say a short phrase, get longer text"))
                    .chain(self.config.snippets.iter().map(|s| {
                        (
                            s.trigger.clone(),
                            s.text.lines().next().unwrap_or("").into(),
                        )
                    }))
                    .collect()
            }
            Page::Styles => std::iter::once(add("Add a style", "A writing tone for specific apps"))
                .chain(self.config.styles.iter().map(|s| {
                    (
                        s.name.clone(),
                        s.apps
                            .iter()
                            .map(|a| options::app_name(a))
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                }))
                .collect(),
            Page::Settings => self.settings(),
            Page::Overlay => self
                .overlay
                .iter()
                .map(|s| (s.summary.clone(), overlay_value(&s.value)))
                .collect(),
            Page::Doctor => {
                let ok = options::glyph("✓", "+");
                let helper = if self.connected {
                    format!("{ok} xflow's background helper is running")
                } else {
                    format!("! {}", self.helper_down())
                };
                let key = if self.key_locked {
                    format!("! {}", xflow_providers::KEYCHAIN_WAITING)
                } else if self.missing_key {
                    "! Your speech service has no API key — Settings → Speech → API key".into()
                } else {
                    format!("{ok} Your speech service is ready")
                };
                [helper, key]
                    .into_iter()
                    .map(|s| (s, String::new()))
                    .chain(
                        self.permission_rows()
                            .into_iter()
                            .map(|(row, more, _)| (row, more)),
                    )
                    .chain(self.diagnostics.iter().map(|s| (s.clone(), String::new())))
                    .collect()
            }
            _ => vec![],
        }
    }
    /// Grants xflowd still lacks, in onboarding order. The microphone counts until
    /// xflowd says it's allowed: its state is only known after asking.
    pub fn missing(&self) -> Vec<Grant> {
        let Some(p) = &self.permissions else {
            return vec![];
        };
        [
            (Grant::Accessibility, p.accessibility),
            (Grant::InputMonitoring, !self.tap_blind()),
            (Grant::Microphone, self.microphone == Some(true)),
        ]
        .into_iter()
        .filter_map(|(grant, on)| (!on).then_some(grant))
        .collect()
    }
    /// Accessibility also lets the tap hear keys, so Input Monitoring is asked only
    /// when the tap still heard nothing in a status taken after a key was typed
    /// here (the tap sees this terminal's keys too).
    /// Only keys typed after the current tap was armed count, as a new tap starts
    /// deaf. ponytail: Secure Keyboard Entry hides this terminal's keys from every
    /// tap, so there a working tap reads as blind and costs one extra ask.
    pub fn tap_blind(&self) -> bool {
        let settle = Duration::from_millis(300); // a re-arm is asynchronous
        self.permissions.as_ref().is_some_and(|p| {
            p.accessibility && !p.input_monitoring && p.keyboard_events == Some(false)
        }) && matches!(
            (self.armed_at, self.typed, self.status_at),
            (Some(armed), Some(typed), Some(status)) if typed >= armed + settle && status >= typed + settle
        )
    }
    /// A new xflowd, or event access just granted, means a new (deaf) tap.
    pub fn take_permissions(&mut self, permissions: Option<Permissions>) {
        let events = |p: &Option<Permissions>| p.as_ref().is_some_and(|p| p.events());
        let now = Instant::now();
        if !events(&self.permissions) && events(&permissions) {
            self.armed_at = Some(now);
        }
        (self.permissions, self.status_at) = (permissions, Some(now));
    }
    /// The stale-row hint, once a step waited STALE_AFTER or the person asked again.
    pub fn stale_hint(&self) -> bool {
        self.asked_again
            || self
                .waiting_since
                .is_some_and(|t| t.elapsed() >= STALE_AFTER)
    }
    /// Check-up rows for xflowd's macOS grants: (row, details, grant Enter fixes).
    /// Rows stay short enough for the list pane; the details pane explains.
    pub fn permission_rows(&self) -> Vec<(String, String, Option<Grant>)> {
        use xflow_core::ipc::{STALE_ROWS, UNVERIFIED_SHORTCUT};
        if !cfg!(target_os = "macos") {
            return vec![];
        }
        let ok = options::glyph("✓", "+");
        let Some(p) = &self.permissions else {
            return vec![(
                "! Can't check permissions yet".into(),
                "They show up once xflow's background helper is running.".into(),
                None,
            )];
        };
        let blind = self.tap_blind();
        let mut rows: Vec<_> = [
            (Grant::Accessibility, "Typing access", "Accessibility", "type your words where your cursor is and hear your shortcut", Some(p.accessibility)),
            (Grant::Microphone, "Microphone", "Microphone", "hear you", self.microphone),
            // Only when Accessibility doesn't get the shortcut through.
            (Grant::InputMonitoring, "Shortcut access", "Input Monitoring", "hear your shortcut key in every app", blind.then_some(false)),
        ]
        .into_iter()
        .filter(|(grant, ..)| *grant != Grant::InputMonitoring || blind)
        .map(|(grant, label, pane, why, on)| {
            let about = format!("{label} ({pane}) lets xflow {why}.");
            match on {
                Some(true) => (format!("{ok} {label} is on"), format!("{about}\n\nIt's on."), None),
                None => (
                    format!("• {label} — Enter to check"),
                    format!("{about}\n\nPress Enter to check. If your Mac asks, click Allow. If it's off, System Settings opens at {pane}: find xflowd in the list and switch it on."),
                    Some(grant),
                ),
                Some(false) => (
                    format!("! {label} off — Enter to fix"),
                    format!("{about}\n\nPress Enter and System Settings opens at {pane}: find xflowd in the list and switch it on. You never need to add it yourself.{}", if self.stale_hint() { format!("\n\n{STALE_ROWS}") } else { String::new() }),
                    Some(grant),
                ),
            }
        })
        .collect();
        match p.keyboard_events {
            Some(true) => rows.push((
                format!("{ok} Your shortcut reaches xflow"),
                String::new(),
                None,
            )),
            Some(false) => rows.push((
                "! Shortcut not heard yet — press it".into(),
                UNVERIFIED_SHORTCUT.into(),
                None,
            )),
            None => (),
        }
        rows
    }
    /// The grant behind a Check-up row (after the helper and key rows).
    pub fn doctor_grant(&self, pos: usize) -> Option<Grant> {
        let rows = self.permission_rows();
        rows.get(pos.checked_sub(2)?)?.2
    }
    /// One step: xflowd asks macOS (which lists xflowd) and the exact pane opens.
    /// `again` is a person asking: the running xflowd already used its one ask.
    /// A restart would drop a dictation in progress, so then the running one asks.
    pub fn fix(&mut self, grant: Grant, again: bool) -> Vec<Effect> {
        self.asked_again |= again;
        if again && grant != Grant::Microphone && !self.active() {
            self.restarting = true;
            return vec![Effect::AskAgain(grant)];
        }
        let mut effects = vec![Effect::Request(
            Request::Allow { grant },
            Tag::Permissions(Some(grant)),
        )];
        if grant != Grant::Microphone || self.microphone == Some(false) {
            effects.push(Effect::Open(grant.pane()));
        } else {
            // Undecided: the system prompt is the whole step; open the pane only if off.
            self.asking = true;
            self.mic_pane = true;
        }
        if grant == Grant::Microphone {
            self.grant = Some(grant); // re-asked until allowed (see `poll`)
        }
        effects
    }
    /// The Fn shortcut also fires the Globe key's own action unless that is Do Nothing.
    pub fn globe_off(&self) -> bool {
        self.globe == Some(false) && self.config.hotkey.enabled && self.config.hotkey.key == "fn"
    }
    /// After the grants and the Globe key: finish with the shortcut test.
    fn checks_done(&mut self) -> Vec<Effect> {
        // Offered on its own to an install that still lacks a key: set that up next.
        let setup = self.permissions_offered && self.needs_setup();
        self.open_setup(if setup { Step::Welcome } else { Step::Try })
    }
    pub fn globe_changed(&mut self) -> Vec<Effect> {
        if matches!(self.modal, Some(Modal::Setup(Step::Globe))) && !self.globe_off() {
            return self.checks_done();
        }
        self.permissions_changed()
    }
    /// Ask for the next missing grant, then the Globe key; finish with the shortcut test.
    fn permissions_step(&mut self) -> Vec<Effect> {
        // Grants come from xflowd's status: wait for xflowd rather than skip them.
        if cfg!(target_os = "macos") && self.permissions.is_none() && !self.connected {
            return vec![];
        }
        match self.missing().first().copied() {
            None => {
                self.grant = None;
                if self.globe_off() {
                    return self.open_setup(Step::Globe);
                }
                self.checks_done()
            }
            Some(grant) if self.grant != Some(grant) => {
                self.grant = Some(grant);
                (self.waiting_since, self.asked_again) = (Some(Instant::now()), false);
                self.fix(grant, false)
            }
            Some(_) => vec![],
        }
    }
    /// After any permission news: advance the open step, or offer it once when
    /// the TUI opens on a configured install that lacks a grant.
    pub fn permissions_changed(&mut self) -> Vec<Effect> {
        if matches!(self.modal, Some(Modal::Setup(Step::Permissions))) {
            return self.permissions_step();
        }
        if self.permissions_offered || self.modal.is_some() || self.disk.is_none() {
            return vec![];
        }
        let missing = self.missing();
        // The microphone is only known by asking: check it once here, so a reinstall
        // settles it now rather than at the first dictation. Allowed shows nothing.
        if missing == [Grant::Microphone] && self.microphone.is_none() {
            if std::mem::replace(&mut self.mic_checked, true) {
                return vec![];
            }
            return vec![Effect::Request(
                Request::Allow {
                    grant: Grant::Microphone,
                },
                Tag::Permissions(Some(Grant::Microphone)),
            )];
        }
        // Some keep the Globe key on purpose: it is only asked in setup and after installing.
        if missing.is_empty() && !(self.globe_off() && self.start.is_some()) {
            return vec![];
        }
        self.permissions_offered = true;
        self.open_setup(Step::Permissions)
    }
    /// Re-check on a short timer only while a screen waits for a switch.
    pub fn watching(&self) -> bool {
        self.connected
            && match &self.modal {
                Some(Modal::Setup(Step::Permissions | Step::Globe)) => true,
                None => {
                    self.page == Page::Doctor
                        && (self.missing().iter().any(|g| *g != Grant::Microphone)
                            || self.permissions.as_ref().is_some_and(|p| {
                                p.accessibility
                                    && !p.input_monitoring
                                    && p.keyboard_events == Some(false)
                            })
                            || self.waiting_for_microphone())
                }
                _ => false,
            }
    }
    /// The microphone is only known by asking xflowd, so a pending fix keeps asking.
    fn waiting_for_microphone(&self) -> bool {
        self.grant == Some(Grant::Microphone) && self.microphone != Some(true)
    }
    pub fn poll(&mut self) -> Vec<Effect> {
        if matches!(self.modal, Some(Modal::Setup(Step::Globe))) {
            return vec![Effect::Globe];
        }
        let grant = Grant::Microphone;
        let screen = match self.modal {
            Some(Modal::Setup(Step::Permissions)) => true,
            None => self.page == Page::Doctor,
            _ => false,
        };
        if screen && self.waiting_for_microphone() {
            if std::mem::replace(&mut self.asking, true) {
                return vec![];
            }
            return vec![Effect::Request(
                Request::Allow { grant },
                Tag::Permissions(Some(grant)),
            )];
        }
        vec![Effect::Request(Request::Status, Tag::Permissions(None))]
    }
    /// Why the background helper isn't connected, the same on every screen.
    pub fn helper_down(&self) -> String {
        if self.downs == 0 && !self.restarting {
            "Connecting…".into()
        } else if self.restarting {
            "Restarting xflow…".into()
        } else if self.start_failed {
            let retry = if self.retry_here() {
                " Press Enter to try again."
            } else {
                ""
            };
            format!("xflow couldn't start its background helper.{retry}")
        } else {
            "Starting xflow…".into()
        }
    }
    /// Enter retries a failed start on screens where Enter has no better use.
    fn retry_here(&self) -> bool {
        self.start_failed
            && !self.connected
            && matches!(self.modal, None | Some(Modal::Setup(Step::Permissions)))
    }
    /// Lost (or never had) the helper: start it once per outage through launchd,
    /// unless an ask-again restart explains the first disconnect.
    pub fn disconnected(&mut self) -> Vec<Effect> {
        self.downs += 1;
        if self.downs == 1 + u32::from(self.restarting) {
            self.restarting = false;
            return vec![Effect::StartHelper];
        }
        vec![]
    }
    /// (key, display value) for every visible settings row.
    pub fn settings(&self) -> Vec<(String, String)> {
        let rows = options::SETTINGS.iter().flat_map(|row| {
            let advanced = if row.0 == options::ADVANCED && self.advanced {
                options::ADVANCED_SETTINGS
            } else {
                &[]
            };
            std::iter::once(row).chain(advanced)
        });
        rows.map(|(key, ..)| {
            let value = match *key {
                "action.setup" => if self.needs_setup() {
                    "Start here"
                } else {
                    "Run again"
                }
                .into(),
                "action.stt_key" => {
                    if matches!(self.config.stt.provider.as_str(), "local" | "custom") {
                        "Not needed"
                    } else if self.key_locked {
                        "Waiting for Keychain — click Always Allow"
                    } else if self.missing_key {
                        "Missing — add one"
                    } else {
                        "Added"
                    }
                    .into()
                }
                "action.cleanup_key" => if self.config.cleanup.provider.is_none() {
                    "Choose a service first"
                } else if self.cleanup_key_locked {
                    "Waiting for Keychain — click Always Allow"
                } else if self.missing_cleanup_key {
                    "Missing — add one"
                } else {
                    "Added"
                }
                .into(),
                "action.stt_test" => "Run check".into(),
                options::ADVANCED => if self.advanced { "Hide" } else { "Show" }.into(),
                _ => match self.effective(key) {
                    Some(value) => value_text(key, &value),
                    None => value_text(key, ""),
                },
            };
            (key.to_string(), value)
        })
        .collect()
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
            // Pick up edits made elsewhere (CLI, editor) before showing values.
            Page::Settings if !self.dirty && !self.saving => vec![Effect::Load(self.path.clone())],
            Page::Settings => vec![Effect::KeyStatus(self.config.clone())],
            Page::Overlay => vec![Effect::Overlay],
            Page::Doctor => vec![
                Effect::Doctor,
                Effect::Request(Request::Status, Tag::Permissions(None)),
            ],
            Page::Home => vec![
                Effect::Request(Request::Status, Tag::Ordinary),
                Effect::Request(Request::Last, Tag::Last),
            ],
            _ => vec![Effect::Request(Request::Status, Tag::Ordinary)],
        }
    }
    pub fn response(&mut self, response: Response, tag: Tag) -> Vec<Effect> {
        // Only the start that began capture owns it; another start's refusal doesn't.
        if matches!(tag, Tag::Session) && response.ok && response.state == State::Listening {
            self.session_pending = true;
        }
        if matches!(tag, Tag::Permissions(Some(Grant::Microphone))) {
            self.asking = false;
        }
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
        if !response.ok && matches!(tag, Tag::Last) {
            return vec![]; // Nothing dictated yet is not an error.
        }
        if !response.ok {
            let message = response
                .message
                .unwrap_or_else(|| "xflow couldn't do that".into());
            self.toast = Some(match tag {
                Tag::Reload
                    if message.contains("idle daemon")
                        || message.starts_with(crate::daemon::MIC_WAITING) =>
                {
                    self.reload_pending = true;
                    "Saved — applies when this dictation finishes.".into()
                }
                Tag::Reload => format!("Saved, but not applied yet: {}", options::plain(&message)),
                _ => options::plain(&message),
            });
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
            Tag::Permissions(grant) => {
                let mut effects = vec![];
                if response.permissions.is_some() {
                    self.take_permissions(response.permissions.clone());
                }
                if grant == Some(Grant::Microphone) {
                    self.microphone = response.permissions.and_then(|p| p.microphone);
                    if self.microphone.is_some()
                        && std::mem::take(&mut self.mic_pane)
                        && self.microphone == Some(false)
                    {
                        effects.push(Effect::Open(Grant::Microphone.pane()));
                    }
                }
                effects.extend(self.permissions_changed());
                return effects;
            }
            Tag::Reload => {
                if std::mem::take(&mut self.reload_pending) {
                    return vec![Effect::Request(Request::Reload, Tag::Reload)];
                }
                // The mic check starts only once the daemon listens to the chosen device.
                if matches!(self.modal, Some(Modal::Setup(Step::MicCheck))) {
                    return mic_check(&self.config);
                }
            }
            Tag::Done(text) => {
                let changed = response.entry.is_some();
                self.toast = Some(
                    response
                        .message
                        .map(|m| options::plain(&m))
                        .unwrap_or_else(|| text.into()),
                );
                // A corrected transcript replies with its updated entry.
                if changed && self.page == Page::History {
                    return self.refresh();
                }
            }
            Tag::Last => {
                if self.transcript.is_empty() {
                    self.transcript = response.text.unwrap_or_default();
                }
            }
            Tag::Ordinary | Tag::Session => {
                // The subscription owns session state; a delayed command reply
                // must never roll a newer state event back to an older state.
                if let Some(text) = response.text {
                    self.transcript = text;
                }
                if let Some(message) = response.message {
                    self.toast = Some(options::plain(&message));
                }
            }
        }
        vec![]
    }
    pub fn event(&mut self, mut response: Response) {
        if !matches!(response.state, State::Listening | State::Processing)
            && matches!(self.status.state, State::Listening | State::Processing)
        {
            self.session_pending = false;
        }
        if let Some(text) = response.text.take() {
            self.transcript = text;
            self.scroll = 0;
        }
        if let Some(timings) = response.timings.take() {
            self.timings = Some(timings);
        }
        if response.permissions.is_some() {
            self.take_permissions(response.permissions.clone());
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
        self.typed = Some(Instant::now());
        if self.retry_here() && key.code == KeyCode::Enter {
            self.start_failed = false;
            return vec![Effect::StartHelper];
        }
        if let Some(modal) = self.modal.take() {
            return self.modal_key(modal, key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('p') => self.action(Action::Palette),
                KeyCode::Char('c') => self.action(Action::Quit),
                _ => vec![],
            };
        }
        let action = match key.code {
            KeyCode::Char(c @ '1'..='9') => Action::Go(PAGES[(c as u8 - b'1') as usize]),
            KeyCode::Tab => Action::Go(PAGES[(self.page.index() + 1) % PAGES.len()]),
            KeyCode::BackTab => {
                Action::Go(PAGES[(self.page.index() + PAGES.len() - 1) % PAGES.len()])
            }
            KeyCode::Up | KeyCode::Char('k') => return self.select(-1),
            KeyCode::Down | KeyCode::Char('j') => return self.select(1),
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
            KeyCode::Char('x') => Action::Cancel,
            KeyCode::Esc => Action::Back,
            KeyCode::Char('c') => Action::Copy,
            KeyCode::Char('p') => Action::Paste,
            KeyCode::Char('n') | KeyCode::Char('N') => Action::Add,
            KeyCode::Enter => Action::Choose,
            KeyCode::Delete | KeyCode::Backspace => Action::Delete,
            KeyCode::Char('?') => Action::Help,
            KeyCode::Char(':') => Action::Palette,
            KeyCode::Char('r') => Action::Refresh,
            KeyCode::Char('/') => Action::Search,
            KeyCode::Char('q') => Action::Quit,
            _ => return vec![],
        };
        self.action(action)
    }
    fn modal_key(&mut self, mut modal: Modal, key: KeyEvent) -> Vec<Effect> {
        let mut keep = true;
        let mut effects = vec![];
        match &mut modal {
            Modal::Help => match key.code {
                KeyCode::PageDown | KeyCode::Down => self.scroll = self.scroll.saturating_add(4),
                KeyCode::PageUp | KeyCode::Up => self.scroll = self.scroll.saturating_sub(4),
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
                KeyCode::Enter => keep = false,
                KeyCode::Esc => {
                    keep = false;
                    self.query.clear();
                    self.offset = 0;
                    effects = self.refresh();
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.offset = 0;
                    effects = self.refresh();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.clear();
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
            Modal::Setup(step) => {
                let step = *step;
                keep = false;
                effects = self.setup_key(step, key);
            }
            Modal::Editor(editor) => {
                if editor.capturing {
                    if key.code == KeyCode::Esc {
                        editor.capturing = false;
                    } else if let Some(chord) = options::chord(key) {
                        let field = &mut editor.fields[editor.focus];
                        if !field.choices.contains(&chord) {
                            field.choices.push(chord.clone());
                            field.labels.push(options::key_label(&chord));
                        }
                        *field.value = chord;
                        editor.capturing = false;
                        editor.error = None;
                    } else {
                        editor.error = Some(
                            "Your terminal keeps that key for itself. Choose a key from the list or try another combination."
                                .into(),
                        );
                    }
                } else {
                    let last = editor.focus + 1 == editor.fields.len();
                    let picking = editor.picking();
                    match key.code {
                        KeyCode::Esc => {
                            keep = false;
                            if matches!(editor.target, EditTarget::Setup(_)) {
                                self.setup = None;
                            }
                        }
                        KeyCode::Enter
                            if picking
                                && editor.fields[editor.focus].value.as_str()
                                    == options::CAPTURE =>
                        {
                            editor.capturing = true;
                            editor.error = None;
                        }
                        KeyCode::Enter
                            if !last && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            editor.focus += 1
                        }
                        KeyCode::Enter | KeyCode::F(2) => match self.apply_editor(editor) {
                            Ok(next) => {
                                keep = false;
                                effects = next;
                            }
                            Err(error) => {
                                editor.error = Some(options::plain(&format!("{error:#}")))
                            }
                        },
                        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            match self.apply_editor(editor) {
                                Ok(next) => {
                                    keep = false;
                                    effects = next;
                                }
                                Err(error) => {
                                    editor.error = Some(options::plain(&format!("{error:#}")))
                                }
                            }
                        }
                        KeyCode::Tab => editor.focus = (editor.focus + 1) % editor.fields.len(),
                        KeyCode::BackTab => {
                            editor.focus =
                                (editor.focus + editor.fields.len() - 1) % editor.fields.len()
                        }
                        KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Home
                        | KeyCode::End
                            if picking =>
                        {
                            let field = &mut editor.fields[editor.focus];
                            let count = field.choices.len();
                            let pos = field.position();
                            let pos = match key.code {
                                KeyCode::Home => 0,
                                KeyCode::End => count - 1,
                                KeyCode::Down | KeyCode::Right => (pos + 1).min(count - 1),
                                _ => pos.saturating_sub(1),
                            };
                            *field.value = field.choices[pos].clone();
                        }
                        // Choice fields never accept typing.
                        _ if picking => (),
                        KeyCode::Up if editor.focus > 0 => editor.focus -= 1,
                        KeyCode::Down if !last => editor.focus += 1,
                        _ => text_key(&mut editor.fields[editor.focus], key),
                    }
                }
            }
        }
        if keep {
            self.modal = Some(modal);
        }
        effects
    }
    /// Mouse click on an option row in the open picker: choose it.
    pub fn pick(&mut self, index: usize) -> Vec<Effect> {
        if let Some(Modal::Editor(editor)) = &mut self.modal {
            let field = &mut editor.fields[editor.focus];
            if let Some(choice) = field.choices.get(index) {
                *field.value = choice.clone();
                return self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
        }
        vec![]
    }
    pub fn paste(&mut self, text: &str) {
        if let Some(Modal::Editor(editor)) = &mut self.modal {
            let field = &mut editor.fields[editor.focus];
            if !field.choices.is_empty() || editor.capturing {
                return;
            }
            let multiline = !field.secret;
            let text = if field.secret { text.trim() } else { text };
            if field.value.len() + text.len() <= 16 * 1024 {
                // Grow once so a pasted secret never leaves reallocated copies behind.
                field.value.reserve(text.len());
                for c in text
                    .chars()
                    .filter(|c| !c.is_control() || (multiline && (*c == '\n' || *c == '\t')))
                {
                    field.insert(c);
                }
            } else {
                editor.error = Some("That's too long to paste here.".into());
            }
        }
    }
    fn edit(
        &mut self,
        target: EditTarget,
        title: &str,
        intro: impl Into<String>,
        fields: Vec<Field>,
    ) {
        self.modal = Some(Modal::Editor(Box::new(Editor {
            target,
            title: title.into(),
            intro: intro.into(),
            fields,
            focus: 0,
            error: None,
            capturing: false,
        })));
    }
    fn editable(&mut self) -> bool {
        if self.saving || self.file.is_none() {
            self.toast = Some("One moment — saving your last change.".into());
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
            Action::Back => {
                if self.active() && self.page == Page::Home {
                    return self.action(Action::Cancel);
                } else if self.page == Page::History && !self.query.is_empty() {
                    self.query.clear();
                    self.offset = 0;
                    return self.refresh();
                } else if self.page != Page::Home {
                    return self.action(Action::Go(Page::Home));
                }
            }
            Action::Copy | Action::Paste => {
                let copy = action == Action::Copy;
                let request = match self.page {
                    Page::History => match self.history.get(self.selected()) {
                        Some(h) if copy => Request::HistoryCopy { id: h.id },
                        Some(h) => Request::HistoryPaste { id: h.id },
                        None => return vec![],
                    },
                    Page::Home if copy => Request::CopyLast,
                    Page::Home => Request::PasteLast,
                    _ => return vec![],
                };
                let done = if copy {
                    "Copied — paste it anywhere."
                } else {
                    "Pasted."
                };
                return vec![Effect::Request(request, Tag::Done(done))];
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
            Action::Quit => {
                if self.saving {
                    self.toast = Some("One moment — saving your last change.".into());
                } else if self.dirty {
                    self.modal = Some(Modal::Confirm {
                        action,
                        prompt: "Your last change wasn't saved. Quit anyway?".into(),
                    });
                } else {
                    self.quit = true;
                }
            }
            Action::Delete => {
                let pos = self.selected();
                let deletable = match self.page {
                    Page::History => pos < self.history.len(),
                    Page::Dictionary | Page::Snippets | Page::Styles => {
                        pos >= self.adds() && pos < self.rows().len()
                    }
                    _ => false,
                };
                if deletable {
                    let row = &self.rows()[pos].0;
                    let mut name = row.chars().take(40).collect::<String>();
                    if name.len() < row.len() {
                        name.push('…');
                    }
                    self.modal = Some(Modal::Confirm {
                        action,
                        prompt: format!("Delete “{name}”? This can't be undone."),
                    });
                }
            }
            Action::Help => {
                self.scroll = 0;
                self.modal = Some(Modal::Help);
            }
            Action::Palette => {
                self.modal = Some(Modal::Palette {
                    query: String::new(),
                    selected: 0,
                })
            }
            Action::Search => {
                if self.page != Page::History {
                    self.page = Page::History;
                    self.modal = Some(Modal::Search);
                    return self.refresh();
                }
                self.modal = Some(Modal::Search);
            }
            Action::NextPage if self.page == Page::History => {
                if ((self.offset + PAGE_SIZE) as u64) < self.total {
                    self.offset += PAGE_SIZE;
                    self.selected[Page::History.index()] = 0;
                    return self.refresh();
                }
            }
            Action::PrevPage if self.page == Page::History => {
                self.offset = self.offset.saturating_sub(PAGE_SIZE);
                self.selected[Page::History.index()] = 0;
                return self.refresh();
            }
            Action::Refresh => return self.refresh(),
            Action::Setup => return self.open_setup(Step::Welcome),
            Action::Choose => return self.choose(),
            Action::Add | Action::AddCorrection | Action::Edit => return self.open_editor(action),
            _ => (),
        }
        vec![]
    }
    fn choose(&mut self) -> Vec<Effect> {
        let pos = self.selected();
        match self.page {
            Page::Home if self.needs_setup() => return self.open_setup(Step::Welcome),
            Page::Home => return self.action(Action::Toggle),
            Page::History => {
                if let Some(h) = self.history.get(pos) {
                    let preview = h.text.trim().chars().take(60).collect::<String>();
                    self.menu(
                        &preview,
                        vec![
                            (Action::Copy, "Copy"),
                            (Action::Paste, "Paste into the app you were using"),
                            (Action::Edit, "Correct this transcript"),
                            (Action::Delete, "Delete"),
                        ],
                    );
                }
            }
            Page::Dictionary | Page::Snippets | Page::Styles => {
                let adds = self.adds();
                if pos < adds {
                    let action = if pos == 1 {
                        Action::AddCorrection
                    } else {
                        Action::Add
                    };
                    return self.open_editor(action);
                }
                if let Some((name, _)) = self.rows().get(pos) {
                    let name = name.clone();
                    self.menu(
                        &name,
                        vec![(Action::Edit, "Edit"), (Action::Delete, "Delete")],
                    );
                }
            }
            Page::Settings => return self.open_setting(pos),
            Page::Doctor => {
                if let Some(grant) = self.doctor_grant(pos) {
                    return self.fix(grant, true);
                }
            }
            Page::Overlay => {
                if let Some(setting) = self.overlay.get(pos).cloned() {
                    let choices = overlay_choices(&setting);
                    if choices.is_empty() {
                        self.toast = Some(
                            "This one can only be changed from the pill's own preferences.".into(),
                        );
                    } else {
                        self.edit(
                            EditTarget::Overlay(setting.key.clone()),
                            &setting.summary,
                            "Applies right away.",
                            vec![Field::pick(
                                "",
                                setting.value.clone(),
                                choices,
                                overlay_value,
                            )],
                        );
                    }
                }
            }
            _ => (),
        }
        vec![]
    }
    fn menu(&mut self, title: &str, items: Vec<(Action, &str)>) {
        let choices = (0..items.len()).map(|i| i.to_string()).collect();
        let labels: Vec<String> = items.iter().map(|(_, l)| l.to_string()).collect();
        self.edit(
            EditTarget::Menu(items.iter().map(|(a, _)| *a).collect()),
            title,
            "",
            vec![Field::pick("", "0", choices, |i| {
                labels[i.parse::<usize>().unwrap_or(0)].clone()
            })],
        );
    }
    fn open_setting(&mut self, pos: usize) -> Vec<Effect> {
        let Some((key, _)) = self.settings().into_iter().nth(pos) else {
            return vec![];
        };
        let cleanup = key.starts_with("action.cleanup");
        match key.as_str() {
            "action.setup" => return self.open_setup(Step::Welcome),
            options::ADVANCED => self.advanced = !self.advanced,
            "action.stt_test" => return vec![Effect::CheckProvider(self.config.clone(), false)],
            "action.stt_key" | "action.cleanup_key" => {
                let id = if cleanup {
                    match self.config.cleanup.provider.clone() {
                        Some(id) => id,
                        None => {
                            self.toast = Some("Choose a writing service first.".into());
                            return vec![];
                        }
                    }
                } else {
                    self.config.stt.provider.clone()
                };
                self.key_editor(id, cleanup, false);
            }
            "recording.device" => return vec![Effect::Devices],
            _ => {
                if !self.editable() {
                    return vec![];
                }
                let field = self.setting_field(&key);
                let (_, group, label, help) =
                    options::row(&key).copied().unwrap_or(("", "", "", ""));
                self.edit(
                    EditTarget::Setting(key),
                    &format!("{group} · {label}"),
                    help,
                    vec![field],
                );
            }
        }
        vec![]
    }
    fn key_editor(&mut self, id: String, cleanup: bool, setup: bool) {
        let provider = integrations::catalog(cleanup)
            .into_iter()
            .find(|p| p.id == id);
        let name = provider
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.clone());
        let url = provider.map(|p| p.key_url).filter(|u| !u.is_empty());
        let mut intro = format!("Paste your {name} API key.");
        if let Some(url) = url {
            intro.push_str(&format!(" Get one at {url}"));
        }
        intro.push_str("\nIt's kept in your system keychain, never in a file.");
        if setup {
            intro.push_str("\nAlready added one? Leave this empty and press Enter.");
        }
        let mut field = Field::new("API key", "");
        field.secret = true;
        let target = if setup {
            EditTarget::Setup(Step::Key)
        } else {
            EditTarget::Key { id, cleanup }
        };
        self.edit(target, &format!("{name} API key"), intro, vec![field]);
    }
    pub fn open_setup(&mut self, step: Step) -> Vec<Effect> {
        self.setup = Some(step);
        self.setup_offered = true;
        match step {
            Step::Welcome | Step::MicCheck | Step::Try | Step::Done => {
                self.modal = Some(Modal::Setup(step))
            }
            Step::Permissions => {
                self.modal = Some(Modal::Setup(step));
                self.grant = None;
                return self.permissions_step();
            }
            Step::Globe => {
                self.modal = Some(Modal::Setup(step));
                return vec![Effect::Open(KEYBOARD_PANE)];
            }
            Step::Provider => {
                let choices = integrations::catalog(false)
                    .into_iter()
                    .map(|p| p.id)
                    .filter(|id| id != "custom" && id != "local")
                    .collect();
                self.edit(
                    EditTarget::Setup(step),
                    "Step 1 of 4 · Speech service",
                    "Choose the service that turns your voice into text. You can change this later.",
                    vec![Field::pick("", self.config.stt.provider.clone(), choices, |v| value_text("stt.provider", v))],
                );
            }
            Step::Key => {
                let id = self.config.stt.provider.clone();
                self.key_editor(id, false, true);
                if let Some(Modal::Editor(editor)) = &mut self.modal {
                    editor.title = format!("Step 2 of 4 · {}", editor.title);
                }
            }
            Step::Microphone => {
                // Show the step at once; the device list replaces it when it arrives.
                self.devices(vec![]);
                return vec![Effect::Devices];
            }
            Step::Shortcut => {
                let current = self
                    .effective("hotkey.key")
                    .map(|v| v.trim_matches('"').to_string())
                    .unwrap_or_else(|| "fn".into());
                self.edit(
                    EditTarget::Setup(step),
                    "Step 4 of 4 · Shortcut",
                    "Hold this key and speak anywhere; let go to finish. Double-tap it for hands-free.",
                    vec![Field::pick("", current, options::presets("hotkey.key"), options::key_label)],
                );
            }
        }
        vec![]
    }
    fn setup_key(&mut self, step: Step, key: KeyEvent) -> Vec<Effect> {
        let cancel = || Effect::Request(Request::Cancel, Tag::Ordinary);
        match (step, key.code) {
            (_, KeyCode::Esc) => {
                self.setup = None;
                return if self.active() {
                    vec![cancel()]
                } else {
                    vec![]
                };
            }
            (Step::Welcome, KeyCode::Enter) => return self.open_setup(Step::Provider),
            (Step::Globe, KeyCode::Enter) => return self.open_setup(step),
            // Ask again: after removing old xflowd rows, macOS lists this one anew.
            (Step::Permissions, KeyCode::Enter) => {
                return match self.missing().first().copied() {
                    Some(grant) => {
                        self.modal = Some(Modal::Setup(step));
                        self.grant = Some(grant);
                        self.fix(grant, true)
                    }
                    None => self.open_setup(step),
                }
            }
            (Step::MicCheck, KeyCode::Enter) => {
                let mut effects = if self.active() {
                    vec![cancel()]
                } else {
                    vec![]
                };
                effects.extend(self.open_setup(Step::Shortcut));
                return effects;
            }
            (Step::Try, KeyCode::Char(' ')) => {
                self.modal = Some(Modal::Setup(step));
                return vec![Effect::Request(
                    Request::Toggle {
                        mode: Mode::Dictation,
                        context: None,
                        t0_us: None,
                        delivery: Delivery::None,
                    },
                    Tag::Ordinary,
                )];
            }
            (Step::Try, KeyCode::Enter) => {
                let effects = if self.active() {
                    vec![cancel()]
                } else {
                    vec![]
                };
                self.open_setup(Step::Done);
                return effects;
            }
            (Step::Done, _) => {
                self.setup = None;
                self.page = Page::Home;
                return vec![];
            }
            _ => self.modal = Some(Modal::Setup(step)),
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
    fn choose_provider(&mut self, id: &str, cleanup: bool, model: Option<&str>) -> Result<()> {
        if self.saving {
            bail!("One moment — saving your last change");
        }
        let config = integrations::selection(&self.config, id, cleanup, model)?;
        let before = toml::Value::try_from(&self.config)?;
        let after = toml::Value::try_from(&config)?;
        let section = if cleanup { "cleanup" } else { "stt" };
        let fields = if cleanup {
            vec!["provider", "endpoint", "model", "api_key_env"]
        } else {
            vec!["provider", "endpoint", "model", "api_key_env", "protocol"]
        };
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| anyhow!("Settings are still loading"))?;
        // Edit only changed leaves so existing comments and other settings survive.
        let mut changed: Vec<(String, Option<String>)> = Vec::new();
        for field in fields {
            if before[section].get(field) == after[section].get(field) {
                continue;
            }
            let key = format!("{section}.{field}");
            let old = file.get(&key);
            let result = match after[section].get(field) {
                Some(value) => file.set_typed(&key, value),
                None => file.unset(&key).map(|_| ()),
            };
            if let Err(error) = result {
                for (key, old) in changed.into_iter().rev() {
                    match old {
                        Some(value) => file.set(&key, &value)?,
                        None => {
                            file.unset(&key)?;
                        }
                    }
                }
                return Err(error);
            }
            changed.push((key, old));
        }
        self.sync_config();
        Ok(())
    }
    /// Turning cleanup on without a writing service reuses the speech service
    /// when it also offers cleanup, so the same key keeps working.
    fn ensure_cleanup_service(&mut self) -> Result<()> {
        if let Some(id) = cleanup_fallback(&self.config)? {
            self.choose_provider(&id, true, None)?;
        }
        Ok(())
    }
    fn set_optional(&mut self, key: &str, value: &str) -> Result<()> {
        if value.is_empty() {
            if !self.editable() {
                bail!("Settings are still loading");
            }
            self.file.as_mut().unwrap().unset(key)?;
            self.sync_config();
            Ok(())
        } else {
            self.set(key, value)
        }
    }
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        if self.saving {
            bail!("One moment — saving your last change");
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| anyhow!("Settings are still loading"))?;
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
    /// Stage a setting chosen from a list.
    fn set_choice(&mut self, key: &str, value: &str) -> Result<()> {
        let result = match key {
            "stt.provider" | "cleanup.provider" => {
                self.choose_provider(value, key == "cleanup.provider", None)
            }
            "stt.model" | "cleanup.model" if !value.is_empty() => {
                let cleanup = key == "cleanup.model";
                let id = if cleanup {
                    self.config.cleanup.provider.clone().unwrap_or_default()
                } else {
                    self.config.stt.provider.clone()
                };
                self.choose_provider(&id, cleanup, Some(value))
            }
            _ => self.set_optional(key, value),
        };
        result?;
        if key == "cleanup.mode" {
            if let Err(error) = self.ensure_cleanup_service() {
                self.set_optional(key, "raw")?;
                return Err(error);
            }
        }
        Ok(())
    }
    fn open_editor(&mut self, action: Action) -> Vec<Effect> {
        if self.page == Page::History {
            if let Some(h) = self
                .history
                .get(self.selected())
                .filter(|_| action == Action::Edit)
            {
                let (id, text) = (h.id, h.text.clone());
                self.edit(
                    EditTarget::Transcript(id),
                    "Correct transcript",
                    "Fix any misheard words. xflow saves the change and learns the corrected words.",
                    vec![Field::new("Transcript (Ctrl+J for a new line)", text)],
                );
            }
            return vec![];
        }
        if !self.editable() {
            return vec![];
        }
        let new = action != Action::Edit;
        let pos = self.selected().saturating_sub(self.adds());
        match self.page {
            Page::Dictionary => {
                let words = self.config.dictionary.words.len();
                if action == Action::AddCorrection || (!new && pos >= words) {
                    let index = (!new).then_some(pos.saturating_sub(words));
                    let r = index.and_then(|i| self.config.dictionary.replacements.get(i));
                    let fields = vec![
                        Field::new("When I say", r.map(|r| r.from.as_str()).unwrap_or("")),
                        Field::new("Write", r.map(|r| r.to.as_str()).unwrap_or("")),
                    ];
                    self.edit(
                        EditTarget::Replacement(index),
                        "Correction",
                        "Whenever xflow hears the first phrase, it writes the second instead. Leave “Write” empty to drop the phrase.",
                        fields,
                    );
                } else {
                    let index = (!new).then_some(pos);
                    let word = index
                        .and_then(|i| self.config.dictionary.words.get(i))
                        .cloned()
                        .unwrap_or_default();
                    self.edit(
                        EditTarget::Word(index),
                        "Word",
                        "A name, brand or term xflow should recognize and spell exactly like this.",
                        vec![Field::new("Word", word)],
                    );
                }
            }
            Page::Snippets => {
                let index = (!new).then_some(pos);
                let s = index.and_then(|i| self.config.snippets.get(i));
                let fields = vec![
                    Field::new("When I say", s.map(|s| s.trigger.as_str()).unwrap_or("")),
                    Field::new(
                        "Insert (Ctrl+J for a new line)",
                        s.map(|s| s.text.as_str()).unwrap_or(""),
                    ),
                ];
                self.edit(
                    EditTarget::Snippet(index),
                    "Snippet",
                    "Say a short phrase while dictating and xflow inserts the full text.",
                    fields,
                );
            }
            Page::Styles => {
                let index = (!new).then_some(pos);
                let style = index.and_then(|i| self.config.styles.get(i));
                let mut apps: Vec<String> = [
                    "com.apple.mail",
                    "com.apple.Notes",
                    "com.apple.Safari",
                    "com.google.Chrome",
                    "com.tinyspeck.slackmacgap",
                    "com.microsoft.VSCode",
                    "slack",
                    "discord",
                    "telegram",
                    "code",
                    "firefox",
                    "thunderbird",
                    "org.gnome.Terminal",
                ]
                .into_iter()
                .map(String::from)
                .collect();
                apps.extend(self.history.iter().filter_map(|h| h.app_id.clone()));
                apps.extend(
                    self.config
                        .styles
                        .iter()
                        .flat_map(|s| s.apps.iter().cloned()),
                );
                apps.sort();
                apps.dedup();
                let current_apps = style
                    .map(|s| s.apps.join(", "))
                    .unwrap_or_else(|| apps[0].clone());
                let mut modes: Vec<String> = ["inherit", "raw", "light", "polished"]
                    .map(String::from)
                    .to_vec();
                if style.and_then(|s| s.prompt.as_deref()).is_some() {
                    modes.push("custom".into());
                }
                let fields = vec![
                    Field::pick(
                        "Name",
                        style
                            .map(|s| s.name.clone())
                            .unwrap_or_else(|| "Chat".into()),
                        ["Chat", "Email", "Notes", "Code", "Formal"]
                            .map(String::from)
                            .to_vec(),
                        str::to_string,
                    ),
                    Field::pick("App", current_apps, apps, |a| {
                        a.split(", ")
                            .map(options::app_name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    }),
                    Field::pick(
                        "Cleanup",
                        style
                            .and_then(|s| s.mode)
                            .map(mode_label)
                            .unwrap_or("inherit"),
                        modes,
                        |m| {
                            if m == "inherit" {
                                "Same as Settings".into()
                            } else {
                                value_text("cleanup.mode", m)
                            }
                        },
                    ),
                    Field::pick(
                        "Tone",
                        style.and_then(|s| s.prompt.as_deref()).unwrap_or(""),
                        [
                            "",
                            "Use a friendly conversational tone.",
                            "Use professional, concise language.",
                            "Preserve technical terms and code identifiers.",
                        ]
                        .map(String::from)
                        .to_vec(),
                        |t| {
                            if t.is_empty() {
                                "No extra instructions".into()
                            } else {
                                t.into()
                            }
                        },
                    ),
                ];
                self.edit(
                    EditTarget::Style(index),
                    "Style",
                    "Pick an app and how text written there should sound. The first matching style wins.",
                    fields,
                );
            }
            _ => (),
        }
        vec![]
    }
    fn apply_editor(&mut self, editor: &mut Editor) -> Result<Vec<Effect>> {
        // Move secrets directly into a zeroizing payload; never clone them into
        // ordinary form strings or surface provider errors containing the key.
        let secret = |editor: &mut Editor| {
            let raw = Zeroizing::new(std::mem::take(&mut *editor.fields[0].value));
            Zeroizing::new(raw.trim().to_owned())
        };
        match editor.target.clone() {
            EditTarget::Key { id, cleanup } => {
                if editor.fields[0].value.trim().is_empty() {
                    bail!("Paste your API key first");
                }
                // A custom cleanup server has its own keyring entry, never the STT one.
                let id = if cleanup {
                    xflow_providers::cleanup_key_id(&id).into()
                } else {
                    id
                };
                return Ok(vec![Effect::SaveKey(id, secret(editor))]);
            }
            EditTarget::Setup(Step::Key) => {
                let mut effects = vec![];
                if !editor.fields[0].value.trim().is_empty() {
                    effects.push(Effect::SaveKey(
                        self.config.stt.provider.clone(),
                        secret(editor),
                    ));
                } else if self.missing_key {
                    bail!("No saved key was found. Paste your key, or press Esc to finish setup later");
                }
                effects.extend(self.open_setup(Step::Microphone));
                return Ok(effects);
            }
            EditTarget::Transcript(id) => {
                let text = editor.fields[0].value.trim().to_owned();
                if text.is_empty() {
                    bail!("Type the corrected transcript first");
                }
                return Ok(vec![Effect::Request(
                    Request::HistoryUpdate { id, text },
                    Tag::Done("Transcript corrected."),
                )]);
            }
            _ => (),
        }
        let values: Vec<_> = editor.fields.iter().map(|f| f.value.to_string()).collect();
        let value = &values[0];
        if values.iter().any(|v| v == options::CAPTURE) {
            bail!("Press Enter, then press the key or combination you want");
        }
        let mut config = self.config.clone();
        let key = match &editor.target {
            EditTarget::Menu(actions) => {
                let action = actions[value.parse::<usize>().unwrap_or(0)];
                return Ok(self.action(action));
            }
            EditTarget::Setup(Step::Provider) => {
                self.choose_provider(value, false, None)?;
                return Ok(self.open_setup(Step::Key));
            }
            EditTarget::Setup(Step::Microphone) => {
                self.set_optional("recording.device", value)?;
                self.modal = Some(Modal::Setup(Step::MicCheck));
                self.setup = Some(Step::MicCheck);
                // Unchanged device: no save/reload will follow, so start the check now.
                return Ok(if self.dirty {
                    vec![]
                } else {
                    mic_check(&self.config)
                });
            }
            EditTarget::Setup(Step::Shortcut) => {
                self.set_choice("hotkey.key", value)?;
                return Ok(self.open_setup(Step::Permissions));
            }
            EditTarget::Setup(_) => return Ok(vec![]),
            EditTarget::Setting(key) => {
                let key = key.clone();
                self.set_choice(&key, value)?;
                return Ok(vec![]);
            }
            EditTarget::Device => {
                self.set_optional("recording.device", value)?;
                return Ok(vec![]);
            }
            EditTarget::Key { .. } | EditTarget::Transcript(_) => unreachable!(),
            EditTarget::Overlay(key) => {
                return Ok(vec![Effect::OverlaySet(key.clone(), value.clone())])
            }
            EditTarget::Word(index) => {
                if value.trim().is_empty() {
                    bail!("Type a word first");
                }
                replace(&mut config.dictionary.words, *index, value.trim().into())?;
                "dictionary.words"
            }
            EditTarget::Replacement(index) => {
                if value.trim().is_empty() {
                    bail!("Type what you say first");
                }
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
                if value.trim().is_empty() || values[1].trim().is_empty() {
                    bail!("Fill in both the phrase and the text to insert");
                }
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
                    _ => Some(CleanupMode::Custom),
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
        cleanup_fallback(&config)?;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| anyhow!("Settings are still loading"))?;
        match key {
            "dictionary.words" => file.set_typed(key, &config.dictionary.words)?,
            "dictionary.replacements" => file.set_typed(key, &config.dictionary.replacements)?,
            "snippets" => file.set_typed(key, &config.snippets)?,
            "styles" => file.set_typed(key, &config.styles)?,
            _ => unreachable!(),
        }
        self.sync_config();
        if key == "styles" {
            self.ensure_cleanup_service()?;
        }
        Ok(vec![])
    }
    fn confirmed(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Quit => self.quit = true,
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
                let pos = pos.saturating_sub(self.adds());
                let file = self.file.as_mut().unwrap();
                let result = match self.page {
                    Page::Dictionary => {
                        let mut dictionary = self.config.dictionary.clone();
                        if pos < dictionary.words.len() {
                            // Same as `xflow dictionary remove`: a learned word is never learned again.
                            let word = dictionary.words[pos].clone();
                            dictionary.forget(&word);
                            file.set_typed("dictionary.words", &dictionary.words)
                                .and_then(|()| {
                                    file.set_typed("dictionary.learned", &dictionary.learned)
                                })
                                .and_then(|()| {
                                    file.set_typed(
                                        "dictionary.never_learn",
                                        &dictionary.never_learn,
                                    )
                                })
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
                    Err(e) => self.toast = Some(options::plain(&format!("{e:#}"))),
                };
                let _ = self.select(0);
            }
            _ => (),
        }
        vec![]
    }
    fn setting_field(&self, key: &str) -> Field {
        let value = match self.effective(key) {
            None => String::new(),
            Some(value) => format!("value = {value}")
                .parse::<toml::Table>()
                .ok()
                .and_then(|doc| {
                    doc.get("value")
                        .and_then(toml::Value::as_str)
                        .map(String::from)
                })
                .unwrap_or(value),
        };
        let choices = match key {
            "stt.provider" | "cleanup.provider" => {
                integrations::catalog(key.starts_with("cleanup."))
                    .into_iter()
                    .map(|p| p.id)
                    .filter(|id| id != "custom")
                    .collect()
            }
            "stt.model" | "cleanup.model" => {
                let cleanup = key.starts_with("cleanup.");
                let id = if cleanup {
                    self.config.cleanup.provider.as_deref().unwrap_or_default()
                } else {
                    &self.config.stt.provider
                };
                let mut models = vec![String::new()];
                if let Some(p) = integrations::catalog(cleanup)
                    .into_iter()
                    .find(|p| p.id == id)
                {
                    models.extend(p.models.into_iter().filter(|m| !m.is_empty()));
                }
                models
            }
            "stt.language" => options::LANGUAGES
                .iter()
                .map(|(code, _)| code.to_string())
                .collect(),
            "ui.theme" => BUILTINS
                .iter()
                .map(|s| s.to_string())
                .chain(self.config.ui.themes.keys().cloned())
                .collect(),
            "cleanup.mode" => {
                let mut modes = options::presets(key);
                if self.config.cleanup.prompt.is_some() {
                    modes.push("custom".into());
                }
                modes
            }
            _ => options::presets(key),
        };
        let key = key.to_string();
        Field::pick("", value, choices, move |v| value_text(&key, v))
    }
    pub fn devices(&mut self, mut devices: Vec<String>) {
        devices.insert(0, String::new());
        let current = self.config.recording.device.clone().unwrap_or_default();
        let field = Field::pick("", current, devices, |d| value_text("recording.device", d));
        if self.setup == Some(Step::Microphone) {
            // The user may have opened something else over the placeholder step.
            if self.modal.as_ref().is_some_and(|m| {
                !matches!(m, Modal::Editor(e) if matches!(e.target, EditTarget::Setup(Step::Microphone)))
            }) {
                return;
            }
            self.edit(
                EditTarget::Setup(Step::Microphone),
                "Step 3 of 4 · Microphone",
                "Choose the microphone you'll speak into. Next you'll see a level meter to check it hears you.",
                vec![field],
            );
        } else if self.modal.is_none() {
            // A list arriving after the wizard moved on must not cover its next step.
            self.edit(
                EditTarget::Device,
                "Speech · Microphone",
                options::row("recording.device").map(|r| r.3).unwrap_or(""),
                vec![field],
            );
        }
    }
}
/// The writing service to add when cleanup is needed but none is set: the
/// speech service, when it also offers cleanup without extra setup.
fn cleanup_fallback(config: &Config) -> Result<Option<String>> {
    let needs = config.cleanup.mode != CleanupMode::Raw
        || config
            .styles
            .iter()
            .any(|s| s.mode.is_some_and(|m| m != CleanupMode::Raw));
    if !needs || config.cleanup.provider.is_some() || config.cleanup.endpoint.is_some() {
        return Ok(None);
    }
    let id = &config.stt.provider;
    if id != "custom" && integrations::catalog(true).iter().any(|p| &p.id == id) {
        Ok(Some(id.clone()))
    } else {
        bail!("cleanup endpoint or provider is required")
    }
}
/// Listen without transcribing so the meter can move; Enter cancels before any upload,
/// and a timer cancels before the daemon's own max-length or silence stop would upload.
fn mic_check(config: &Config) -> Vec<Effect> {
    let recording = &config.recording;
    let limit = match recording.auto_stop_secs {
        0 => recording.max_seconds,
        secs => secs.min(recording.max_seconds),
    }
    .min(10);
    vec![
        Effect::Request(
            Request::Start {
                mode: Mode::Dictation,
                context: None,
                t0_us: None,
                delivery: Delivery::None,
            },
            Tag::Ordinary,
        ),
        Effect::MicTimeout(std::time::Duration::from_millis(
            u64::from(limit) * 1000 - 300,
        )),
    ]
}
fn text_key(field: &mut Field, key: KeyEvent) {
    match key.code {
        KeyCode::Left => field.left(),
        KeyCode::Right => field.right(),
        KeyCode::Home => field.cursor = 0,
        KeyCode::End => field.cursor = field.value.len(),
        KeyCode::Delete => field.delete(),
        KeyCode::Backspace => field.backspace(),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            field.value.clear();
            field.cursor = 0;
        }
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) && !field.secret => {
            if field.value.len() < 16 * 1024 {
                field.insert('\n');
            }
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && field.value.len() + c.len_utf8() <= 16 * 1024
                && !c.is_control() =>
        {
            field.insert(c)
        }
        _ => (),
    }
}
fn replace<T>(rows: &mut Vec<T>, index: Option<usize>, value: T) -> Result<()> {
    match index {
        Some(i) => {
            let item = rows
                .get_mut(i)
                .ok_or_else(|| anyhow!("That item no longer exists"))?;
            *item = value;
        }
        None => rows.push(value),
    };
    Ok(())
}
pub fn mode_label(mode: CleanupMode) -> &'static str {
    match mode {
        CleanupMode::Raw => "raw",
        CleanupMode::Light => "light",
        CleanupMode::Polished => "polished",
        CleanupMode::Custom => "custom",
    }
}
/// GSettings values in plain words: `['<Super>space']` → "Super+Space".
pub fn overlay_value(value: &str) -> String {
    let value = value.trim_matches('\'');
    match value {
        "true" => "On".into(),
        "false" => "Off".into(),
        "[]" | "@as []" => "Not set".into(),
        _ if value.starts_with('[') => value
            .trim_matches(|c| c == '[' || c == ']')
            .split(',')
            .map(|s| {
                s.trim()
                    .trim_matches('\'')
                    .replace('>', "+")
                    .replace('<', "")
                    .split('+')
                    .map(|p| {
                        let mut c = p.chars();
                        c.next()
                            .map(|f| f.to_uppercase().chain(c).collect())
                            .unwrap_or_default()
                    })
                    .collect::<Vec<String>>()
                    .join("+")
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ => {
            let text = value.replace('-', " ");
            let mut c = text.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
    }
}
fn overlay_choices(setting: &Setting) -> Vec<String> {
    match &setting.kind {
        Kind::Bool => vec!["true".into(), "false".into()],
        Kind::Choice(choices) => choices.clone(),
        Kind::Int { min, max } => [*min, 0, 100, 200, 300, 500, 1000, *max]
            .into_iter()
            .filter(|v| v >= min && v <= max)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|v| v.to_string())
            .collect(),
        Kind::Double { min, max } => [*min, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, *max]
            .into_iter()
            .filter(|v| v >= min && v <= max)
            .map(|v| v.to_string())
            .collect(),
        Kind::Type(kind) if kind == "as" => [
            "[]",
            "['<Super>space']",
            "['<Control><Alt>space']",
            "['<Shift><Super>space']",
        ]
        .map(String::from)
        .to_vec(),
        _ => vec![],
    }
}
