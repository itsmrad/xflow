use crate::{AppContext, InjectionOutcome, Mode, State};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Bumped when a request or response changes incompatibly; reported by Status.
pub const PROTOCOL_VERSION: u32 = 4;
/// First protocol with `HistoryPaste::original` and `History::filter`. Older daemons
/// ignore both fields, so clients check `protocol` before sending them.
pub const HISTORY_ACTIONS_PROTOCOL: u32 = 4;

/// Where a finished transcript goes.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Insert into the focused app per `[injection]` (normal dictation).
    #[default]
    Inject,
    /// Copy to the clipboard only.
    Clipboard,
    /// Do not touch the desktop; the requesting client reads the final event
    /// (`xflow listen` prints it to stdout).
    None,
}

/// One daemon request plus the channel for its single reply. Shared by the
/// Unix-socket server and the D-Bus bridge.
pub struct Command {
    pub request: Request,
    pub reply: oneshot::Sender<Response>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// Begin capture. `context` is the focus the caller captured at hotkey time
    /// (the daemon then skips its own focus query); `t0_us` is the caller's
    /// CLOCK_MONOTONIC hotkey timestamp in microseconds, used only for latency stats.
    Start {
        #[serde(default)]
        mode: Mode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<AppContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        t0_us: Option<u64>,
        #[serde(default)]
        delivery: Delivery,
        /// The session belongs to this connection: the client keeps it open, the
        /// daemon closes it when the session ends and cancels the session if the
        /// client closes it first (or dies, `kill -9` included).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        hold: bool,
    },
    Stop,
    /// Start when idle, stop when listening; same fields as Start.
    Toggle {
        #[serde(default)]
        mode: Mode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<AppContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        t0_us: Option<u64>,
        #[serde(default)]
        delivery: Delivery,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        hold: bool,
    },
    Cancel,
    Last,
    CopyLast,
    PasteLast,
    /// Newest first. `query` matches transcript text case-insensitively.
    History {
        limit: usize,
        #[serde(default)]
        offset: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "HistoryFilter::is_empty")]
        filter: HistoryFilter,
    },
    HistoryGet {
        id: i64,
    },
    HistoryDelete {
        id: i64,
    },
    HistoryCopy {
        id: i64,
    },
    /// `original` pastes the entry's first text (`raw_text`) instead of its text.
    HistoryPaste {
        id: i64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        original: bool,
    },
    /// Replace an entry's text with the user's correction (its first text is kept
    /// as `raw_text`) and, with `dictionary.auto_learn`, learn the corrected words.
    /// Replies with the updated `entry`; `message` names any learned words.
    HistoryUpdate {
        id: i64,
        text: String,
    },
    ClearHistory,
    Stats,
    /// Transcribe the kept failed dictation again (`privacy.keep_failed_audio`), then
    /// clean up and deliver it like a new one. Parts already delivered are left out
    /// unless `full`; `discard` deletes the kept audio instead.
    Retry {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        full: bool,
        #[serde(default)]
        delivery: Delivery,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        discard: bool,
    },
    /// Re-read config.toml and rebuild providers/desktop services. On error the
    /// running configuration is kept and the error is returned.
    Reload,
    Subscribe,
    /// macOS: ask the system for `grant` so xflowd appears in its System Settings
    /// list (and the system prompt shows when undecided). Replies with Status.
    Allow {
        grant: Grant,
    },
    Shutdown,
    /// Undo the latest automatic dictionary change once: `op` from the pill's
    /// notice, or None for the latest (`xflow dictionary undo`). Refused, with
    /// nothing written, once config.toml changed after it or the daemon restarted.
    UndoLearned {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op: Option<u64>,
    },
    /// Daemon-internal, never on the wire: the socket could not deliver the start
    /// reply for `session`, so cancel that session (and no later one).
    #[serde(skip)]
    Abandon {
        session: u64,
    },
}
impl Request {
    pub fn start() -> Self {
        Self::Start {
            mode: Mode::Dictation,
            context: None,
            t0_us: None,
            delivery: Delivery::Inject,
            hold: false,
        }
    }
    pub fn toggle() -> Self {
        Self::Toggle {
            mode: Mode::Dictation,
            context: None,
            t0_us: None,
            delivery: Delivery::Inject,
            hold: false,
        }
    }
}

/// Narrows `History`; an empty filter matches every entry.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryFilter {
    /// Exact `app_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    /// Unix seconds: entries created at or after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}
impl HistoryFilter {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A failed dictation's audio kept on disk for `retry`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailedAudio {
    /// Unix seconds; the audio is deleted at `expires_at`.
    pub created_at: i64,
    pub expires_at: i64,
    /// Kept audio still to transcribe.
    pub audio_ms: u64,
    /// Part of the dictation was already delivered; a retry delivers only the rest.
    pub partial: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub created_at: i64,
    pub text: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The first text of the entry, when it differs: provider output before local
    /// formatting/cleanup, or the text before a history correction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Recorded audio length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Stop -> text delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub mode: Mode,
    /// What cleanup did; unset when it was off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<CleanupOutcome>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CleanupOutcome {
    /// The cleaned text was delivered.
    Applied,
    /// Too short, mostly code, or a terminal: not sent.
    Skipped,
    /// The model answered or rewrote; the local text was delivered.
    Rejected,
    /// Past the dictation deadline; the local text was delivered.
    Slow,
    /// The provider failed; the local text was delivered.
    Failed,
    /// The speaker took it all back ("scratch that"); nothing was delivered.
    Discarded,
}
impl CleanupOutcome {
    pub const ALL: [Self; 6] = [
        Self::Applied,
        Self::Skipped,
        Self::Rejected,
        Self::Slow,
        Self::Failed,
        Self::Discarded,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Skipped => "skipped",
            Self::Rejected => "rejected",
            Self::Slow => "slow",
            Self::Failed => "failed",
            Self::Discarded => "discarded",
        }
    }
}

/// Aggregates over persisted history (local only).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Stats {
    pub sessions: u64,
    pub words: u64,
    pub audio_ms: u64,
    pub sessions_today: u64,
    pub words_today: u64,
    /// Consecutive local days, ending today or yesterday, with a session.
    pub streak_days: u32,
    /// Words per minute of recorded speech.
    pub wpm: Option<f64>,
    pub latency_p50_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
}

/// Correction learning since xflowd started: counts only, never field text,
/// dictated or learned words.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Learning {
    /// Field watches started after a paste.
    pub watches: u32,
    /// How each watch ended ("learned", "unsettled", "filtered:dissimilar", ...).
    pub outcomes: std::collections::BTreeMap<String, u32>,
    /// Field reads, and the most change notifications seen between two reads.
    pub reads: u32,
    pub unread_max: u32,
    /// Change notifications that arrived while a read ran, or in the grace after
    /// the dictation left its field.
    pub late: u32,
    /// Value changes the desktop folded into the one before (see `FieldEvent::Merged`).
    pub merged: u32,
    /// Reads of a field still being edited away from the dictation.
    pub editing: u32,
    /// The daemon's dictionary: entry counts, and its `DictionaryConfig::fingerprint`
    /// to compare with config.toml.
    pub words: u32,
    pub learned: u32,
    pub replacements: u32,
    pub fingerprint: String,
    /// An automatic change can still be undone (`xflow dictionary undo`).
    pub undo: bool,
}

/// Per-session latency breakdown in milliseconds.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Timings {
    /// Caller hotkey press -> capture running (needs `t0_us`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey_ms: Option<u32>,
    /// Start request -> microphone stream running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_ms: Option<u32>,
    /// Caller hotkey press -> first microphone sample of this dictation (needs `t0_us`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_sample_ms: Option<u32>,
    /// Caller hotkey press -> recording pill on screen (macOS; needs `t0_us`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pill_ms: Option<u32>,
    pub audio_ms: u32,
    /// Stop -> transcript received.
    pub stt_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_ms: Option<u32>,
    /// Light cleanup wasn't called: too short to improve.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cleanup_skipped: bool,
    /// Cleanup output was discarded for changing too much; local text was kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cleanup_rejected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject_ms: Option<u32>,
    /// Stop -> text delivered (or ready, when delivery failed).
    pub total_ms: u32,
}

pub const INPUT_MONITORING_FIX: &str = "Input Monitoring is off for xflowd — run xflow, open Check-up and press Enter on it, then find xflowd in the list and switch it on";
pub const ACCESSIBILITY_FIX: &str = "Accessibility is off for xflowd (it types your words and hears your shortcut) — run xflow, open Check-up and press Enter on it, then find xflowd in the list and switch it on";
pub const UNVERIFIED_SHORTCUT: &str = "No keyboard events received by xflowd yet — press the shortcut to check it. If it stays silent, run xflow and open Check-up";
/// Bare executables are listed by path, so old copies leave look-alike rows behind.
pub const STALE_ROWS: &str = "If you see xflowd already switched on, it's an old copy: select it and click −, then press Enter here.";

/// A macOS privacy grant xflowd needs.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    InputMonitoring,
    Accessibility,
    Microphone,
}
impl Grant {
    /// The exact System Settings pane, for `open`.
    pub fn pane(self) -> &'static str {
        match self {
            Self::InputMonitoring => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
            }
            Self::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            Self::Microphone => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
        }
    }
}

/// Nonprompting checks made by xflowd, never by the client displaying them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Permissions {
    pub accessibility: bool,
    pub input_monitoring: bool,
    /// None when the global shortcut is disabled or no tap could be created.
    pub keyboard_events: Option<bool>,
    /// Only in an `Allow { grant: Microphone }` reply; None while undecided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microphone: Option<bool>,
}
impl Permissions {
    /// The listen-only tap gets key events with either grant: Accessibility trust
    /// covers it too (CGEventTapCreate), so Input Monitoring is only a fallback.
    /// Terminal correction replay subscribes to this same tap and does not ask again.
    pub fn events(&self) -> bool {
        self.input_monitoring || self.accessibility
    }
    pub fn warning(&self) -> Option<&'static str> {
        if !self.accessibility {
            Some(ACCESSIBILITY_FIX)
        } else if self.keyboard_events == Some(false) {
            Some(UNVERIFIED_SHORTCUT)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub injection: Option<InjectionOutcome>,
    pub level: f32,
    /// Last successfully opened microphone, including selection notices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microphone: Option<String>,
    /// Daemon version; on Status and the Subscribe snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Permissions>,
    /// Mode of the active session while listening/processing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// History: total rows matching the query, for paging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// HistoryGet, and the final event of a successful session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<HistoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    /// Final event of a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<Timings>,
    /// The failed dictation kept for `retry`, on every status and event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_audio: Option<FailedAudio>,
    /// Status only: count-only correction-learning diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning: Option<Learning>,
    /// What an error `message` means, for the shared presenter; `message` is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<crate::problem::Problem>,
    /// Daemon-internal, never on the wire: the session a successful start began.
    #[serde(skip)]
    pub session: Option<u64>,
}
impl Response {
    pub fn status(state: State, level: f32) -> Self {
        Self {
            ok: true,
            state,
            level,
            ..Self::default()
        }
    }
    pub fn error(state: State, message: impl Into<String>) -> Self {
        let mut response = Self::status(state, 0.0);
        response.ok = false;
        let message = message.into();
        let mut end = message.len().min(4093);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        response.message = Some(if end < message.len() {
            format!("{}...", &message[..end])
        } else {
            message
        });
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_status_is_optional_and_round_trips() {
        let old: Response =
            serde_json::from_str(r#"{"ok":true,"state":"idle","level":0.0}"#).unwrap();
        assert!(old.microphone.is_none());
        let mut response = old;
        response.microphone = Some("Built-in (Bluetooth headphones kept in high quality)".into());
        let json = serde_json::to_string(&response).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.microphone, response.microphone);
    }

    #[test]
    fn v1_clients_and_extension_requests_still_parse() {
        for (json, expected) in [
            (r#"{"command":"start"}"#, Request::start()),
            (r#"{"command":"toggle"}"#, Request::toggle()),
            (
                r#"{"command":"history","limit":5}"#,
                Request::History {
                    limit: 5,
                    offset: 0,
                    query: None,
                    filter: HistoryFilter::default(),
                },
            ),
            (
                r#"{"command":"history_paste","id":7}"#,
                Request::HistoryPaste {
                    id: 7,
                    original: false,
                },
            ),
        ] {
            assert_eq!(serde_json::from_str::<Request>(json).unwrap(), expected);
        }
        let request: Request = serde_json::from_str(
            r#"{"command":"toggle","mode":"command","context":{"app_id":"code","window_id":"7","selected_text":null},"t0_us":42}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            Request::Toggle {
                mode: Mode::Command,
                context: Some(_),
                t0_us: Some(42),
                delivery: Delivery::Inject,
                hold: false,
            }
        ));
    }

    #[test]
    fn history_actions_stay_off_the_wire_unless_used() {
        // Protocol 3 daemons ignore unknown fields: plain requests must look the same.
        let plain = Request::HistoryPaste {
            id: 7,
            original: false,
        };
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"{"command":"history_paste","id":7}"#
        );
        let list = Request::History {
            limit: 5,
            offset: 0,
            query: None,
            filter: HistoryFilter::default(),
        };
        assert_eq!(
            serde_json::to_string(&list).unwrap(),
            r#"{"command":"history","limit":5,"offset":0}"#
        );
        for request in [
            Request::HistoryPaste {
                id: 7,
                original: true,
            },
            Request::History {
                limit: 5,
                offset: 0,
                query: Some("é".into()),
                filter: HistoryFilter {
                    app_id: Some("slack".into()),
                    mode: Some(Mode::Command),
                    since: Some(1_700_000_000),
                },
            },
        ] {
            let json = serde_json::to_string(&request).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
        }
        assert_eq!(
            serde_json::to_string(&Request::HistoryPaste {
                id: 7,
                original: true
            })
            .unwrap(),
            r#"{"command":"history_paste","id":7,"original":true}"#
        );
    }

    #[test]
    fn status_frames_stay_small() {
        let json = serde_json::to_string(&Response::status(State::Idle, 0.0)).unwrap();
        assert_eq!(json, r#"{"ok":true,"state":"idle","level":0.0}"#);
    }
}
