//! Streaming model gateway: one provider interface, three wires, and the timeouts,
//! retries and fallback around them. Cancel by dropping the future, which aborts the
//! HTTP body. Error text never carries a request body, key or provider message.
mod chat_completions;
mod messages;
mod responses;
mod sse;
#[cfg(test)]
mod tests;

use super::{
    client_with, endpoint, find_provider, is_loopback, read_bytes, Auth, Credentials, ProviderKind,
};
use async_trait::async_trait;
use reqwest::{Client, RequestBuilder, Response, Url};
use serde_json::Value;
use sse::Frame;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use xflow_core::config::{AiConfig, CleanupConfig};

const CONNECT: Duration = Duration::from_secs(5);
const FIRST_BYTE: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(45);
const TOTAL: Duration = Duration::from_secs(300);
const RETRIES: u32 = 2;
const RETRY_BASE: Duration = Duration::from_millis(500);
const RETRY_CAP: Duration = Duration::from_secs(8);
const RETRY_AFTER_MAX: Duration = Duration::from_secs(20);
const ERROR_BODY_BYTES: usize = 64 * 1024;
const STREAM_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    User(String),
    /// `opaque`: provider items to replay verbatim (Responses reasoning items).
    Assistant {
        text: String,
        tool_calls: Vec<ToolCall>,
        opaque: Vec<Value>,
    },
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub max_output_tokens: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    End,
    ToolUse,
    Length,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Text(String),
    ToolCall(ToolCall),
    Opaque(Value),
    Usage {
        input: u64,
        output: u64,
        cached: u64,
    },
    Done(Stop),
}
#[async_trait]
pub trait ChatProvider: Send + Sync {
    /// Opens the connection, like the cleanup `warm()`.
    async fn warm(&self) -> Result<(), LlmError>;
    async fn stream(&self, request: &ChatRequest, tx: &mpsc::Sender<Event>)
        -> Result<(), LlmError>;
}
#[derive(Clone, Debug, PartialEq)]
pub enum LlmError {
    Auth,
    RateLimit { retry_after: Option<Duration> },
    Overloaded,
    Network(String),
    Timeout,
    ContextTooLong,
    QuotaExhausted,
    NotLoggedIn,
    NotInstalled,
    Offline,
    Interrupted { partial: bool },
    Provider(String),
}
impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth => f.write_str("the provider rejected the API key"),
            Self::RateLimit {
                retry_after: Some(wait),
            } => write!(f, "rate limited; retry in {} s", wait.as_secs().max(1)),
            Self::RateLimit { retry_after: None } => f.write_str("rate limited; retry shortly"),
            Self::Overloaded => f.write_str("the provider is overloaded; retry shortly"),
            Self::Network(why) => write!(f, "network failure: {why}"),
            Self::Timeout => f.write_str("the provider timed out; retry shortly"),
            Self::ContextTooLong => f.write_str("the question and context are too long"),
            Self::QuotaExhausted => f.write_str("the account is out of credit or quota"),
            Self::NotLoggedIn => f.write_str("no API key is set"),
            Self::NotInstalled => f.write_str("the provider is not installed"),
            Self::Offline => f.write_str("privacy.offline only allows loopback providers"),
            Self::Interrupted { partial: true } => {
                f.write_str("the answer was cut off; the text above is incomplete")
            }
            Self::Interrupted { partial: false } => {
                f.write_str("the connection closed before any answer")
            }
            Self::Provider(why) => f.write_str(why),
        }
    }
}
impl std::error::Error for LlmError {}

fn malformed() -> LlmError {
    LlmError::Provider("the provider sent a malformed response".into())
}
fn json(data: &str) -> Result<Value, LlmError> {
    serde_json::from_str(data).map_err(|_| malformed())
}
/// Tool arguments are parsed only once complete; the first fragment may be "".
fn tool_args(text: &str) -> Option<Value> {
    if text.trim().is_empty() {
        return Some(Value::Object(Default::default()));
    }
    serde_json::from_str(text).ok().filter(Value::is_object)
}
/// Classified from the status, code and text, none of which are kept: providers echo
/// request content and partial keys into their messages.
fn classify(status: Option<u16>, code: &str, message: &str) -> LlmError {
    let text = format!("{code} {message}").to_ascii_lowercase();
    let names = |words: &[&str]| words.iter().any(|w| text.contains(w));
    let status = status.or_else(|| code.parse().ok());
    if status != Some(429)
        && names(&[
            "context_length",
            "context length",
            "prompt is too long",
            "input is too long",
            "too many tokens",
        ])
    {
        return LlmError::ContextTooLong;
    }
    if status == Some(402) || names(&["insufficient_quota", "credit balance", "billing_hard_limit"])
    {
        return LlmError::QuotaExhausted;
    }
    match status {
        Some(401 | 403) => return LlmError::Auth,
        Some(408) => return LlmError::Timeout,
        Some(429) if names(&["spend", "quota", "credit"]) => return LlmError::QuotaExhausted,
        Some(429) => return LlmError::RateLimit { retry_after: None },
        Some(500..=599) => return LlmError::Overloaded,
        _ => {}
    }
    if status.is_none() {
        if names(&["authentication", "invalid_api_key", "permission"]) {
            return LlmError::Auth;
        }
        if names(&["rate_limit"]) {
            return LlmError::RateLimit { retry_after: None };
        }
        if names(&["overloaded", "server_error", "api_error", "internal"]) {
            return LlmError::Overloaded;
        }
    }
    // Only an identifier-shaped code is safe to show.
    let safe = (1..=64).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    let label = match (status, safe) {
        (Some(status), true) if code.parse::<u16>().is_err() => format!("HTTP {status}, {code}"),
        (Some(status), _) => format!("HTTP {status}"),
        (None, true) => code.to_owned(),
        (None, false) => "unspecified error".to_owned(),
    };
    LlmError::Provider(format!("the provider returned an error ({label})"))
}
fn error_fields(value: &Value) -> (String, String) {
    let error = value
        .get("error")
        .filter(|e| e.is_object())
        .unwrap_or(value);
    let code = [&error["code"], &error["type"]]
        .into_iter()
        .find_map(|v| match v {
            Value::String(s) if s != "error" => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .unwrap_or_default();
    (
        code,
        error["message"].as_str().unwrap_or_default().to_owned(),
    )
}
/// An error object delivered inside a 200 stream.
fn stream_error(value: &Value) -> LlmError {
    let (code, message) = error_fields(value);
    classify(None, &code, &message)
}
fn credential_error(error: anyhow::Error) -> LlmError {
    let text = error.to_string();
    if text.contains("API key unavailable") || text.contains("run `xflow key status`") {
        LlmError::NotLoggedIn
    } else {
        LlmError::Provider(text)
    }
}
fn retry_after(response: &Response) -> Option<Duration> {
    let seconds: f64 = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    (0.0..86_400.0)
        .contains(&seconds)
        .then(|| Duration::from_secs_f64(seconds))
}
/// Full jitter: uniform in zero to the capped exponential backoff.
fn jitter(attempt: u32) -> Duration {
    use ring::rand::SecureRandom;
    let mut bytes = [0; 4];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    RETRY_BASE
        .saturating_mul(1 << attempt.min(8))
        .min(RETRY_CAP)
        .mul_f64(f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Wire {
    ChatCompletions,
    Responses,
    Messages,
}
enum Parser {
    ChatCompletions(chat_completions::Parser),
    Responses(responses::Parser),
    Messages(messages::Parser),
}
impl Parser {
    fn new(wire: Wire) -> Self {
        match wire {
            Wire::ChatCompletions => Self::ChatCompletions(Default::default()),
            Wire::Responses => Self::Responses(Default::default()),
            Wire::Messages => Self::Messages(Default::default()),
        }
    }
    /// True once the stream's terminal event has been handled.
    fn frame(&mut self, frame: &Frame, out: &mut Vec<Event>) -> Result<bool, LlmError> {
        match self {
            Self::ChatCompletions(p) => p.frame(frame, out),
            Self::Responses(p) => p.frame(frame, out),
            Self::Messages(p) => p.frame(frame, out),
        }
    }
    fn eof(&mut self, out: &mut Vec<Event>) -> Result<bool, LlmError> {
        match self {
            Self::ChatCompletions(p) => p.eof(out),
            _ => Ok(false),
        }
    }
}
struct Failure {
    error: LlmError,
    retry: bool,
    after: Option<Duration>,
}
impl From<LlmError> for Failure {
    fn from(error: LlmError) -> Self {
        Self {
            error,
            retry: false,
            after: None,
        }
    }
}
struct Route {
    provider: String,
    wire: Wire,
    url: Url,
    model: String,
    credential: Credentials,
    client: Client,
}
impl Route {
    fn build(
        id: &str,
        endpoint_override: Option<&str>,
        model: Option<&str>,
        offline: bool,
    ) -> Result<Self, LlmError> {
        let invalid = |text: &str| LlmError::Provider(text.to_owned());
        let client = client_with(TOTAL, CONNECT).map_err(|e| invalid(&e.to_string()))?;
        if id == "openai-compatible" {
            let url = chat_endpoint(
                endpoint_override
                    .ok_or_else(|| invalid("provider openai-compatible needs ai.endpoint"))?,
                offline,
            )?;
            let local = is_loopback(&url);
            return Ok(Self {
                provider: id.into(),
                wire: Wire::ChatCompletions,
                url,
                model: model
                    .filter(|m| !m.trim().is_empty())
                    .ok_or_else(|| invalid("provider openai-compatible needs ai.model"))?
                    .to_owned(),
                credential: Credentials::new(
                    "cleanup",
                    (!local).then(|| "XFLOW_AI_API_KEY".into()),
                    local,
                    false,
                )
                .map_err(credential_error)?,
                client,
            });
        }
        if id == "custom" {
            let url = endpoint_override
                .and_then(|value| endpoint(value, offline).ok())
                .filter(is_loopback)
                .ok_or_else(|| {
                    invalid("provider custom needs ai.endpoint on a loopback address")
                })?;
            return Ok(Self {
                provider: id.into(),
                wire: Wire::ChatCompletions,
                url,
                model: model
                    .filter(|m| !m.trim().is_empty())
                    .ok_or_else(|| invalid("provider custom needs ai.model"))?
                    .to_owned(),
                // Anonymous: no stored key ever goes to a local server.
                credential: Credentials::new(id, None, true, false)
                    .map_err(|e| invalid(&e.to_string()))?,
                client,
            });
        }
        let info = find_provider(ProviderKind::Chat, id).ok_or_else(|| {
            LlmError::Provider(format!(
                "unknown AI provider {id:?}; use openai, anthropic, groq, openrouter, openai-compatible or custom"
            ))
        })?;
        if endpoint_override.is_some() {
            return Err(invalid("ai.endpoint applies only to provider custom"));
        }
        if offline {
            return Err(LlmError::Offline);
        }
        Ok(Self {
            provider: id.into(),
            wire: match id {
                "openai" => Wire::Responses,
                "anthropic" => Wire::Messages,
                _ => Wire::ChatCompletions,
            },
            url: endpoint(info.endpoint, false).map_err(|e| invalid(&e.to_string()))?,
            model: model.unwrap_or(info.default_model).to_owned(),
            credential: Credentials::new(id, info.env_var.map(str::to_owned), false, true)
                .map_err(|e| invalid(&e.to_string()))?,
            client,
        })
    }
    fn headers(&self, builder: RequestBuilder) -> RequestBuilder {
        match self.wire {
            Wire::Messages => builder.header("anthropic-version", messages::VERSION),
            _ => builder,
        }
    }
    fn auth(&self) -> Auth {
        match self.wire {
            Wire::Messages => Auth::Raw("x-api-key"),
            _ => Auth::Bearer,
        }
    }
    async fn status_failure(
        &self,
        mut response: Response,
        deadline: tokio::time::Instant,
    ) -> Failure {
        let status = response.status().as_u16();
        let after = retry_after(&response);
        if matches!(status, 401 | 403) {
            self.credential.invalidate().await;
            return LlmError::Auth.into();
        }
        let mut bytes = Vec::new();
        while bytes.len() < ERROR_BODY_BYTES {
            match tokio::time::timeout_at(deadline, response.chunk()).await {
                Ok(Ok(Some(chunk))) => bytes.extend_from_slice(&chunk),
                _ => break,
            }
        }
        let (code, message) = serde_json::from_slice(&bytes)
            .map(|value: Value| error_fields(&value))
            .unwrap_or_default();
        let mut error = classify(Some(status), &code, &message);
        // A 429 that says when to come back is a rate limit whatever else it names.
        if status == 429 && after.is_some() {
            error = LlmError::RateLimit { retry_after: after };
        }
        let retry = matches!(status, 408 | 429 | 500..=599)
            && !matches!(error, LlmError::QuotaExhausted | LlmError::ContextTooLong)
            && after.is_none_or(|wait| wait <= RETRY_AFTER_MAX);
        Failure {
            error,
            retry,
            after,
        }
    }
    /// One request. `output` turns true when the first Text or ToolCall is sent.
    async fn attempt(
        &self,
        request: &ChatRequest,
        model: &str,
        tx: &mpsc::Sender<Event>,
        output: &mut bool,
        overhead: &AtomicU64,
    ) -> Result<(), Failure> {
        let started = Instant::now();
        let mut body = match self.wire {
            Wire::ChatCompletions => {
                chat_completions::body(request, model, self.provider == "openrouter")
            }
            Wire::Responses => responses::body(request, model),
            Wire::Messages => messages::body(request, model),
        };
        if matches!(self.wire, Wire::ChatCompletions) {
            // Preserve the verified cleanup model's low-latency reasoning settings.
            super::disable_reasoning(&mut body, &self.provider, model);
        }
        let builder = self.headers(self.client.post(self.url.clone())).json(&body);
        let builder = self
            .credential
            .apply(builder, self.auth())
            .await
            .map_err(credential_error)?;
        let mut local = started.elapsed();
        let deadline = tokio::time::Instant::now() + FIRST_BYTE;
        let mut response = match tokio::time::timeout_at(deadline, builder.send()).await {
            Err(_) => return Err(LlmError::Timeout.into()),
            Ok(Err(error)) if error.is_connect() => {
                return Err(Failure {
                    error: LlmError::Network(super::connect_failure(&error).into()),
                    retry: true,
                    after: None,
                })
            }
            Ok(Err(error)) if error.is_timeout() => return Err(LlmError::Timeout.into()),
            Ok(Err(_)) => return Err(LlmError::Network("the connection failed".into()).into()),
            Ok(Ok(response)) => response,
        };
        if !response.status().is_success() {
            return Err(self.status_failure(response, deadline).await);
        }
        let mut frames = sse::Parser::default();
        let mut parser = Parser::new(self.wire);
        let (mut batch, mut events, mut held) = (Vec::new(), Vec::new(), Vec::new());
        let (mut received, mut first, mut finished) = (0, true, false);
        while !finished {
            // Any byte, a keep-alive comment included, counts as activity.
            let next = if std::mem::take(&mut first) {
                tokio::time::timeout_at(deadline, response.chunk()).await
            } else {
                tokio::time::timeout(IDLE, response.chunk()).await
            };
            let ended = match next {
                Err(_) => return Err(cut(LlmError::Timeout, *output)),
                Ok(Err(error)) if error.is_timeout() => {
                    return Err(cut(LlmError::Timeout, *output))
                }
                Ok(Err(_)) => return Err(cut(LlmError::Interrupted { partial: false }, *output)),
                Ok(Ok(None)) => true,
                Ok(Ok(Some(chunk))) => {
                    received += chunk.len();
                    if received > STREAM_BYTES {
                        let error = LlmError::Provider("the provider response is too large".into());
                        return Err(cut(error, *output));
                    }
                    let parsing = Instant::now();
                    frames.push(&chunk, &mut batch);
                    if !*output {
                        local += parsing.elapsed();
                    }
                    false
                }
            };
            if ended {
                frames.finish(&mut batch);
            }
            let parsing = Instant::now();
            for frame in batch.drain(..) {
                match parser.frame(&frame, &mut events) {
                    Ok(done) => finished = done,
                    // An error object from the server, before any output, may pass.
                    Err(error) => {
                        send(&mut events, &mut held, tx, output).await?;
                        let retry = error != malformed()
                            && !matches!(
                                error,
                                LlmError::ContextTooLong
                                    | LlmError::Auth
                                    | LlmError::QuotaExhausted
                            );
                        let mut failure = cut(error, *output);
                        failure.retry = retry && !*output;
                        return Err(failure);
                    }
                }
                if finished {
                    break;
                }
            }
            if ended && !finished {
                match parser.eof(&mut events) {
                    Ok(true) => finished = true,
                    Ok(false) => {
                        send(&mut events, &mut held, tx, output).await?;
                        return Err(cut(LlmError::Interrupted { partial: false }, *output));
                    }
                    Err(error) => return Err(cut(error, *output)),
                }
            }
            if !*output {
                local += parsing.elapsed();
                if events.iter().any(is_output) {
                    let _ = overhead.compare_exchange(
                        0,
                        local.as_micros() as u64,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
            }
            send(&mut events, &mut held, tx, output).await?;
        }
        Ok(())
    }
}
fn is_output(event: &Event) -> bool {
    matches!(event, Event::Text(_) | Event::ToolCall(_))
}
/// After the first output there is no retry, only `Interrupted { partial: true }`.
fn cut(error: LlmError, output: bool) -> Failure {
    if output {
        LlmError::Interrupted { partial: true }.into()
    } else {
        error.into()
    }
}
/// Hold reasoning and usage until output or the end, so that an attempt that fails
/// and is retried leaves nothing behind for the caller to replay or count.
async fn send(
    events: &mut Vec<Event>,
    held: &mut Vec<Event>,
    tx: &mpsc::Sender<Event>,
    output: &mut bool,
) -> Result<(), Failure> {
    for event in events.drain(..) {
        if !*output && matches!(event, Event::Opaque(_) | Event::Usage { .. }) {
            held.push(event);
            continue;
        }
        if is_output(&event) || matches!(event, Event::Done(_)) {
            for earlier in held.drain(..) {
                tx.send(earlier).await.map_err(|_| closed(*output))?;
            }
        }
        *output |= is_output(&event);
        tx.send(event).await.map_err(|_| closed(*output))?;
    }
    Ok(())
}
fn closed(output: bool) -> Failure {
    LlmError::Interrupted { partial: output }.into()
}

/// The only production `ChatProvider`: a primary and its fallbacks, each with the
/// shared timeouts and retries.
pub struct Gateway {
    routes: Vec<Route>,
    overhead_us: AtomicU64,
}
impl Gateway {
    /// Local time before the first output of the last `stream`: credentials, body
    /// encoding and parsing, without the network or the server.
    pub fn overhead(&self) -> Duration {
        Duration::from_micros(self.overhead_us.load(Ordering::Relaxed))
    }
    async fn with_retries(
        &self,
        route: &Route,
        request: &ChatRequest,
        model: &str,
        tx: &mpsc::Sender<Event>,
        output: &mut bool,
    ) -> Result<(), LlmError> {
        let mut attempt = 0;
        loop {
            let Err(failure) = route
                .attempt(request, model, tx, output, &self.overhead_us)
                .await
            else {
                return Ok(());
            };
            if !failure.retry || *output || attempt == RETRIES || tx.is_closed() {
                return Err(failure.error);
            }
            tokio::time::sleep(failure.after.unwrap_or_else(|| jitter(attempt))).await;
            attempt += 1;
        }
    }
}
#[async_trait]
impl ChatProvider for Gateway {
    async fn warm(&self) -> Result<(), LlmError> {
        let route = &self.routes[0];
        let (method, url) = super::check::probe_url(&route.url, &route.provider, None)
            .map_err(|_| LlmError::Provider("no connection check for this endpoint".into()))?;
        let builder = route
            .credential
            .apply(
                route.headers(route.client.request(method, url)),
                route.auth(),
            )
            .await
            .map_err(credential_error)?;
        let warmed = tokio::time::timeout(CONNECT, async {
            let response = builder
                .send()
                .await
                .map_err(|_| LlmError::Network("could not connect".to_owned()))?;
            if matches!(response.status().as_u16(), 401 | 403) {
                route.credential.invalidate().await;
                return Err(LlmError::Auth);
            }
            // Drain the bounded body so the pool can reuse the socket.
            read_bytes(response, false)
                .await
                .map(drop)
                .map_err(|_| LlmError::Network("the connection failed".into()))
        })
        .await;
        warmed.map_err(|_| LlmError::Timeout)?
    }
    async fn stream(
        &self,
        request: &ChatRequest,
        tx: &mpsc::Sender<Event>,
    ) -> Result<(), LlmError> {
        self.overhead_us.store(0, Ordering::Relaxed);
        let mut output = false;
        let mut first_error = None;
        for (index, route) in self.routes.iter().enumerate() {
            // A fallback serves its own model, not the primary's.
            let model = if index == 0 && !request.model.is_empty() {
                &request.model
            } else {
                &route.model
            };
            match self
                .with_retries(route, request, model, tx, &mut output)
                .await
            {
                Ok(()) => return Ok(()),
                // The caller shrinks and asks the same provider again.
                Err(error) if output || error == LlmError::ContextTooLong || tx.is_closed() => {
                    return Err(error)
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or_else(|| LlmError::Provider("no provider configured".into())))
    }
}
/// `config.provider` and its fallbacks. A fallback receives the same transcript
/// excerpts, so it needs saved consent of its own; offline, it is left out.
pub fn build_provider(config: &AiConfig, offline: bool) -> Result<Gateway, LlmError> {
    let route = Route::build(
        &config.provider,
        config.endpoint.as_deref(),
        config.model.as_deref(),
        offline,
    )?;
    gateway(config, route, offline)
}
fn gateway(config: &AiConfig, route: Route, offline: bool) -> Result<Gateway, LlmError> {
    let mut routes = vec![route];
    for id in &config.fallback {
        if *id == config.provider || !config.consented.contains(id) {
            continue;
        }
        match Route::build(id, None, None, offline) {
            Ok(route) => routes.push(route),
            Err(LlmError::Offline) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Gateway {
        routes,
        overhead_us: AtomicU64::new(0),
    })
}

/// Use explicit AI settings, or reuse cleanup's Chat Completions connection.
/// Returned config identifies the actual destination for consent/model selection;
/// this helper never grants transcript consent. Call it again rather than rebuilding
/// the returned config: OpenAI cleanup uses Chat Completions, explicit AI uses Responses.
pub fn build_provider_with_cleanup(
    config: &AiConfig,
    cleanup: &CleanupConfig,
    offline: bool,
) -> Result<(AiConfig, Gateway), LlmError> {
    if !config.provider.is_empty() {
        return build_provider(config, offline).map(|gateway| (config.clone(), gateway));
    }
    if cleanup.provider.is_none() && cleanup.endpoint.is_none() {
        return Err(LlmError::Provider("xflow ask is not set up: run xflow key set groq, then set ai.provider or cleanup.provider".into()));
    }
    if cleanup.provider.as_deref() == Some("gemini") {
        return Err(LlmError::Provider(
            "Gemini cleanup is not Chat Completions; configure ai.provider for ask".into(),
        ));
    }
    let mut cleanup = cleanup.clone();
    if config.model.is_some() {
        cleanup.model.clone_from(&config.model);
    }
    if config.endpoint.is_some() {
        cleanup.endpoint.clone_from(&config.endpoint);
    }
    let resolved = super::HttpTransformer::new(&cleanup, offline).map_err(|error| {
        if offline {
            LlmError::Offline
        } else {
            credential_error(error)
        }
    })?;
    if resolved.provider == "gemini" {
        return Err(LlmError::Provider(
            "Gemini cleanup is not Chat Completions; configure ai.provider for ask".into(),
        ));
    }
    let local = is_loopback(&resolved.endpoint);
    let canonical = super::cleanup_providers().iter().any(|info| {
        info.id == resolved.provider
            && Url::parse(info.endpoint).ok().is_some_and(|url| {
                url.host_str()
                    .is_some_and(|host| super::is_canonical_host(&resolved.endpoint, host))
            })
    });
    let mut credential = resolved.credential;
    // Never forward stored cleanup credentials to endpoint overrides, or any key to loopback.
    credential.allow_store = canonical && !local;
    if local {
        credential.env = None;
        credential.allow_anonymous = true;
    } else if !canonical && credential.env.is_none() {
        return Err(LlmError::Provider(
            "a custom remote cleanup endpoint needs an explicit cleanup.api_key_env".into(),
        ));
    }
    let id = if local {
        "custom"
    } else if canonical {
        &resolved.provider
    } else {
        "openai-compatible"
    };
    let route = Route {
        provider: id.into(),
        wire: Wire::ChatCompletions,
        url: chat_endpoint(resolved.endpoint.as_str(), offline)?,
        model: resolved.model,
        credential,
        client: client_with(TOTAL, CONNECT).map_err(credential_error)?,
    };
    let mut selected = config.clone();
    selected.provider = route.provider.clone();
    selected.model = Some(route.model.clone());
    selected.endpoint = Some(route.url.to_string());
    gateway(&selected, route, offline).map(|gateway| (selected, gateway))
}
fn chat_endpoint(value: &str, offline: bool) -> Result<Url, LlmError> {
    let mut url = endpoint(value, offline).map_err(|error| {
        if offline {
            LlmError::Offline
        } else {
            credential_error(error)
        }
    })?;
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/chat/completions") {
        path.to_owned()
    } else if path.is_empty() {
        "/v1/chat/completions".into()
    } else {
        format!("{path}/chat/completions")
    };
    url.set_path(&path);
    Ok(url)
}
