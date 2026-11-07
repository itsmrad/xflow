//! Chat page state and its pure reducer: no IO and no rendering. Everything here lives
//! in memory only; `chat_view.rs` draws it and `runtime.rs` runs the turns.
use super::{
    options::glyph,
    state::{key_issue, App, EditTarget, Editor, Effect, Field, Modal},
};
use crate::chat::{consent_text, TurnEvent, MAX_QUESTION_BYTES};
use anyhow::{bail, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use std::{
    cell::RefCell,
    path::PathBuf,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use xflow_core::config::{AiConfig, Config};
use xflow_providers::{
    chat_providers, find_provider,
    llm::{build_provider, Event, LlmError, Message, Stop},
    KeySource, ProviderKind,
};
use zeroize::Zeroizing;

/// Streaming draws at most this often (under 30 a second).
pub const FRAME: Duration = Duration::from_millis(34);
/// A paste over this is refused whole; the send limit is `MAX_QUESTION_BYTES`.
const INPUT_BYTES: usize = 1024 * 1024;
/// A key this soon after the consent modal opens was aimed at the input, not the modal.
pub const CONSENT_GUARD: Duration = Duration::from_millis(500);
const LOCAL: &str = "custom";
const LOCAL_ENDPOINT: &str = "http://127.0.0.1:11434/v1/chat/completions";
const RETRY: &str = "Ctrl+R retries.";

/// One row of the setup list. Harness providers join this list as more rows.
#[derive(Clone, Copy, Debug)]
pub struct Choice {
    pub id: &'static str,
    pub name: &'static str,
    pub model: &'static str,
    pub key_url: &'static str,
    pub needs_key: bool,
}
pub fn choices() -> Vec<Choice> {
    chat_providers()
        .iter()
        .map(|p| Choice {
            id: p.id,
            name: p.name,
            model: p.default_model,
            key_url: p.key_url,
            needs_key: true,
        })
        .chain([Choice {
            id: LOCAL,
            name: "Local (on this computer)",
            model: "",
            key_url: "",
            needs_key: false,
        }])
        .chain(
            [
                ("claude-code", "Claude Code"),
                ("codex", "Codex (isolation unverified)"),
                ("cursor", "Cursor (isolation unverified)"),
            ]
            .map(|(id, name)| Choice {
                id,
                name,
                model: "",
                key_url: "",
                needs_key: false,
            }),
        )
        .collect()
}
fn display_name(id: &str) -> String {
    match find_provider(ProviderKind::Chat, id) {
        Some(provider) => provider.name.to_owned(),
        None if id == LOCAL => "Local".to_owned(),
        None => id.to_owned(),
    }
}

/// What the reducer needs to know about the settings.
pub struct Ctx {
    pub provider: String,
    pub name: String,
    pub model: String,
    /// Who a transcript read may reach: the provider and its consented fallbacks.
    pub receivers: String,
    pub offline: bool,
    pub history: bool,
    pub consented: bool,
}
impl Ctx {
    pub fn new(config: &Config) -> Self {
        let ai = &config.ai;
        let offline = config.privacy.offline;
        let mut receivers = display_name(&ai.provider);
        for id in &ai.fallback {
            if *id != ai.provider && ai.consented.contains(id) && !offline {
                receivers.push_str(&format!(" or {}", display_name(id)));
            }
        }
        let default = find_provider(ProviderKind::Chat, &ai.provider).map(|p| p.default_model);
        Self {
            provider: ai.provider.clone(),
            name: display_name(&ai.provider),
            model: ai.model.as_deref().or(default).unwrap_or("").to_owned(),
            receivers,
            offline,
            history: config.privacy.history,
            consented: ai.consented.contains(&ai.provider),
        }
    }
}

/// One turn for the runtime to start.
#[derive(Debug)]
pub struct Turn {
    pub id: u64,
    pub question: String,
    pub history: Vec<Message>,
    pub dictations: bool,
    pub meetings: bool,
    pub meeting: Option<xflow_core::meeting::MeetingId>,
}
pub struct ChatRun {
    pub turn: Turn,
    pub model: String,
    pub ai: AiConfig,
    pub offline: bool,
    pub path: PathBuf,
}
/// What a reducer step asks of the app.
#[derive(Debug)]
pub enum Out {
    None,
    Send(Turn),
    Cancel,
    Consent,
    Leave,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    You(String),
    Answer(String),
    Note(String),
    Error(String),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    You,
    Answer,
    Note,
    Error,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub kind: Kind,
    pub text: String,
}
/// Wrapped rows. Entries before the live answer are never wrapped twice at one width.
#[derive(Default)]
struct Layout {
    width: usize,
    height: usize,
    rows: Vec<Row>,
    frozen_entries: usize,
    frozen_rows: usize,
    /// Top row while the user has scrolled up; `None` follows the bottom.
    anchor: Option<usize>,
}
#[derive(Default)]
struct InputLayout {
    revision: u64,
    width: usize,
    rows: Vec<String>,
    cursor: (usize, usize),
}
/// The visible transcript rows.
pub struct Window {
    pub rows: Vec<Row>,
    pub below: bool,
}
/// The visible input rows, and the cursor among them as (row, column).
pub struct InputView {
    pub rows: Vec<String>,
    pub cursor: (usize, usize),
}

#[derive(Default)]
pub struct Chat {
    pub entries: Vec<Entry>,
    pub input: String,
    /// 0=dictations, 1=meetings, 2=both, 3=none. In memory only.
    source: u8,
    cursor: usize,
    revision: u64,
    /// What the model remembers; `run_turn` replaces it when a turn succeeds.
    history: Vec<Message>,
    /// The provider and history setting `history` was built under.
    scope: Option<(String, bool)>,
    turn: u64,
    running: Option<u64>,
    question: String,
    answered: bool,
    retry: Option<String>,
    /// A question waiting for the consent answer, and whether it came from the input.
    pending: Option<(String, bool)>,
    dirty: bool,
    pub keys: Vec<(String, KeySource)>,
    layout: RefCell<Layout>,
    input_layout: RefCell<InputLayout>,
}
impl Chat {
    pub fn running(&self) -> bool {
        self.running.is_some()
    }
    pub fn can_retry(&self) -> bool {
        self.retry.is_some() && !self.running()
    }
    /// True once per batch of changes that streaming left undrawn.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
    pub fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Out {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter if !self.running() => {
                let question = self.input.trim().to_owned();
                return self.gate(question, true, ctx);
            }
            KeyCode::Esc if self.running() => return self.cancel(),
            KeyCode::Esc => return Out::Leave,
            KeyCode::Char('r') if ctrl => {
                if let Some(question) = self.retry.clone().filter(|_| !self.running()) {
                    return self.gate(question, false, ctx);
                }
            }
            KeyCode::Char('s') if ctrl && !self.running() => {
                self.source = (self.source + 1) % 4;
                self.history.clear();
                self.retry = None;
                self.push(Entry::Note(format!("Sources: {}. Previous model context cleared. Meetings require consent for each turn.", self.source_name())));
                return Out::None;
            }
            KeyCode::Char('n') if ctrl => return self.new_chat(),
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Up => self.scroll(-1),
            KeyCode::Down => self.scroll(1),
            KeyCode::PageUp => self.scroll(-self.page()),
            KeyCode::PageDown => self.scroll(self.page()),
            KeyCode::Left => self.cursor -= self.before().map_or(0, str::len),
            KeyCode::Right => self.cursor += self.after().map_or(0, str::len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Backspace => {
                let start = self.cursor - self.before().map_or(0, str::len);
                self.input.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            KeyCode::Delete => {
                let end = self.cursor + self.after().map_or(0, str::len);
                self.input.replace_range(self.cursor..end, "");
            }
            KeyCode::Char(c) if !ctrl && !c.is_control() => {
                if self.input.len() + c.len_utf8() > INPUT_BYTES {
                    self.push(Entry::Note(
                        "The chat box is full: send or shorten the text first.".into(),
                    ));
                } else {
                    self.input.insert(self.cursor, c);
                    self.cursor += c.len_utf8();
                }
            }
            _ => return Out::None,
        }
        self.revision += 1;
        Out::None
    }
    fn before(&self) -> Option<&str> {
        self.input[..self.cursor].graphemes(true).next_back()
    }
    fn after(&self) -> Option<&str> {
        self.input[self.cursor..].graphemes(true).next()
    }
    pub fn paste(&mut self, text: &str) {
        let text = clean(text);
        if self.input.len() + text.len() > INPUT_BYTES {
            self.push(Entry::Note(
                "That paste is too long for the chat box, so nothing was added.".into(),
            ));
            return;
        }
        self.input.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.revision += 1;
    }
    fn gate(&mut self, question: String, from_input: bool, ctx: &Ctx) -> Out {
        if question.is_empty() {
            return Out::None;
        }
        if question.len() > MAX_QUESTION_BYTES {
            self.push(Entry::Error(
                "The question is over 64 KiB: shorten it, then press Enter.".into(),
            ));
            return Out::None;
        }
        if ctx.offline && ctx.provider != LOCAL {
            self.push(Entry::Error(offline_refusal(&ctx.name)));
            return Out::None;
        }
        if (ctx.history && self.source != 3 && !ctx.consented) || matches!(self.source, 1 | 2) {
            self.pending = Some((question, from_input));
            return Out::Consent;
        }
        self.start(question, from_input, ctx)
    }
    /// The answer to the consent modal. `ctx` no longer decides: the answer holds for
    /// this question even if saving it fails.
    pub fn consent(&mut self, yes: bool, ctx: &Ctx) -> Out {
        let Some((question, from_input)) = self.pending.take() else {
            return Out::None;
        };
        if yes {
            return self.start(question, from_input, ctx);
        }
        self.push(Entry::Note(
            "Nothing was sent: press Enter again and answer y to allow it.".into(),
        ));
        Out::None
    }
    fn start(&mut self, question: String, from_input: bool, ctx: &Ctx) -> Out {
        // Earlier turns may hold transcript excerpts: they never follow a changed
        // provider, or carry on once history is off.
        let scope = (ctx.provider.clone(), ctx.history);
        if self.scope.as_ref().is_some_and(|s| *s != scope) && !self.history.is_empty() {
            self.history.clear();
            self.push(Entry::Note(
                "The service or history setting changed, so the model starts fresh here.".into(),
            ));
        }
        self.scope = Some(scope);
        if from_input {
            self.input.clear();
            self.cursor = 0;
            self.revision += 1;
            self.push(Entry::You(question.clone()));
        } else {
            self.push(Entry::Note("Retrying.".into()));
        }
        self.retry = None;
        self.turn += 1;
        self.running = Some(self.turn);
        self.question = question.clone();
        self.answered = false;
        self.layout.get_mut().anchor = None;
        Out::Send(Turn {
            id: self.turn,
            question,
            history: self.history.clone(),
            dictations: ctx.history && matches!(self.source, 0 | 2),
            meetings: matches!(self.source, 1 | 2),
            meeting: None,
        })
    }
    fn cancel(&mut self) -> Out {
        self.running = None;
        self.retry = Some(std::mem::take(&mut self.question));
        self.push(Entry::Error(if self.answered {
            format!("Interrupted: the text above is incomplete. {RETRY}")
        } else {
            format!("Interrupted before any answer. {RETRY}")
        }));
        Out::Cancel
    }
    pub(super) fn meeting_chat(&mut self) {
        self.new_chat();
        self.input.clear();
        self.cursor = 0;
        self.source = 1;
    }
    /// The typed input survives; everything else is gone.
    fn new_chat(&mut self) -> Out {
        let was_running = self.running();
        let (input, cursor, revision, turn, keys) = (
            std::mem::take(&mut self.input),
            self.cursor,
            self.revision,
            self.turn,
            std::mem::take(&mut self.keys),
        );
        *self = Self {
            input,
            cursor,
            revision: revision + 1,
            turn,
            keys,
            ..Self::default()
        };
        if was_running {
            Out::Cancel
        } else {
            Out::None
        }
    }
    /// A late event from a cancelled or replaced turn is ignored.
    pub fn event(&mut self, id: u64, event: TurnEvent, ctx: &Ctx) {
        if self.running != Some(id) {
            return;
        }
        match event {
            TurnEvent::Model(Event::Text(text)) => {
                let text = clean(&text);
                self.answered = true;
                if let Some(Entry::Answer(answer)) = self.entries.last_mut() {
                    answer.push_str(&text);
                } else {
                    self.entries.push(Entry::Answer(text));
                }
            }
            TurnEvent::Model(_) => return,
            TurnEvent::Read {
                tool,
                arguments,
                bytes,
                ..
            } => self.push(Entry::Note(read_line(
                &tool,
                &arguments,
                bytes,
                &ctx.receivers,
            ))),
            TurnEvent::Notice(notice) => self.push(Entry::Note(clean(&notice))),
        }
        self.dirty = true;
    }
    pub fn finish(
        &mut self,
        id: u64,
        result: Result<Stop, LlmError>,
        history: Vec<Message>,
        ctx: &Ctx,
    ) {
        if self.running != Some(id) {
            return;
        }
        self.running = None;
        let question = std::mem::take(&mut self.question);
        match result {
            Ok(stop) => {
                self.history = history;
                if !self.answered {
                    self.push(Entry::Note(
                        "No answer came back: try asking another way.".into(),
                    ));
                } else if stop == Stop::Length {
                    self.push(Entry::Note(
                        "The answer reached the output limit and may be incomplete: ask it to continue.".into(),
                    ));
                }
            }
            Err(LlmError::Offline) => self.push(Entry::Error(offline_refusal(&ctx.name))),
            Err(error) => {
                self.retry = Some(question);
                self.push(Entry::Error(explain(ctx, &error)));
            }
        }
    }
    fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
        self.dirty = true;
    }
    fn page(&self) -> isize {
        self.layout.borrow().height.saturating_sub(1).max(1) as isize
    }
    fn scroll(&mut self, delta: isize) {
        let layout = self.layout.get_mut();
        let bottom = layout.rows.len().saturating_sub(layout.height);
        let top = layout.anchor.unwrap_or(bottom) as isize + delta;
        layout.anchor = (top < bottom as isize).then_some(top.max(0) as usize);
    }
    /// The rows to show. Only the live answer is wrapped again; a new width wraps all.
    pub fn window(&self, width: usize, height: usize) -> Window {
        let mut layout = self.layout.borrow_mut();
        let layout = &mut *layout;
        let resized = layout.width != width;
        let old_rows = layout.rows.len();
        if resized {
            *layout = Layout {
                width,
                anchor: layout.anchor,
                ..Layout::default()
            };
        }
        layout.height = height;
        layout.rows.truncate(layout.frozen_rows);
        let live = self.running() && matches!(self.entries.last(), Some(Entry::Answer(_)));
        let stable = self.entries.len() - usize::from(live);
        for entry in &self.entries[layout.frozen_entries..stable] {
            lay_out(entry, width, &mut layout.rows);
        }
        layout.frozen_entries = stable;
        layout.frozen_rows = layout.rows.len();
        if live {
            lay_out(&self.entries[stable], width, &mut layout.rows);
        }
        if self.entries.is_empty() {
            let hint = "Ask a question. This chat stays in memory and is gone when you quit.";
            lay_out(&Entry::Note(hint.into()), width, &mut layout.rows);
        }
        let total = layout.rows.len();
        if resized && old_rows > 0 {
            layout.anchor = layout.anchor.map(|top| top * total / old_rows);
        }
        let bottom = total.saturating_sub(height);
        let top = layout.anchor.map_or(bottom, |top| top.min(bottom));
        Window {
            rows: layout.rows[top..(top + height).min(total)].to_vec(),
            below: top < bottom,
        }
    }
    /// At most `max_rows`, keeping the cursor in view. Cached: a key costs one wrap and
    /// a streaming frame none, however long the pasted text is.
    pub fn input_view(&self, width: usize, max_rows: usize) -> InputView {
        let mut layout = self.input_layout.borrow_mut();
        if layout.revision != self.revision || layout.width != width || layout.rows.is_empty() {
            let (rows, cursor) = wrap_input(&self.input, self.cursor, width);
            *layout = InputLayout {
                revision: self.revision,
                width,
                rows,
                cursor,
            };
        }
        let (row, column) = layout.cursor;
        let max_rows = max_rows.max(1);
        let first = (row + 1).saturating_sub(max_rows);
        let end = (first + max_rows).min(layout.rows.len());
        InputView {
            rows: layout.rows[first..end].to_vec(),
            cursor: (row - first, column),
        }
    }
    fn source_name(&self) -> &'static str {
        match self.source {
            0 => "dictations",
            1 => "meetings",
            2 => "dictations + meetings",
            _ => "none",
        }
    }
    pub fn title(&self, ctx: &Ctx) -> String {
        let mut title = format!("Chat · {} · {} (Ctrl+S)", ctx.name, self.source_name());
        if !ctx.model.is_empty() {
            title.push_str(&format!(" · {}", ctx.model));
        }
        if self.running() {
            title.push_str(" · answering…");
        }
        title
    }
    pub fn banner(ctx: &Ctx) -> Option<&'static str> {
        (!ctx.history).then_some("History is off, so answers use none of your dictations.")
    }
    pub fn footer(&self) -> Vec<&'static str> {
        if self.running() {
            return vec![
                "Esc cancel",
                "↑↓ scroll",
                "Ctrl+N new",
                "Tab page",
                "Ctrl+C quit",
            ];
        }
        let mut keys = vec!["Enter send"];
        if self.can_retry() {
            keys.push("Ctrl+R retry");
        }
        // All of it fits 80 columns, so "Ctrl+C quit" always shows.
        keys.extend(["Ctrl+N new", "Esc back", "Tab page", "Ctrl+C quit"]);
        keys
    }
    #[cfg(test)]
    pub fn frozen_entries(&self) -> usize {
        self.layout.borrow().frozen_entries
    }
    fn key_state(&self, id: &str) -> Option<&KeySource> {
        self.keys.iter().find(|(k, _)| k == id).map(|(_, s)| s)
    }
}

fn offline_refusal(name: &str) -> String {
    format!("Local services only is on, so {name} is refused: set up a local server, or turn it off in Settings.")
}
fn explain(ctx: &Ctx, error: &LlmError) -> String {
    let (name, id) = (&ctx.name, &ctx.provider);
    let key_url = find_provider(ProviderKind::Chat, id)
        .map(|p| format!(" (get one at {})", p.key_url))
        .unwrap_or_default();
    let line = match error {
        LlmError::Auth => {
            format!("{name} rejected the API key: run xflow key set {id}{key_url}.")
        }
        LlmError::NotLoggedIn if crate::harness::is_harness(id) => { format!("Log in using the installed {name} CLI, then retry.") }
        LlmError::NotLoggedIn => {
            format!("No API key for {name}: run xflow key set {id}{key_url}.")
        }
        LlmError::QuotaExhausted => format!(
            "Your {name} account is out of credit or quota: add some, or choose another service in Settings."
        ),
        LlmError::ContextTooLong => "The question and context are too long for this model: press Ctrl+N for a new chat, or lower the context limit in Settings.".to_owned(),
        LlmError::Interrupted { partial: true } => {
            "Interrupted: the answer was cut off, so the text above is incomplete.".to_owned()
        }
        LlmError::Interrupted { partial: false } => {
            "Interrupted: the connection closed before any answer.".to_owned()
        }
        other => clean(&format!("{name}: {other}.")),
    };
    format!("{line} {RETRY}")
}
/// `bytes` is what leaves the machine; a search that found nothing sends nothing.
fn read_line(tool: &str, arguments: &Value, bytes: usize, receivers: &str) -> String {
    let what = match (tool, &arguments["query"], &arguments["id"]) {
        ("search_dictations", Value::String(query), _) => {
            format!("Searched dictations for '{}'", clean(query))
        }
        ("search_dictations", _, _) => "Searched dictations".to_owned(),
        ("get_dictation", _, id) => format!("Read dictation {id}"),
        _ => format!("Read {}", clean(tool)),
    };
    if bytes == 0 {
        format!("{what}: nothing found, so nothing was sent")
    } else {
        format!("{what}: sent to {receivers}")
    }
}
/// No terminal controls from pasted or model text; line ends become `\n`.
fn clean(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' | '\r' => out.push('\n'),
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}
fn lay_out(entry: &Entry, width: usize, rows: &mut Vec<Row>) {
    let (kind, text, first, rest, gap) = match entry {
        Entry::You(text) => (Kind::You, text, glyph("› ", "> "), "  ", true),
        Entry::Answer(text) => (Kind::Answer, text, "", "", true),
        Entry::Note(text) => (Kind::Note, text, glyph("· ", "- "), "  ", false),
        Entry::Error(text) => (Kind::Error, text, "! ", "  ", false),
    };
    if gap && !rows.is_empty() {
        rows.push(Row {
            kind,
            text: String::new(),
        });
    }
    let inner = width.saturating_sub(first.width());
    for (index, line) in wrap(text, inner).into_iter().enumerate() {
        let prefix = if index == 0 { first } else { rest };
        rows.push(Row {
            kind,
            text: format!("{prefix}{line}"),
        });
    }
}
/// Word wrap by display width. A word wider than the row breaks between graphemes.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = vec![];
    for line in text.split('\n') {
        let mut row = String::new();
        let mut column = 0;
        // Just after the last space: its byte offset and column.
        let mut space = None;
        for grapheme in line.graphemes(true) {
            let cells = grapheme.width();
            while column + cells > width && column > 0 {
                match space.take() {
                    Some((at, used)) => {
                        let rest = row.split_off(at);
                        rows.push(std::mem::replace(&mut row, rest));
                        column -= used;
                    }
                    None => {
                        rows.push(std::mem::take(&mut row));
                        column = 0;
                    }
                }
            }
            row.push_str(grapheme);
            column += cells;
            if grapheme == " " {
                space = Some((row.len(), column));
            }
        }
        rows.push(row);
    }
    rows
}
/// Hard wrap that keeps every character, so the cursor maps to one cell.
fn wrap_input(text: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let width = width.max(1);
    let (mut rows, mut row, mut column) = (vec![], String::new(), 0);
    let (mut offset, mut at) = (0, None);
    for grapheme in text.graphemes(true) {
        let cells = grapheme.width();
        if grapheme != "\n" && column + cells > width && column > 0 {
            rows.push(std::mem::take(&mut row));
            column = 0;
        }
        if at.is_none() && offset >= cursor {
            at = Some((rows.len(), column));
        }
        offset += grapheme.len();
        if grapheme == "\n" {
            rows.push(std::mem::take(&mut row));
            column = 0;
        } else {
            row.push_str(grapheme);
            column += cells;
        }
    }
    if at.is_none() && column >= width {
        rows.push(std::mem::take(&mut row));
        column = 0;
    }
    let at = at.unwrap_or((rows.len(), column));
    rows.push(row);
    (rows, at)
}
fn key_label(state: Option<&KeySource>) -> String {
    match state {
        None => "Checking the key…",
        Some(KeySource::NotRequired) => "No key needed",
        Some(KeySource::Environment(_)) => "Key from your environment",
        Some(KeySource::Keystore) => "Key saved",
        Some(KeySource::Keychain) => "Key waiting to be moved",
        Some(KeySource::Missing) => "No key yet",
        Some(KeySource::Unreadable(_)) => "Saved key can't be read",
    }
    .to_owned()
}

/// Keys the Chat page leaves to the app: moving between pages, the palette, and quit.
pub fn captures(key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    !matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
        && !(ctrl && matches!(key.code, KeyCode::Char('c' | 'p')))
}

/// Glue between the reducer and the app: configuration edits stay in memory until the
/// runtime's usual save, and everything else becomes an `Effect`.
impl App {
    pub fn chat_ready(&self) -> bool {
        !self.config.ai.provider.is_empty()
    }
    pub fn chat_ctx(&self) -> Ctx {
        Ctx::new(&self.config)
    }
    pub fn chat_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctx = self.chat_ctx();
        let out = self.chat.key(key, &ctx);
        self.chat_out(out, &ctx)
    }
    fn chat_out(&mut self, out: Out, ctx: &Ctx) -> Vec<Effect> {
        match out {
            Out::None => vec![],
            Out::Cancel => vec![Effect::ChatCancel],
            Out::Leave => self.action(super::state::Action::Back),
            Out::Consent => {
                self.modal = Some(Modal::ChatConsent {
                    name: ctx.name.clone(),
                    opened_at: Instant::now(),
                });
                vec![]
            }
            Out::Send(mut turn) => {
                turn.meeting = self.ask_meeting.clone().filter(|_| turn.meetings);
                if turn.meeting.is_some() {
                    turn.dictations = false;
                }
                vec![Effect::ChatTurn(Box::new(ChatRun {
                    turn,
                    model: ctx.model.clone(),
                    ai: self.config.ai.clone(),
                    offline: self.config.privacy.offline,
                    path: self.path.clone(),
                }))]
            }
        }
    }
    /// Yes stages `ai.consented`, which the runtime saves like any other choice.
    pub fn chat_consent(&mut self, yes: bool) -> Vec<Effect> {
        if let Some(id) = self.pending_notes.take() {
            return if yes {
                vec![Effect::Meetings(super::meetings::Job::Generate(
                    id,
                    self.path.clone(),
                ))]
            } else {
                vec![]
            };
        }

        if yes && self.chat.source == 0 {
            let mut consented = self.config.ai.consented.clone();
            consented.push(self.config.ai.provider.clone());
            let staged = self
                .file
                .as_mut()
                .map(|file| file.set_typed("ai.consented", &consented));
            if !matches!(staged, Some(Ok(()))) {
                self.toast = Some("That choice wasn't saved, so xflow will ask again.".into());
            }
            self.sync_config();
        }
        let ctx = self.chat_ctx();
        let out = self.chat.consent(yes, &ctx);
        self.chat_out(out, &ctx)
    }
    pub fn chat_rows(&self) -> Vec<(String, String)> {
        choices()
            .into_iter()
            .map(|choice| {
                let state = if choice.needs_key || crate::harness::is_harness(choice.id) {
                    self.chat.key_state(choice.id)
                } else {
                    Some(&KeySource::NotRequired)
                };
                (
                    choice.name.to_owned(),
                    if crate::harness::is_harness(choice.id) {
                        match state {
                            Some(KeySource::Unreadable(s)) => s.clone(),
                            _ => "Checking installed CLI…".into(),
                        }
                    } else {
                        key_label(state)
                    },
                )
            })
            .collect()
    }
    pub fn chat_details(&self, pos: usize) -> String {
        let Some(choice) = choices().into_iter().nth(pos) else {
            return String::new();
        };
        if crate::harness::is_harness(choice.id) {
            return crate::harness::isolation_reason(choice.id).unwrap_or("Opt-in installed CLI. Uses its own login; xflow never reads authentication files. The CLI may persist logs, caches or sessions. Claude is prefetch only.").into();
        }
        let mut text = format!("Choose where your questions go. Press Enter to set up {}.\n\nThe key stays encrypted on this computer. Your dictations are shared only after you allow it.", choice.name);
        if let Some(issue) = self.chat.key_state(choice.id).and_then(key_issue) {
            text.push_str(&format!("\n\n{issue}"));
        }
        text
    }
    pub fn chat_choose(&mut self, pos: usize) {
        let Some(choice) = choices().into_iter().nth(pos) else {
            return;
        };
        let (intro, first) = if crate::harness::is_harness(choice.id) {
            ("Uses your installed CLI and its own login. No key is requested. Transcript excerpts leave this computer after consent. The CLI may retain logs, caches or sessions. Codex and Cursor are unavailable until their isolation is verified. Claude uses tools-off safe mode with bounded prefetch; no MCP tools.".into(), Field::new("Authentication", "Managed by installed CLI"))
        } else if choice.needs_key {
            let mut field = Field::new("API key", "");
            field.secret = true;
            let intro = format!(
                "Paste your {} API key. Get one at {}\nxflow keeps it encrypted on this computer, never in your settings file.\nAlready added one? Leave it empty and press Enter.",
                choice.name, choice.key_url
            );
            (intro, field)
        } else {
            let intro =
                "The address of a chat server running on this computer. Nothing is sent elsewhere."
                    .to_owned();
            (intro, Field::new("Address", LOCAL_ENDPOINT))
        };
        let title = if choice.needs_key || crate::harness::is_harness(choice.id) {
            choice.name
        } else {
            "Local server"
        };
        self.edit(
            EditTarget::ChatProvider(choice.id.into()),
            title,
            intro,
            vec![first, Field::new("Model", choice.model)],
        );
    }
    pub fn apply_chat_setup(&mut self, id: &str, editor: &mut Editor) -> Result<Vec<Effect>> {
        let local = id == LOCAL;
        let model = editor.fields[1].value.trim().to_owned();
        if model.len() > 256 || model.chars().any(char::is_control) {
            bail!("The model name must be under 256 characters, on one line");
        }
        let ai = AiConfig {
            provider: id.to_owned(),
            model: (!model.is_empty()).then_some(model),
            endpoint: local.then(|| editor.fields[0].value.trim().to_owned()),
            ..self.config.ai.clone()
        };
        if local && build_provider(&ai, false).is_err() {
            bail!("Enter a model, and an address on this computer such as {LOCAL_ENDPOINT}");
        }
        let typed =
            !local && !crate::harness::is_harness(id) && !editor.fields[0].value.trim().is_empty();
        if !local
            && !crate::harness::is_harness(id)
            && !typed
            && self.chat.key_state(id) == Some(&KeySource::Missing)
        {
            bail!("Paste your API key first");
        }
        if self.saving {
            bail!("One moment — saving your last change");
        }
        let Some(file) = self.file.as_mut() else {
            bail!("Settings are still loading");
        };
        let mut next = file.clone();
        next.set_typed("ai.provider", &ai.provider)?;
        if local {
            // Local setup promises no cloud transfer, including on connection failure.
            next.set_typed("ai.fallback", &Vec::<String>::new())?;
        }
        for (key, value) in [("ai.model", &ai.model), ("ai.endpoint", &ai.endpoint)] {
            match value {
                Some(value) => next.set_typed(key, value)?,
                None => drop(next.unset(key)?),
            }
        }
        *file = next;
        self.sync_config();
        Ok(if typed {
            // Moved, never cloned, into the zeroizing payload.
            let raw = Zeroizing::new(std::mem::take(&mut *editor.fields[0].value));
            let secret = Zeroizing::new(raw.trim().to_owned());
            vec![Effect::SaveKey(id.to_owned(), secret)]
        } else {
            vec![]
        })
    }
    /// The consent modal's text, the same words as `xflow ask`.
    pub fn chat_consent_text(name: &str) -> String {
        format!(
            "{}\nIncludes the selected dictation and/or meeting sources. Installed CLIs may retain logs, caches or sessions.\n\nAllow it?   y = yes   ·   n or Esc = no",
            consent_text(name)
        )
    }
}
