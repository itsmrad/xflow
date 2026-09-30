//! Shared contracts. No desktop toolkit or model runtime belongs here.
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod config;
pub mod ipc;

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
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppContext {
    pub app_id: Option<String>,
    pub window_id: Option<String>,
    pub selected_text: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InjectionOutcome {
    Pasted,
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
}

#[async_trait]
pub trait SpeechToText: Send + Sync {
    fn name(&self) -> &str;
    fn supports_streaming(&self) -> bool {
        false
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

#[async_trait]
pub trait TextTransformer: Send + Sync {
    async fn transform(
        &self,
        text: &str,
        mode: CleanupMode,
        context: &AppContext,
    ) -> Result<String>;
}

#[async_trait]
pub trait AudioCapture: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<AudioClip>;
    async fn cancel(&self) -> Result<()>;
    fn level(&self) -> f32;
}

#[async_trait]
pub trait Desktop: Send + Sync {
    async fn context(&self) -> Result<AppContext>;
    async fn inject(&self, text: &str, target: &AppContext) -> Result<InjectionOutcome>;
    async fn copy(&self, text: &str) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    Start,
    Stop,
    Toggle,
    Cancel,
    PasteLast,
}

#[async_trait]
pub trait GlobalHotkeys: Send {
    async fn next_action(&mut self) -> Result<HotkeyAction>;
}

#[async_trait]
pub trait Overlay: Send + Sync {
    async fn update(&self, event: &DesktopEvent) -> Result<()>;
}
