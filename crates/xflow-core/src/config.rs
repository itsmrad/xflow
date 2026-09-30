use crate::CleanupMode;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub stt: SttConfig,
    pub cleanup: CleanupConfig,
    pub privacy: PrivacyConfig,
    pub recording: RecordingConfig,
    pub injection: InjectionConfig,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SttConfig {
    pub provider: String,
    /// Full endpoint URL, not a base URL.
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    pub language: Option<String>,
    pub vocabulary: Vec<String>,
    pub timeout_secs: u64,
}
impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: "groq".into(),
            endpoint: None,
            model: None,
            api_key_env: None,
            language: None,
            vocabulary: vec![],
            timeout_secs: 30,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CleanupConfig {
    pub mode: CleanupMode,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub api_key_env: String,
}
impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            mode: CleanupMode::Raw,
            endpoint: None,
            model: None,
            api_key_env: "OPENAI_API_KEY".into(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivacyConfig {
    pub history: bool,
    pub history_limit: usize,
    pub offline: bool,
}
impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            history: true,
            history_limit: 500,
            offline: false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordingConfig {
    pub max_seconds: u32,
    pub silence_threshold: f32,
}
impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            max_seconds: 120,
            silence_threshold: 0.005,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InjectionConfig {
    pub clipboard_only: bool,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = if path.exists() {
            toml::from_str(&std::fs::read_to_string(path)?).context("invalid configuration")?
        } else {
            Self::default()
        };
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        if !(1..=600).contains(&self.recording.max_seconds) {
            bail!("recording.max_seconds must be 1..600");
        }
        if !self.recording.silence_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.recording.silence_threshold)
        {
            bail!("invalid silence threshold");
        }
        if self.stt.timeout_secs == 0 || self.stt.timeout_secs > 300 {
            bail!("stt.timeout_secs must be 1..300");
        }
        if self.privacy.history_limit > 100_000 {
            bail!("history_limit exceeds 100000");
        }
        Ok(())
    }
}
