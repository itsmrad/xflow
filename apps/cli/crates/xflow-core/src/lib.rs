//! Shared contracts. No desktop toolkit or model runtime belongs here.
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod code;
pub mod config;
pub mod ipc;
pub mod learn;
pub mod lists;
pub mod snap;
pub mod text;
pub mod words;

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TranscriptionOptions {
    pub language: Option<String>,
    pub vocabulary: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transcript {
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
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InjectionOutcome {
    Pasted,
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
    async fn copy(&self, text: &str) -> Result<()>;
    /// The current text selection (PRIMARY) for command mode; `None` when
    /// nothing is selected or the desktop cannot provide it.
    async fn selection(&self) -> Result<Option<String>> {
        Ok(None)
    }
    /// Pin the focused editable field, if it is still in `target`, and watch it
    /// while the user may correct a dictation. None when unsupported, secure, a
    /// terminal, or focus moved. Dropping the watch releases everything.
    async fn watch_field(&self, _target: &AppContext) -> Option<Box<dyn FieldWatch>> {
        None
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
    /// The field's value and caret now, read from the pinned element even when
    /// focus has moved on.
    async fn read(&mut self) -> Option<FieldText>;
    /// Whether the pinned field still has keyboard focus.
    async fn focused(&mut self) -> bool;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldEvent {
    Changed,
    /// Focus may have left the field; re-check after a short grace.
    Focus,
}
/// Fields longer than this (UTF-16 units) are not read for learning: each read
/// copies the whole value across processes, on every edit.
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
