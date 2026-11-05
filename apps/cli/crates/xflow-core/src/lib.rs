//! Shared contracts. No desktop toolkit or model runtime belongs here.
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc,
};

pub mod backtrack;
pub mod code;
pub mod config;
mod cues;
pub mod hints;
pub mod ipc;
pub mod learn;
pub mod lists;
pub mod meeting;
pub mod snap;
pub mod text;
pub mod title;
pub mod words;

/// xflowd's log: stderr (the service's log file), filtered by `XFLOW_LOG`
/// (error, warn, info, debug; default warn). Shared so platform code logs too.
pub fn log(level: &str, message: &str) {
    if log_enabled(level, std::env::var("XFLOW_LOG").ok().as_deref()) {
        eprintln!("xflowd [{level}]: {message}");
    }
}

fn log_enabled(level: &str, configured: Option<&str>) -> bool {
    let rank = |s| match s {
        "error" => 0,
        "warn" => 1,
        "info" => 2,
        "debug" => 3,
        _ => 1,
    };
    rank(level) <= rank(configured.unwrap_or("warn"))
}

/// FNV-1a 64: a stable, explicitly specified hash (unlike std's
/// `DefaultHasher`, whose output may change between Rust releases), so two
/// binaries can compare local state. Not a secret and not collision-proof.
pub fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Maximum uploadable mono 16-bit PCM WAV: 25 MB including the 44-byte header.
pub const MAX_UPLOAD_WAV_BYTES: usize = 25_000_000;
pub const MAX_UPLOAD_FRAMES: usize = (MAX_UPLOAD_WAV_BYTES - 44) / 2;

#[derive(Clone, Debug)]
pub struct AudioClip {
    /// Interleaved normalized PCM. Providers must encode/downmix explicitly.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionOptions {
    pub language: Option<String>,
    pub vocabulary: Vec<String>,
    /// Shipped domain terms (`hints::select`), sent after `vocabulary` within
    /// the provider's budget. After the provider's merge: the ones it sends,
    /// which end `vocabulary`.
    #[serde(skip)]
    pub domain: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transcript {
    #[serde(default)]
    pub provider: Option<String>,
    pub text: String,
    pub language: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CleanupMode {
    #[default]
    Raw,
    Light,
    Polished,
    /// Only the user's `cleanup.prompt` instructions.
    Custom,
}

/// What a recording session is for.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Insert what was said.
    #[default]
    Dictation,
    /// Apply the spoken instruction to the selected text, or write new text
    /// from the instruction when nothing is selected.
    Command,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContext {
    pub app_id: Option<String>,
    pub window_id: Option<String>,
    pub selected_text: Option<String>,
    /// Focused terminal input inside an editor; not proof of bracketed-paste support.
    #[serde(default)]
    pub terminal_input: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InjectionOutcome {
    Pasted,
    /// Paste keys sent to an editable target, but no insertion receipt was available.
    Uncertain,
    /// Delivered as synthesized key presses (`injection.method = "type"`).
    Typed,
    ClipboardOnly,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Idle,
    Listening,
    Processing,
    Success,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopEvent {
    pub state: State,
    pub level: f32,
    pub message: Option<String>,
    #[serde(default)]
    pub mode: Mode,
}

#[async_trait]
pub trait SpeechToText: Send + Sync {
    fn name(&self) -> &str;
    /// Effective model id, for status and history display.
    fn model(&self) -> &str {
        ""
    }
    /// A degraded route that should be visible in daemon status.
    fn warning(&self) -> Option<&str> {
        None
    }
    fn supports_streaming(&self) -> bool {
        false
    }
    /// Called when recording starts so stop -> transcript skips DNS/TCP/TLS
    /// setup. Must not upload audio or incur billable usage; errors are ignored.
    async fn warm(&self) -> Result<()> {
        Ok(())
    }
    async fn transcribe(
        &self,
        audio: AudioClip,
        options: TranscriptionOptions,
    ) -> Result<Transcript>;
}

/// Streaming providers finalize before injection; partial text is preview-only.
#[async_trait]
pub trait StreamingSpeechSession: Send {
    async fn send_audio(&mut self, samples: &[f32]) -> Result<()>;
    async fn next_partial(&mut self) -> Result<Option<Transcript>>;
    async fn finish(self: Box<Self>) -> Result<Transcript>;
    async fn cancel(self: Box<Self>) -> Result<()>;
}

/// Input for optional LLM post-processing. Built by the daemon; providers own
/// the prompt wording. `Raw` never reaches a transformer.
#[derive(Clone, Copy, Debug)]
pub struct TransformRequest<'a> {
    /// The transcript, or the selected text in command mode.
    pub text: &'a str,
    pub mode: CleanupMode,
    /// `cleanup.prompt` or the matching `styles[].prompt`.
    pub instructions: Option<&'a str>,
    /// Command mode: the user's spoken instruction to apply to `text`.
    pub command: Option<&'a str>,
    /// Focused app id, only when `cleanup.app_context` is enabled.
    pub app_id: Option<&'a str>,
    /// Dictionary words whose spelling must be preserved.
    pub vocabulary: &'a [String],
}

#[async_trait]
pub trait TextTransformer: Send + Sync {
    /// Like `SpeechToText::warm`: called when recording starts so cleanup skips
    /// connection setup. Must not incur billable usage; errors are ignored.
    async fn warm(&self) -> Result<()> {
        Ok(())
    }
    async fn transform(&self, request: TransformRequest<'_>) -> Result<String>;
}

#[async_trait]
pub trait AudioCapture: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<AudioClip>;
    async fn cancel(&self) -> Result<()>;
    fn level(&self) -> f32;
    /// Last successfully opened microphone and any selection notice. No device I/O.
    fn microphone(&self) -> Option<String> {
        None
    }
    /// Capture reached its device/buffer limit or failed. The daemon calls
    /// stop() to retrieve the buffered clip or original error. No idle polling.
    fn finished(&self) -> bool {
        false
    }
    /// Audio captured since the last drain, while recording continues; `stop()`
    /// returns only what is left. Lets the daemon transcribe long dictations in
    /// segments with bounded memory. `None`: not supported or nothing captured.
    async fn drain(&self) -> Result<Option<AudioClip>> {
        Ok(None)
    }
}

#[async_trait]
pub trait Desktop: Send + Sync {
    async fn context(&self) -> Result<AppContext>;
    async fn inject(&self, text: &str, target: &AppContext) -> Result<InjectionOutcome>;
    /// Optional observation must be prepared by injection itself, before keys.
    /// Unsupported adapters deliver normally and explicitly report a safe miss.
    async fn inject_observed(&self, text: &str, target: &AppContext) -> Result<ObservedDelivery> {
        Ok(ObservedDelivery {
            outcome: self.inject(text, target).await?,
            ticket: None,
        })
    }
    async fn copy(&self, text: &str) -> Result<()>;
    /// The current text selection (PRIMARY) for command mode; `None` when
    /// nothing is selected or the desktop cannot provide it.
    async fn selection(&self) -> Result<Option<String>> {
        Ok(None)
    }
    /// `context`, plus `selected_text` when the focused field is editable and not
    /// secure, a search or address field, or a terminal: a hold-to-talk rewrite.
    async fn context_with_selection(&self) -> Result<AppContext> {
        self.context().await
    }
    /// Pin the focused editable field, if it is still in `target`, and watch it
    /// while the user may correct a dictation. None when unsupported, secure, a
    /// terminal, or focus moved. Dropping the watch releases everything.
    async fn watch_field(&self, _target: &AppContext) -> Option<Box<dyn FieldWatch>> {
        None
    }
    /// The focused field's text just before and after the caret (a few
    /// characters each), for `text::adjust`. None when unsupported, secure, a
    /// terminal or unreadable. Never logged or uploaded.
    async fn neighbors(&self) -> Option<(String, String)> {
        None
    }
    /// The title of `target`'s window while it still has focus, for local term
    /// hints. None when unsupported, secure or focus moved. Never logged or uploaded.
    async fn window_title(&self, _target: &AppContext) -> Option<String> {
        None
    }
}

/// A delivery-owned field and span. Never obtained by searching for text after delivery.
pub struct DeliveryTicket {
    pub field: Box<dyn FieldWatch>,
    pub receipt: learn::Receipt,
}
pub struct ObservedDelivery {
    pub outcome: InjectionOutcome,
    /// None is an unsupported/late-readiness safe miss, never permission to reacquire.
    pub ticket: Option<DeliveryTicket>,
}
/// Selection and generation belong to the same guarded bounded read as text.
pub struct FieldSnapshot {
    pub text: FieldText,
    pub selection: Option<(usize, usize)>,
    pub generation: u64,
}
/// The native calls one bounded field read is made of. Each runs only after a
/// fresh `guard`, so a field that turns secure or loses focus stops the read
/// before any more of its text crosses processes.
pub trait FieldSource: Send + Sync + 'static {
    /// Labels and capabilities that rarely change: checked once per read.
    fn policy(&self) -> bool;
    /// Not secure, not protected, still focused. Unknown or failed is false.
    fn guard(&self) -> bool;
    /// Selection start and length, UTF-16 units.
    fn selection(&self) -> Option<(usize, usize)>;
    /// The field's length, UTF-16 units.
    fn count(&self) -> Option<usize>;
    /// Exactly `len` units from `start`.
    fn range(&self, start: usize, len: usize) -> Option<String>;
}
/// One watch's stop and invalidation state, set from any thread. Latched: a focus
/// journey ends the watch even if focus returns before the next read.
#[derive(Default)]
pub struct ReadGate {
    pub stop: AtomicBool,
    pub invalid: AtomicBool,
    pub overflow: AtomicBool,
    /// The observer could not cover the field; nothing it reports can be trusted.
    pub unsupported: AtomicBool,
    /// Bumped by every value notification.
    pub generation: AtomicU64,
    /// Native reads admitted and finished; `stop` waits for them to match.
    pub started: AtomicU32,
    pub finished: AtomicU32,
}
impl ReadGate {
    pub fn open(&self) -> bool {
        !self.stop.load(Ordering::Acquire)
            && !self.invalid.load(Ordering::Acquire)
            && !self.overflow.load(Ordering::Acquire)
            && !self.unsupported.load(Ordering::Acquire)
    }
}
/// One guarded read: at most MAX_FIELD_UTF16 units, never a whole-value copy.
/// Returns nothing if the gate closes, the guard fails or the field or its
/// generation moves at any step. Blocking.
pub fn read_field<S: FieldSource>(source: &S, gate: &ReadGate) -> Option<FieldSnapshot> {
    let generation = gate.generation.load(Ordering::Acquire);
    let admit = || gate.open() && source.guard();
    if !gate.open() || !source.policy() || !admit() {
        return None;
    }
    let selection = source.selection()?;
    if !admit() {
        return None;
    }
    let count = source.count()?;
    let end = selection.0.checked_add(selection.1)?;
    if count > MAX_FIELD_UTF16 || end > count || !admit() {
        return None;
    }
    let value = source.range(0, count)?;
    // Too late to take back, but a field that moved during the copy is not used.
    if value.encode_utf16().count() != count || !admit() || source.count()? != count {
        return None;
    }
    (gate.open() && generation == gate.generation.load(Ordering::Acquire)).then_some(FieldSnapshot {
        text: FieldText {
            value,
            caret: Some(end),
        },
        selection: Some(selection),
        generation,
    })
}
/// At most one native read at a time, off the async runtime. A caller that timed
/// out finds its read still in flight and joins it instead of starting another.
pub struct BoundedReader<S: FieldSource> {
    source: Arc<S>,
    gate: Arc<ReadGate>,
    inflight: Option<tokio::task::JoinHandle<Option<FieldSnapshot>>>,
}
impl<S: FieldSource> BoundedReader<S> {
    pub fn new(source: Arc<S>, gate: Arc<ReadGate>) -> Self {
        Self {
            source,
            gate,
            inflight: None,
        }
    }
    /// A read whose generation is still current, or nothing.
    pub async fn snapshot(&mut self) -> Option<FieldSnapshot> {
        if !self.gate.open() {
            return None;
        }
        if self.inflight.is_none() {
            let (source, gate) = (self.source.clone(), self.gate.clone());
            self.inflight = Some(tokio::task::spawn_blocking(move || {
                gate.started.fetch_add(1, Ordering::AcqRel);
                let read = read_field(&*source, &gate);
                gate.finished.fetch_add(1, Ordering::AcqRel);
                read
            }));
        }
        let read = self.inflight.as_mut()?.await.ok().flatten();
        self.inflight = None;
        read.filter(|r| {
            self.gate.open() && r.generation == self.gate.generation.load(Ordering::Acquire)
        })
    }
    /// Close the gate, then wait out a native call already running: it cannot be
    /// preempted, and its result is discarded. No read starts after this.
    pub async fn stop(&mut self) {
        self.gate.stop.store(true, Ordering::Release);
        if let Some(read) = self.inflight.take() {
            let _ = read.await;
        }
    }
    pub fn gate(&self) -> &ReadGate {
        &self.gate
    }
}
/// A pinned text field, for learning user corrections.
#[async_trait]
pub trait FieldWatch: Send {
    /// The next change or focus notification; pending forever when the desktop
    /// cannot notify, None once the field is gone.
    async fn next(&mut self) -> Option<FieldEvent>;
    /// A notification already queued, without waiting.
    fn try_next(&mut self) -> Option<FieldEvent>;
    /// Bounded text from the pinned element, only while it still owns focus.
    async fn read(&mut self) -> Option<FieldText>;
    /// Whether the pinned field still has keyboard focus.
    async fn focused(&mut self) -> bool;
    async fn snapshot(&mut self) -> Option<FieldSnapshot> {
        Some(FieldSnapshot {
            text: self.read().await?,
            selection: None,
            generation: 0,
        })
    }
    /// Invalidate immediately, then acknowledge that native reads/observer have drained.
    async fn stop(&mut self) {}
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldEvent {
    /// A real departure, a destroyed element or a stop: terminal, nothing more is read.
    Invalidated,
    /// Notifications were lost: the field's history is unknown.
    Overflow,
    /// The observer could not cover this field.
    Unsupported,
    Changed,
    /// Focus may have left the field; re-check after a short grace.
    Focus,
    /// A value change within a few milliseconds of the last one, which the
    /// desktop folds into it: an app's duplicate notification, or a second edit
    /// in the same app turn (the last letter and Enter). Never schedules a read
    /// or counts as an edit, but the text before it is no longer known to be
    /// the last one a cleared field held.
    Merged,
}
/// Maximum requested native range length, in UTF-16 units. No whole-value fallback.
pub const MAX_FIELD_UTF16: usize = 20_000;
pub struct FieldText {
    pub value: String,
    /// End of the selection in UTF-16 units, when the field reports one.
    pub caret: Option<usize>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    Start,
    Stop,
    Toggle,
    Cancel,
    PasteLast,
    /// A second tap latched the current capture until the next press.
    HandsFree,
    /// The press is a real hold, not a tap or a modifier use: show feedback.
    Reveal,
}

#[async_trait]
pub trait GlobalHotkeys: Send {
    async fn next_action(&mut self) -> Result<HotkeyAction>;
    /// Whether the native event tap has received keyboard input since creation.
    fn received_events(&self) -> Option<bool> {
        None
    }
    /// Forget a gesture after a recording ends through another control.
    fn reset(&mut self) {}
}

#[async_trait]
pub trait Overlay: Send + Sync {
    async fn update(&self, event: &DesktopEvent) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::{log_enabled, stable_hash};

    #[test]
    fn stable_hash_is_fnv_1a_64() {
        // Published FNV-1a 64 test vectors: the same in every build.
        assert_eq!(stable_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(stable_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(stable_hash(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn xflow_log_debug_shows_platform_debug_lines_and_default_is_warn() {
        assert!(log_enabled("debug", Some("debug")));
        assert!(log_enabled("warn", Some("debug")));
        assert!(!log_enabled("debug", None));
        assert!(!log_enabled("info", None));
        assert!(log_enabled("warn", None) && log_enabled("error", None));
        assert!(!log_enabled("debug", Some("info")));
        assert!(!log_enabled("debug", Some("bogus")));
    }
}
