use crate::{AppContext, InjectionOutcome, Mode, State};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Bumped when a request or response changes incompatibly; reported by Status.
pub const PROTOCOL_VERSION: u32 = 2;

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
    HistoryPaste {
        id: i64,
    },
    ClearHistory,
    Stats,
    /// Re-read config.toml and rebuild providers/desktop services. On error the
    /// running configuration is kept and the error is returned.
    Reload,
    Subscribe,
    Shutdown,
}
impl Request {
    pub fn start() -> Self {
        Self::Start {
            mode: Mode::Dictation,
            context: None,
            t0_us: None,
        }
    }
    pub fn toggle() -> Self {
        Self::Toggle {
            mode: Mode::Dictation,
            context: None,
            t0_us: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub created_at: i64,
    pub text: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider output before local formatting/cleanup, when it differs.
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

/// Per-session latency breakdown in milliseconds.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Timings {
    /// Caller hotkey press -> capture running (needs `t0_us`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey_ms: Option<u32>,
    /// Start request -> microphone stream running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_ms: Option<u32>,
    pub audio_ms: u32,
    /// Stop -> transcript received.
    pub stt_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject_ms: Option<u32>,
    /// Stop -> text delivered (or ready, when delivery failed).
    pub total_ms: u32,
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
    /// Daemon version; on Status and the Subscribe snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
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
                t0_us: Some(42)
            }
        ));
    }

    #[test]
    fn status_frames_stay_small() {
        let json = serde_json::to_string(&Response::status(State::Idle, 0.0)).unwrap();
        assert_eq!(json, r#"{"ok":true,"state":"idle","level":0.0}"#);
    }
}
