//! Plain-language copy and the closed option lists behind every setting.
//! Users never see config keys, TOML, paths or raw errors; this module owns that translation.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub const CAPTURE: &str = "capture";
pub const ADVANCED: &str = "action.advanced";

/// (key, group, label, help). `action.*` rows run an action instead of opening a picker.
pub type Row = (&'static str, &'static str, &'static str, &'static str);
pub const SETTINGS: &[Row] = &[
    ("action.setup", "Start", "Guided setup", "Walk through the speech service, key, microphone and shortcut again."),
    ("stt.provider", "Speech", "Service", "The service that turns your voice into text. Changing it picks that service's recommended model."),
    ("stt.model", "Speech", "Model", "Models offered by the speech service you chose."),
    ("action.stt_key", "Speech", "API key", "Your key for the speech service. xflow stores it encrypted on this computer, never in your settings file."),
    ("stt.language", "Speech", "Language", "Automatic works for most people. Pick a language if words come out in the wrong one."),
    ("recording.device", "Speech", "Microphone", "Which microphone to listen to. System default follows your computer's sound settings."),
    ("recording.ignore_speakers", "Speech", "Ignore speaker sound", "Off by default. On macOS, reduce speaker sound with the system default microphone; listening can take longer to start. On Linux, configure an echo-cancel input in Sound settings."),
    ("action.stt_test", "Speech", "Check connection", "Checks your key and connection without recording or using paid credits."),
    ("hotkey.key", "Shortcut", "Key", "Hold this key and speak; let go to finish. Double-tap it for hands-free."),
    ("hotkey.enabled", "Shortcut", "Shortcut", "Turn the global shortcut on or off."),
    ("hotkey.double_tap_ms", "Shortcut", "Double-tap speed", "How quickly two taps must follow each other to start hands-free mode."),
    ("hotkey.hold_threshold_ms", "Shortcut", "Hold delay", "How long to hold the key before listening starts."),
    ("cleanup.mode", "Writing", "Smart formatting", "On formats lists, numbers and corrections and spells your dictionary terms right, with the writing service below. Six words or fewer skip it. Off keeps your exact words. Polished also smooths phrasing."),
    ("cleanup.provider", "Writing", "Service", "The AI service that tidies your text when cleanup is on."),
    ("cleanup.model", "Writing", "Model", "Models offered by the writing service you chose."),
    ("action.cleanup_key", "Writing", "API key", "Your key for the writing service, stored encrypted by xflow."),
    ("formatting.remove_fillers", "Writing", "Remove um and uh", "Leave out filler words such as um and uh."),
    ("formatting.spoken_punctuation", "Writing", "Spoken punctuation", "Say \"comma\", \"period\" or \"new line\" to type them."),
    ("formatting.lists", "Writing", "Spoken lists", "Say \"one …, two …, three …\" or \"bullet …\" to get a list. Never in terminals."),
    ("injection.method", "Delivery", "Text arrives by", "Paste is fastest. Typing works where pasting is blocked. Copy only leaves the text ready for you to paste."),
    ("injection.trailing_space", "Delivery", "Space after text", "Add a space after each dictation so the next one doesn't run into it."),
    ("injection.restore_clipboard", "Delivery", "Restore clipboard", "Put back what you had copied before xflow pasted."),
    ("sounds.enabled", "Sounds", "Sounds", "Play a short sound when listening starts and stops."),
    ("sounds.volume", "Sounds", "Volume", "How loud those sounds are."),
    ("notifications.enabled", "Sounds", "Notifications", "Show a notification when something needs your attention."),
    ("privacy.history", "Privacy", "Save history", "Keep your dictations on this computer so you can find them later. Turning this off keeps what's already saved."),
    ("privacy.history_limit", "Privacy", "Keep last", "Older dictations are removed once this many are saved."),
    ("privacy.offline", "Privacy", "Local services only", "Only use services running on this computer. Cloud services stop working while this is on."),
    ("cleanup.app_context", "Privacy", "Share app name", "Tell the writing service which app you're typing in (never what's on screen), so it can match the tone."),
    ("ui.theme", "Look", "Theme", "Colors for this window."),
    ("ui.mouse", "Look", "Mouse", "Click and scroll in this window. The keyboard always works too."),
    (ADVANCED, "Advanced", "Advanced settings", "Recording limits and time limits. Most people never need these."),
];
pub const ADVANCED_SETTINGS: &[Row] = &[
    (
        "recording.max_seconds",
        "Advanced",
        "Longest recording",
        "Recording stops by itself after this long.",
    ),
    (
        "recording.auto_stop_secs",
        "Advanced",
        "Stop after silence",
        "Stop listening after this much silence.",
    ),
    (
        "recording.min_ms",
        "Advanced",
        "Ignore short taps",
        "Recordings shorter than this are ignored and never uploaded.",
    ),
    (
        "recording.keep_warm_secs",
        "Advanced",
        "Keep microphone ready",
        "Keep the microphone open briefly after a dictation so the next one starts faster.",
    ),
    (
        "recording.silence_threshold",
        "Advanced",
        "Silence sensitivity",
        "Lower values treat quieter sounds as speech.",
    ),
    (
        "injection.restore_delay_ms",
        "Advanced",
        "Clipboard restore delay",
        "Wait this long before putting your clipboard back.",
    ),
    (
        "stt.timeout_secs",
        "Advanced",
        "Speech time limit",
        "Give up on the speech service after this long.",
    ),
    (
        "cleanup.timeout_secs",
        "Advanced",
        "Cleanup time limit",
        "Give up on cleanup after this long; your words are kept either way.",
    ),
];

pub fn row(key: &str) -> Option<&'static Row> {
    SETTINGS
        .iter()
        .chain(ADVANCED_SETTINGS)
        .find(|r| r.0 == key)
}

/// Closed choices for simple settings. Catalog-backed ones (providers, models,
/// languages, devices, themes) are built by the caller.
pub fn presets(key: &str) -> Vec<String> {
    let values: &[&str] = match key {
        "cleanup.mode" => &["raw", "light", "polished"],
        "injection.method" => &["paste", "type", "clipboard"],
        "hotkey.key" => &[
            "fn",
            "right_option",
            "left_option",
            "right_command",
            "right_control",
            "right_shift",
            "caps_lock",
            "f13",
            "f14",
            "f15",
            "f16",
            "f17",
            "f18",
            "f19",
            CAPTURE,
        ],
        "hotkey.double_tap_ms" => &["250", "350", "500"],
        "hotkey.hold_threshold_ms" => &["150", "200", "250", "350", "500"],
        "recording.max_seconds" => &["60", "300", "600", "1200"],
        "recording.min_ms" => &["0", "150", "300", "500", "1000"],
        "recording.auto_stop_secs" => &["0", "2", "5", "10", "30", "60"],
        "recording.keep_warm_secs" => &["0", "5", "15", "30", "60", "300", "600"],
        "recording.silence_threshold" => &["0", "0.001", "0.005", "0.01", "0.02", "0.05"],
        "stt.timeout_secs" => &["15", "30", "60", "120", "300"],
        "cleanup.timeout_secs" => &["10", "20", "30", "60", "120"],
        "injection.restore_delay_ms" => &["150", "300", "500", "1000"],
        "privacy.history_limit" => &["100", "500", "1000", "5000", "10000"],
        "sounds.volume" => &["0.25", "0.5", "0.75", "1"],
        _ => &["true", "false"],
    };
    values.iter().map(|s| s.to_string()).collect()
}

pub const LANGUAGES: &[(&str, &str)] = &[
    ("", "Automatic"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("hi", "Hindi"),
    ("pt", "Portuguese"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("ar", "Arabic"),
    ("ru", "Russian"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("sv", "Swedish"),
    ("da", "Danish"),
    ("no", "Norwegian"),
    ("fi", "Finnish"),
    ("cs", "Czech"),
    ("el", "Greek"),
    ("he", "Hebrew"),
    ("id", "Indonesian"),
    ("ms", "Malay"),
    ("vi", "Vietnamese"),
    ("th", "Thai"),
    ("bn", "Bengali"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("mr", "Marathi"),
    ("gu", "Gujarati"),
    ("kn", "Kannada"),
    ("ml", "Malayalam"),
    ("pa", "Punjabi"),
    ("ur", "Urdu"),
    ("ro", "Romanian"),
    ("hu", "Hungarian"),
    ("bg", "Bulgarian"),
    ("hr", "Croatian"),
    ("sr", "Serbian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("ca", "Catalan"),
    ("fa", "Persian"),
    ("sw", "Swahili"),
    ("af", "Afrikaans"),
];

/// One plain line per speech/writing service, shown in pickers and setup.
pub fn blurb(id: &str) -> &'static str {
    match id {
        "groq" => "fast, with a free tier",
        "openai" => "reliable and widely used",
        "deepgram" => "very fast, great for English",
        "assemblyai" => "accurate, strong punctuation",
        "elevenlabs" => "high accuracy in many languages",
        "gemini" => "Google's speech model",
        "mistral" => "European provider",
        "openrouter" => "one key for many AI models",
        "together" => "low-cost open models",
        "deepinfra" => "low-cost open models",
        "fireworks" => "fast open models",
        "ollama" => "runs on this computer",
        "local" => "a speech server on this computer",
        "custom" => "your own server",
        _ => "",
    }
}

fn provider_name(id: &str) -> String {
    xflow_providers::stt_providers()
        .iter()
        .chain(xflow_providers::cleanup_providers())
        .find(|p| p.id == id)
        .map(|p| p.name.to_string())
        .unwrap_or_else(|| title(id))
}
fn model_label(id: &str) -> String {
    xflow_providers::stt_providers()
        .iter()
        .chain(xflow_providers::cleanup_providers())
        .flat_map(|p| p.models.iter())
        .find(|m| m.id == id)
        .map(|m| m.label.to_string())
        .unwrap_or_else(|| id.into())
}
fn title(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Friendly name for a hotkey value: `right_option` → "Right Option", `ctrl+alt+space` → "Ctrl+Alt+Space".
pub fn key_label(value: &str) -> String {
    match value {
        "fn" => "Fn (Globe)".into(),
        CAPTURE => "Press a key to record it…".into(),
        _ if value.contains('+') => value.split('+').map(title).collect::<Vec<_>>().join("+"),
        _ => value
            .split('_')
            .map(|w| {
                if w.len() > 1 && w.starts_with('f') && w[1..].chars().all(|c| c.is_ascii_digit()) {
                    w.to_uppercase()
                } else {
                    title(w)
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// What a stored value means to a person. `value` is TOML text or a bare string; empty means unset.
pub fn value_text(key: &str, value: &str) -> String {
    let value = value.trim_matches('"');
    let unset = value.is_empty() || value == "(unset)";
    let n = value.parse::<f64>().ok();
    match key {
        _ if value == "true" => "On".into(),
        _ if value == "false" => "Off".into(),
        "recording.device" if unset => "System default".into(),
        "stt.language" => LANGUAGES
            .iter()
            .find(|(code, _)| *code == value)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| value.into()),
        "stt.model" | "cleanup.model" if unset => "Recommended".into(),
        "cleanup.provider" if unset => "None chosen".into(),
        "stt.provider" | "cleanup.provider" => {
            let blurb = blurb(value);
            if blurb.is_empty() {
                provider_name(value)
            } else {
                format!("{} — {blurb}", provider_name(value))
            }
        }
        "stt.model" | "cleanup.model" => model_label(value),
        "cleanup.mode" => match value {
            "raw" => "Off — exact words",
            "light" => "On — light touch",
            "polished" => "Polished",
            "custom" => "Your own instructions",
            _ => value,
        }
        .into(),
        "injection.method" => match value {
            "paste" => "Paste",
            "type" => "Typing",
            "clipboard" => "Copy only",
            _ => value,
        }
        .into(),
        "hotkey.key" => key_label(value),
        "ui.theme" => title(&value.replace('-', " ")),
        "sounds.volume" => n
            .map(|v| format!("{:.0}%", v * 100.0))
            .unwrap_or_else(|| value.into()),
        "privacy.history_limit" => format!("{value} dictations"),
        "recording.auto_stop_secs" | "recording.keep_warm_secs" | "recording.min_ms"
            if n == Some(0.0) =>
        {
            "Off".into()
        }
        "recording.silence_threshold" if n == Some(0.0) => "Off".into(),
        k if k.ends_with("_ms") => format!("{value} ms"),
        k if k.ends_with("_secs") || k.ends_with("_seconds") => format!("{value} seconds"),
        _ if unset => "Default".into(),
        _ => value.into(),
    }
}

/// Plain-English replacement for daemon, provider and config errors.
/// Never shows paths, sockets, TOML or stack text.
pub fn plain(error: &str) -> String {
    let lower = error.to_lowercase();
    let rules: &[(&str, &str)] = &[
        ("cleanup endpoint or provider is required", "Cleanup needs a writing service. Choose one in Settings → Writing → Service."),
        ("cleanup model is required", "Choose a model in Settings → Writing → Model."),
        ("changed outside", "Your settings were changed somewhere else, so xflow reloaded them. Please make your choice again."),
        ("api key unavailable", "This service needs an API key. Add it in Settings → Speech → API key."),
        ("missing key", "This service needs an API key. Add it in Settings → Speech → API key."),
        ("401", "The service didn't accept your API key. Paste it again in Settings → Speech → API key."),
        ("403", "The service didn't accept your API key. Paste it again in Settings → Speech → API key."),
        ("gemini asr language", "Gemini can't use that language. Pick another language or Automatic."),
        ("invalid provider endpoint", "That service needs a server address. Set it up with the xflow setup command, or choose another service."),
        ("custom endpoint", "That service needs a server address. Set it up with the xflow setup command, or choose another service."),
        ("offline mode only permits", "Local services only is on, so cloud services are blocked. Turn it off in Settings → Privacy."),
        ("requires a literal loopback", "That service must run on this computer. Choose another service, or set it up with the xflow setup command."),
        ("no transcript yet", "Nothing dictated yet. Press Space to start."),
        ("no desktop display", "xflow can't reach your desktop to paste here. Your text is saved in History."),
        ("no input device", "No microphone found. Plug one in, then choose it in Settings → Speech → Microphone."),
        ("microphone", "xflow can't use the microphone. Check that it's plugged in and that microphone access is allowed in your system settings."),
        ("permission", "xflow isn't allowed to do that yet. Allow microphone and accessibility access for your terminal in your system settings."),
        ("busy", "xflow is busy with a dictation. Try again when it finishes."),
        ("connection refused", "xflow's background helper isn't running yet — xflow starts it by itself."),
        ("cannot connect", "xflow's background helper isn't running yet — xflow starts it by itself."),
        ("no such file or directory (os error 2)", "xflow's background helper isn't running yet — xflow starts it by itself."),
        ("daemon unavailable", "xflow's background helper isn't running yet — xflow starts it by itself."),
        ("daemon disconnected", "xflow's background helper stopped — xflow starts it again by itself."),
        ("timed out", "The service took too long to answer. Check your internet connection and try again."),
        ("dns", "Can't reach the service. Check your internet connection."),
        ("error sending request", "Can't reach the service. Check your internet connection."),
        ("still in the macos keychain", "Your key is still in the macOS Keychain. Run xflow key status in a terminal once to move it (macOS asks once)."),
        ("still in your system keyring", "Your key is still in your system keyring. Run xflow key status in a terminal once to move it."),
        ("chmod 600", "Your saved keys are readable by other users. Run xflow doctor for the one-line fix."),
        ("xflow key set", "Your saved key can't be read. Add it again: Settings → API key."),
    ];
    if let Some((_, text)) = rules.iter().find(|(needle, _)| lower.contains(needle)) {
        return text.to_string();
    }
    // Unknown: keep the first sentence, without technical tails after a colon.
    let first = error
        .lines()
        .next()
        .unwrap_or("")
        .split(": ")
        .next()
        .unwrap_or("")
        .trim();
    if first.is_empty() {
        "Something went wrong. Please try again.".into()
    } else {
        format!("{}.", title(first.trim_end_matches('.')))
    }
}

/// Unicode glyph with an ASCII fallback for terminals without UTF-8.
pub fn glyph(fancy: &'static str, ascii: &'static str) -> &'static str {
    static UTF8: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let utf8 = *UTF8.get_or_init(|| {
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
            .is_none_or(|v| v.to_lowercase().contains("utf"))
    });
    if utf8 {
        fancy
    } else {
        ascii
    }
}

/// "3 min ago" style age for a unix timestamp.
pub fn ago(created_at: i64, now: i64) -> String {
    let s = (now - created_at).max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// A friendly app name from an app id: `com.apple.mail` → "Mail".
pub fn app_name(id: &str) -> String {
    let last = id.rsplit(['.', '/']).next().unwrap_or(id);
    match last.to_lowercase().as_str() {
        "slackmacgap" | "slack" => "Slack".into(),
        "vscode" | "code" => "VS Code".into(),
        _ => title(last),
    }
}

/// Recorded chord in hotkey syntax: lowercase modifiers joined with `+`, key last.
pub fn chord(key: KeyEvent) -> Option<String> {
    let code = match key.code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) if c.is_ascii_alphanumeric() => c.to_ascii_lowercase().to_string(),
        KeyCode::F(n) if (1..=24).contains(&n) => format!("f{n}"),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        _ => return None,
    };
    let mut parts = Vec::new();
    for (modifier, name) in [
        (KeyModifiers::CONTROL, "ctrl"),
        (KeyModifiers::ALT, "alt"),
        (KeyModifiers::SUPER, "cmd"),
        (KeyModifiers::SHIFT, "shift"),
    ] {
        if key.modifiers.contains(modifier) {
            parts.push(name.to_string());
        }
    }
    parts.push(code);
    Some(parts.join("+"))
}
