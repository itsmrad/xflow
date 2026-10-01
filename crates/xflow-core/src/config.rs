use crate::CleanupMode;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

/// Bounds shared by validation and by the CLI/TUI editors.
pub const MAX_PROMPT_BYTES: usize = 4096;
pub const MAX_WORD_BYTES: usize = 128;
pub const MAX_SNIPPET_BYTES: usize = 16 * 1024;
pub const MAX_DICTIONARY_ITEMS: usize = 1000;
pub const MAX_SNIPPETS: usize = 500;
pub const MAX_STYLES: usize = 100;

/// Everything the daemon, CLI and TUI read from `config.toml`. Desktop-shell
/// settings (overlay look, global shortcuts) belong to the GNOME extension's
/// GSettings schema instead; see packaging/gnome-extension.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub stt: SttConfig,
    pub cleanup: CleanupConfig,
    pub recording: RecordingConfig,
    pub injection: InjectionConfig,
    pub formatting: FormattingConfig,
    pub dictionary: DictionaryConfig,
    pub sounds: SoundsConfig,
    pub notifications: NotificationsConfig,
    pub privacy: PrivacyConfig,
    pub ui: UiConfig,
    /// Spoken trigger phrase -> inserted text.
    pub snippets: Vec<Snippet>,
    /// Per-app cleanup styles; the first style whose `apps` matches wins.
    pub styles: Vec<Style>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SttConfig {
    pub provider: String,
    /// Full endpoint URL, not a base URL.
    pub endpoint: Option<String>,
    pub model: Option<String>,
    /// Wire protocol for a custom endpoint; ids are defined by xflow-providers.
    pub protocol: Option<String>,
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
            protocol: None,
            api_key_env: None,
            language: None,
            vocabulary: vec![],
            timeout_secs: 30,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CleanupConfig {
    pub mode: CleanupMode,
    /// Provider preset id (e.g. "groq"); fills endpoint, model and key defaults.
    pub provider: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub api_key_env: String,
    /// Extra instructions for light/polished; the whole instruction for custom.
    pub prompt: Option<String>,
    pub timeout_secs: u64,
    /// Tell the cleanup model which app is focused (app id only, never window text).
    pub app_context: bool,
}
impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            mode: CleanupMode::Raw,
            provider: None,
            endpoint: None,
            model: None,
            api_key_env: "OPENAI_API_KEY".into(),
            prompt: None,
            timeout_secs: 20,
            app_context: true,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RecordingConfig {
    pub max_seconds: u32,
    pub silence_threshold: f32,
    /// Input device name (see `xflow devices`); unset uses the system default.
    pub device: Option<String>,
    /// Shorter recordings are discarded without upload (accidental taps).
    pub min_ms: u32,
    /// Stop automatically after this many seconds of continuous silence; 0 = off.
    pub auto_stop_secs: u32,
    /// Keep the microphone stream open this long after a recording for faster
    /// back-to-back dictation; 0 = close immediately (privacy default).
    pub keep_warm_secs: u32,
}
impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            max_seconds: 120,
            silence_threshold: 0.005,
            device: None,
            min_ms: 300,
            auto_stop_secs: 0,
            keep_warm_secs: 0,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InjectionMethod {
    /// Clipboard + paste shortcut; the most compatible and fastest method.
    #[default]
    Paste,
    /// Synthesized key presses; clipboard untouched, slower for long text.
    Type,
    /// Copy only; paste manually.
    Clipboard,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct InjectionConfig {
    pub method: InjectionMethod,
    /// Deprecated alias for `method = "clipboard"`.
    pub clipboard_only: bool,
    /// Put the previous clipboard text back after pasting.
    pub restore_clipboard: bool,
    pub restore_delay_ms: u32,
    /// Append a space so consecutive dictations do not run together.
    pub trailing_space: bool,
}
impl Default for InjectionConfig {
    fn default() -> Self {
        Self {
            method: InjectionMethod::Paste,
            clipboard_only: false,
            restore_clipboard: true,
            restore_delay_ms: 300,
            trailing_space: true,
        }
    }
}
impl InjectionConfig {
    pub fn effective_method(&self) -> InjectionMethod {
        if self.clipboard_only {
            InjectionMethod::Clipboard
        } else {
            self.method
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct FormattingConfig {
    /// Drop standalone filler words locally, before any cleanup model.
    pub remove_fillers: bool,
    pub fillers: Vec<String>,
    /// Turn spoken "comma", "period", "new line", ... into punctuation.
    pub spoken_punctuation: bool,
}
impl Default for FormattingConfig {
    fn default() -> Self {
        Self {
            remove_fillers: true,
            fillers: [
                "um", "umm", "uh", "uhm", "er", "erm", "ah", "hmm", "mm", "mhm",
            ]
            .map(String::from)
            .to_vec(),
            spoken_punctuation: false,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DictionaryConfig {
    /// Names and terms sent as recognition hints and kept verbatim by cleanup.
    pub words: Vec<String>,
    /// Whole-word, case-insensitive corrections applied to every transcript.
    pub replacements: Vec<Replacement>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Replacement {
    pub from: String,
    /// May be empty to delete a word.
    pub to: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snippet {
    pub trigger: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Style {
    pub name: String,
    /// Case-insensitive substrings of the focused app id, e.g. "slack".
    pub apps: Vec<String>,
    /// Overrides `cleanup.mode` for these apps.
    #[serde(default)]
    pub mode: Option<CleanupMode>,
    #[serde(default)]
    pub prompt: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SoundsConfig {
    pub enabled: bool,
    pub volume: f32,
    /// Custom sound files; unset uses the built-in cues.
    pub start: Option<PathBuf>,
    pub stop: Option<PathBuf>,
    pub error: Option<PathBuf>,
}
impl Default for SoundsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 0.5,
            start: None,
            stop: None,
            error: None,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    /// Desktop notifications for errors and clipboard-only fallbacks.
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
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
/// Terminal UI preferences. Only the TUI interprets theme contents.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub theme: String,
    /// User themes: name -> { slot -> color }. Slots and colors are validated by the TUI.
    pub themes: BTreeMap<String, BTreeMap<String, String>>,
    pub mouse: bool,
}
impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "paper".into(),
            themes: BTreeMap::new(),
            mouse: true,
        }
    }
}

fn check_text(field: &str, value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    if value.len() > max {
        bail!("{field} exceeds {max} bytes");
    }
    Ok(())
}
fn check_count(field: &str, count: usize, max: usize) -> Result<()> {
    if count > max {
        bail!("{field} has more than {max} entries");
    }
    Ok(())
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
        if self.recording.min_ms > 5000 {
            bail!("recording.min_ms must be 0..5000");
        }
        if self.recording.auto_stop_secs > 60 {
            bail!("recording.auto_stop_secs must be 0..60");
        }
        if self.recording.keep_warm_secs > 600 {
            bail!("recording.keep_warm_secs must be 0..600");
        }
        if let Some(device) = &self.recording.device {
            check_text("recording.device", device, 256)?;
        }
        if self.stt.timeout_secs == 0 || self.stt.timeout_secs > 300 {
            bail!("stt.timeout_secs must be 1..300");
        }
        if !(1..=120).contains(&self.cleanup.timeout_secs) {
            bail!("cleanup.timeout_secs must be 1..120");
        }
        if let Some(prompt) = &self.cleanup.prompt {
            check_text("cleanup.prompt", prompt, MAX_PROMPT_BYTES)?;
        } else if self.cleanup.mode == CleanupMode::Custom {
            bail!("cleanup.mode = \"custom\" requires cleanup.prompt");
        }
        if self.injection.restore_delay_ms > 10_000 {
            bail!("injection.restore_delay_ms must be 0..10000");
        }
        if self.privacy.history_limit > 100_000 {
            bail!("history_limit exceeds 100000");
        }
        check_count("formatting.fillers", self.formatting.fillers.len(), 200)?;
        for filler in &self.formatting.fillers {
            check_text("formatting.fillers entry", filler, 64)?;
        }
        check_count(
            "dictionary.words",
            self.dictionary.words.len() + self.stt.vocabulary.len(),
            MAX_DICTIONARY_ITEMS,
        )?;
        for word in self.dictionary.words.iter().chain(&self.stt.vocabulary) {
            check_text("dictionary word", word, MAX_WORD_BYTES)?;
        }
        check_count(
            "dictionary.replacements",
            self.dictionary.replacements.len(),
            MAX_DICTIONARY_ITEMS,
        )?;
        for replacement in &self.dictionary.replacements {
            check_text(
                "dictionary replacement 'from'",
                &replacement.from,
                MAX_WORD_BYTES,
            )?;
            if replacement.to.len() > MAX_WORD_BYTES {
                bail!("dictionary replacement 'to' exceeds {MAX_WORD_BYTES} bytes");
            }
        }
        check_count("snippets", self.snippets.len(), MAX_SNIPPETS)?;
        let mut triggers = HashSet::new();
        for snippet in &self.snippets {
            check_text("snippet trigger", &snippet.trigger, MAX_WORD_BYTES)?;
            check_text("snippet text", &snippet.text, MAX_SNIPPET_BYTES)?;
            if !triggers.insert(snippet.trigger.trim().to_lowercase()) {
                bail!("duplicate snippet trigger {:?}", snippet.trigger);
            }
        }
        check_count("styles", self.styles.len(), MAX_STYLES)?;
        let mut names = HashSet::new();
        for style in &self.styles {
            check_text("style name", &style.name, MAX_WORD_BYTES)?;
            if !names.insert(style.name.trim().to_lowercase()) {
                bail!("duplicate style name {:?}", style.name);
            }
            if style.apps.is_empty() {
                bail!("style {:?} must list at least one app", style.name);
            }
            for app in &style.apps {
                check_text("style app", app, MAX_WORD_BYTES)?;
            }
            if let Some(prompt) = &style.prompt {
                check_text("style prompt", prompt, MAX_PROMPT_BYTES)?;
            } else if style.mode == Some(CleanupMode::Custom) {
                bail!(
                    "style {:?} uses mode \"custom\" without a prompt",
                    style.name
                );
            }
        }
        if !self.sounds.volume.is_finite() || !(0.0..=1.0).contains(&self.sounds.volume) {
            bail!("sounds.volume must be 0.0..1.0");
        }
        check_text("ui.theme", &self.ui.theme, 64)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_template_is_valid_and_matches_defaults() {
        let template: Config =
            toml::from_str(include_str!("../../../config/default.toml")).unwrap();
        template.validate().unwrap();
        assert_eq!(template, Config::default());
    }

    #[test]
    fn mvp_configs_still_load() {
        let config: Config = toml::from_str(
            "[stt]\nprovider = \"groq\"\nvocabulary = []\ntimeout_secs = 30\n\
             [cleanup]\nmode = \"raw\"\napi_key_env = \"OPENAI_API_KEY\"\n\
             [privacy]\nhistory = true\nhistory_limit = 500\noffline = false\n\
             [recording]\nmax_seconds = 120\nsilence_threshold = 0.005\n\
             [injection]\nclipboard_only = true\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.injection.effective_method(),
            InjectionMethod::Clipboard
        );
    }

    #[test]
    fn defaults_round_trip_through_toml() {
        let mut config = Config::default();
        config.snippets.push(Snippet {
            trigger: "my email".into(),
            text: "me@example.com".into(),
        });
        config.styles.push(Style {
            name: "chat".into(),
            apps: vec!["slack".into()],
            mode: Some(CleanupMode::Light),
            prompt: None,
        });
        let text = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn invalid_values_are_rejected() {
        let cases: [fn(&mut Config); 8] = [
            |c| c.cleanup.mode = CleanupMode::Custom,
            |c| c.recording.auto_stop_secs = 61,
            |c| c.sounds.volume = 1.5,
            |c| c.dictionary.words.push(" ".into()),
            |c| {
                c.snippets.push(Snippet {
                    trigger: "Hi".into(),
                    text: "a".into(),
                });
                c.snippets.push(Snippet {
                    trigger: "hi ".into(),
                    text: "b".into(),
                });
            },
            |c| {
                c.styles.push(Style {
                    name: "x".into(),
                    apps: vec![],
                    mode: None,
                    prompt: None,
                })
            },
            |c| c.cleanup.timeout_secs = 0,
            |c| c.cleanup.prompt = Some("x".repeat(MAX_PROMPT_BYTES + 1)),
        ];
        for (index, case) in cases.iter().enumerate() {
            let mut config = Config::default();
            case(&mut config);
            assert!(config.validate().is_err(), "case {index} should fail");
        }
    }
}
