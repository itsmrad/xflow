use crate::{InjectionOutcome, State};
use serde::{Deserialize, Serialize};

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    Status,
    Start,
    Stop,
    Toggle,
    Cancel,
    Last,
    CopyLast,
    PasteLast,
    History { limit: usize },
    ClearHistory,
    Subscribe,
    Shutdown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub created_at: i64,
    pub text: String,
    pub provider: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
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
}
impl Response {
    pub fn status(state: State, level: f32) -> Self {
        Self {
            ok: true,
            state,
            message: None,
            text: None,
            history: vec![],
            injection: None,
            level,
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
