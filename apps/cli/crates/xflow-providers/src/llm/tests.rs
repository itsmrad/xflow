//! Loopback SSE fixtures with synthetic data; every byte boundary is deliberate.
use super::*;
use serde_json::json;
use std::{
    io::{Read as _, Write as _},
    sync::{
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
        Arc, Mutex,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
};

const KEY: &str = "sk-test-secret-123";

enum Step {
    Send(Vec<u8>),
    Wait(Duration),
    /// Close without the terminating chunk: a connection cut.
    Cut,
    /// Keep the connection open until the client goes away.
    Hold,
}
use Step::*;
struct Reply {
    status: &'static str,
    headers: &'static str,
    steps: Vec<Step>,
}
fn ok(steps: Vec<Step>) -> Reply {
    Reply {
        status: "200 OK",
        headers: "Content-Type: text/event-stream\r\n",
        steps,
    }
}
fn sse(text: &str) -> Step {
    Send(text.as_bytes().to_vec())
}
fn status(status: &'static str, headers: &'static str, body: &str) -> Reply {
    Reply {
        status,
        headers,
        steps: vec![sse(body)],
    }
}
struct Fixture {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    connections: Arc<AtomicUsize>,
    /// Fires when a request has been read in full.
    received: Arc<Notify>,
    /// Fires when a held connection is closed by the client.
    closed: Arc<Notify>,
}
impl Fixture {
    fn connections(&self) -> usize {
        self.connections.load(AtomicOrdering::SeqCst)
    }
    fn body(&self, index: usize) -> Value {
        let requests = self.requests.lock().unwrap();
        let raw = &requests[index];
        serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap()
    }
}
async fn serve(replies: Vec<Reply>) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture {
        url: format!("http://{}/v1/test", listener.local_addr().unwrap()),
        requests: Arc::default(),
        connections: Arc::default(),
        received: Arc::default(),
        closed: Arc::default(),
    };
    let (requests, connections, received, closed) = (
        fixture.requests.clone(),
        fixture.connections.clone(),
        fixture.received.clone(),
        fixture.closed.clone(),
    );
    tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            connections.fetch_add(1, AtomicOrdering::SeqCst);
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 8192];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let size: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .map_or(0, |n| n.parse().unwrap());
                    if bytes.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            requests
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&bytes).into_owned());
            received.notify_one();
            let head = format!(
                "HTTP/1.1 {}\r\n{}Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                reply.status, reply.headers
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let mut terminated = true;
            for step in reply.steps {
                match step {
                    Send(data) => {
                        let mut chunk = format!("{:x}\r\n", data.len()).into_bytes();
                        chunk.extend_from_slice(&data);
                        chunk.extend_from_slice(b"\r\n");
                        let _ = socket.write_all(&chunk).await;
                        let _ = socket.flush().await;
                    }
                    Wait(delay) => tokio::time::sleep(delay).await,
                    Cut => terminated = false,
                    Hold => {
                        let mut buf = [0; 64];
                        while socket.read(&mut buf).await.is_ok_and(|n| n > 0) {}
                        closed.notify_one();
                        terminated = false;
                    }
                }
            }
            if terminated {
                let _ = socket.write_all(b"0\r\n\r\n").await;
            }
        }
    });
    fixture
}
fn route(wire: Wire, provider: &str, url: &str) -> Route {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let name = format!(
        "XFLOW_LLM_TEST_KEY_{}",
        NEXT.fetch_add(1, AtomicOrdering::SeqCst)
    );
    std::env::set_var(&name, KEY);
    Route {
        provider: provider.into(),
        wire,
        url: Url::parse(url).unwrap(),
        model: "route-model".into(),
        credential: Credentials::new(provider, Some(name), true, false).unwrap(),
        client: client_with(TOTAL, CONNECT).unwrap(),
    }
}
fn gateway(routes: Vec<Route>) -> Gateway {
    Gateway {
        routes,
        overhead_us: AtomicU64::new(0),
    }
}
fn request() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        system: "sys".into(),
        messages: vec![Message::User("hello".into())],
        tools: vec![ToolDef {
            name: "search".into(),
            description: "d".into(),
            input_schema: json!({"type":"object"}),
        }],
        max_output_tokens: 50,
    }
}
async fn run(gateway: &Gateway, request: &ChatRequest) -> (Result<(), LlmError>, Vec<Event>) {
    let (tx, mut rx) = mpsc::channel(1024);
    let result = gateway.stream(request, &tx).await;
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    (result, events)
}
async fn once(wire: Wire, replies: Vec<Reply>) -> (Fixture, Result<(), LlmError>, Vec<Event>) {
    let fixture = serve(replies).await;
    let gateway = gateway(vec![route(wire, "openai", &fixture.url)]);
    let (result, events) = run(&gateway, &request()).await;
    (fixture, result, events)
}
fn text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}
fn tool(id: &str, name: &str, arguments: Value) -> Event {
    Event::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    })
}
fn chat_text(text: &str) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":{"content":text}}]})
    )
}
const CHAT_END: &str =
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
fn messages_frame(kind: &str, value: Value) -> String {
    format!("event: {kind}\ndata: {value}\n\n")
}
fn messages_text(text: &str) -> String {
    messages_frame(
        "message_start",
        json!({"message":{"usage":{"input_tokens":1,"output_tokens":1}}}),
    ) + &messages_frame(
        "content_block_start",
        json!({"index":0,"content_block":{"type":"text","text":""}}),
    ) + &messages_frame(
        "content_block_delta",
        json!({"index":0,"delta":{"type":"text_delta","text":text}}),
    )
}
fn responses_frame(value: Value) -> String {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().unwrap()
    )
}
fn responses_text(text: &str) -> String {
    responses_frame(json!({"type":"response.output_text.delta","output_index":0,"delta":text}))
}
fn responses_done() -> String {
    responses_frame(
        json!({"type":"response.completed","response":{"usage":{"input_tokens":5,"output_tokens":2,"input_tokens_details":{"cached_tokens":1}}}}),
    )
}

#[test]
fn sse_handles_splits_crlf_comments_and_a_missing_last_blank_line() {
    let input = ": keep-alive\r\nevent: a\r\ndata: 1\r\ndata:2\r\n\r\nid: 9\ndata: é\n\ndata: last";
    let mut parser = sse::Parser::default();
    let mut frames = Vec::new();
    for byte in input.as_bytes() {
        parser.push(std::slice::from_ref(byte), &mut frames);
    }
    parser.finish(&mut frames);
    let got: Vec<_> = frames
        .iter()
        .map(|f| (f.event.as_str(), f.data.as_str()))
        .collect();
    assert_eq!(got, [("a", "1\n2"), ("", "é"), ("", "last")]);
}
#[test]
fn only_identifier_codes_appear_in_errors() {
    let error = classify(
        Some(400),
        "bad code with spaces",
        "contains sk-abc and hello",
    );
    assert_eq!(
        error.to_string(),
        "the provider returned an error (HTTP 400)"
    );
    assert_eq!(
        classify(None, "invalid_prompt", "text").to_string(),
        "the provider returned an error (invalid_prompt)"
    );
    assert_eq!(
        classify(None, "429", "slow down"),
        LlmError::RateLimit { retry_after: None }
    );
    assert_eq!(
        classify(Some(429), "", "too many tokens per minute"),
        LlmError::RateLimit { retry_after: None }
    );
    assert_eq!(
        classify(Some(400), "", "prompt is too long"),
        LlmError::ContextTooLong
    );
    assert_eq!(
        classify(Some(400), "", "Your credit balance is too low"),
        LlmError::QuotaExhausted
    );
    assert_eq!(classify(Some(402), "", ""), LlmError::QuotaExhausted);
}
#[test]
fn jitter_stays_under_the_capped_backoff() {
    for attempt in 0..20 {
        let cap = RETRY_BASE
            .saturating_mul(1 << attempt.min(8))
            .min(RETRY_CAP);
        assert!(jitter(attempt) <= cap);
    }
}

// Chat Completions
#[tokio::test]
async fn chat_assembles_split_tool_deltas_usage_and_comments() {
    let stream = [
        ": OPENROUTER PROCESSING\n\n".to_owned(),
        // "é" split across two chunks below.
        chat_text("h\u{e9}llo"),
        "data: ".to_owned()
            + &json!({"choices":[{"delta":{"tool_calls":[
                {"index":1,"id":"call_b","type":"function","function":{"name":"second","arguments":""}},
                {"index":0,"id":"call_a","type":"function","function":{"name":"first","arguments":"{\"q\""}},
                // A duplicate same-index entry continues the same call.
                {"index":0,"function":{"arguments":":\"x"}}
            ]}}]})
                .to_string()
            + "\n\n",
        ": OPENROUTER PROCESSING\n\n".to_owned(),
        "data: ".to_owned()
            + &json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":"y\"}"}},
                {"index":1,"function":{"arguments":"{}"}},
                // A server repeating a finished entry must not corrupt it.
                {"index":1,"id":"call_b","function":{"name":"second","arguments":"{}"}}
            ]},"finish_reason":"tool_calls"}]})
                .to_string()
            + "\n\n",
        "data: ".to_owned()
            + &json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":4}}}).to_string()
            + "\n\n",
        "data: [DONE]\n\n".to_owned(),
    ]
    .concat()
    .into_bytes();
    let middle = stream.iter().position(|b| *b == 0xc3).unwrap() + 1;
    let tail = stream.len() - 9;
    let steps = vec![
        Send(stream[..middle].to_vec()),
        Send(stream[middle..tail].to_vec()),
        Send(stream[tail..].to_vec()),
    ];
    let fixture = serve(vec![ok(steps)]).await;
    let gateway = gateway(vec![route(
        Wire::ChatCompletions,
        "openrouter",
        &fixture.url,
    )]);
    let (result, events) = run(&gateway, &request()).await;
    result.unwrap();
    assert_eq!(
        events,
        [
            Event::Text("héllo".into()),
            tool("call_a", "first", json!({"q":"xy"})),
            tool("call_b", "second", json!({})),
            Event::Usage {
                input: 10,
                output: 3,
                cached: 4
            },
            Event::Done(Stop::ToolUse),
        ]
    );
    let body = fixture.body(0);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["provider"]["data_collection"], "deny");
    assert_eq!(
        body["messages"][0],
        json!({"role":"system","content":"sys"})
    );
    assert_eq!(body["tools"][0]["function"]["name"], "search");
    let raw = fixture.requests.lock().unwrap()[0].clone();
    assert!(raw
        .to_ascii_lowercase()
        .contains(&format!("authorization: bearer {KEY}").to_ascii_lowercase()));
}
#[tokio::test]
async fn only_openrouter_sends_the_data_collection_rule() {
    let fixture = serve(vec![ok(vec![sse(CHAT_END)])]).await;
    let gateway = gateway(vec![route(Wire::ChatCompletions, "custom", &fixture.url)]);
    run(&gateway, &request()).await.0.unwrap();
    assert!(fixture.body(0).get("provider").is_none());
}
#[tokio::test]
async fn chat_replays_tool_calls_and_results() {
    let fixture = serve(vec![ok(vec![sse(CHAT_END)])]).await;
    let gateway = gateway(vec![route(Wire::ChatCompletions, "custom", &fixture.url)]);
    let mut asked = request();
    asked.messages.push(Message::Assistant {
        text: "thinking".into(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "search".into(),
            arguments: json!({"q":1}),
        }],
        opaque: vec![],
    });
    asked.messages.push(Message::ToolResult {
        call_id: "c1".into(),
        content: "found".into(),
        is_error: false,
    });
    run(&gateway, &asked).await.0.unwrap();
    let body = fixture.body(0);
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["function"]["arguments"],
        "{\"q\":1}"
    );
    assert_eq!(
        body["messages"][3],
        json!({"role":"tool","tool_call_id":"c1","content":"found"})
    );
}
#[tokio::test]
async fn chat_body_may_end_without_done_once_finished() {
    let stream = chat_text("hi") + "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n";
    let (_, result, events) = once(Wire::ChatCompletions, vec![ok(vec![sse(&stream)])]).await;
    result.unwrap();
    assert_eq!(events, [Event::Text("hi".into()), Event::Done(Stop::End)]);
}
#[tokio::test]
async fn chat_error_inside_200_before_output_is_retried() {
    let error = "data: {\"error\":{\"code\":502,\"message\":\"upstream\"}}\n\n";
    let good = chat_text("hi") + CHAT_END;
    let (fixture, result, events) = once(
        Wire::ChatCompletions,
        vec![ok(vec![sse(error)]), ok(vec![sse(&good)])],
    )
    .await;
    result.unwrap();
    assert_eq!(text(&events), "hi");
    assert_eq!(fixture.connections(), 2);
}
#[tokio::test]
async fn chat_finish_reason_error_after_output_is_interrupted_without_retry() {
    let stream = chat_text("partial")
        + "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"error\",\"error\":{\"code\":500}}]}\n\n";
    let (fixture, result, events) = once(Wire::ChatCompletions, vec![ok(vec![sse(&stream)])]).await;
    assert_eq!(result, Err(LlmError::Interrupted { partial: true }));
    assert_eq!(text(&events), "partial");
    assert_eq!(fixture.connections(), 1);
}
#[tokio::test]
async fn context_length_inside_200_is_final() {
    let error = "data: {\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"long\"}}\n\n";
    let (fixture, result, _) = once(Wire::ChatCompletions, vec![ok(vec![sse(error)])]).await;
    assert_eq!(result, Err(LlmError::ContextTooLong));
    assert_eq!(fixture.connections(), 1);
}
#[tokio::test]
async fn malformed_json_is_a_final_error_not_a_panic() {
    let (fixture, result, events) = once(
        Wire::ChatCompletions,
        vec![ok(vec![sse("data: {nope\n\n")])],
    )
    .await;
    assert_eq!(result, Err(malformed()));
    assert!(events.is_empty());
    assert_eq!(fixture.connections(), 1);
    // Malformed tool arguments at a normal finish.
    let stream = "data: ".to_owned()
        + &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"x","arguments":"{oops"}}]},"finish_reason":"tool_calls"}]}).to_string()
        + "\n\n";
    let (fixture, result, _) = once(Wire::ChatCompletions, vec![ok(vec![sse(&stream)])]).await;
    assert_eq!(result, Err(malformed()));
    assert_eq!(fixture.connections(), 1);
}

// Status codes
#[tokio::test]
async fn unauthorized_is_final() {
    let (fixture, result, _) = once(
        Wire::ChatCompletions,
        vec![status(
            "401 Unauthorized",
            "",
            "{\"error\":{\"message\":\"Incorrect API key provided: sk-test\"}}",
        )],
    )
    .await;
    assert_eq!(result, Err(LlmError::Auth));
    assert_eq!(fixture.connections(), 1);
}
#[tokio::test]
async fn retry_after_is_honoured_and_the_request_resent() {
    let good = chat_text("hi") + CHAT_END;
    let started = Instant::now();
    let (fixture, result, events) = once(
        Wire::ChatCompletions,
        vec![
            status("429 Too Many Requests", "Retry-After: 1\r\n", "{}"),
            ok(vec![sse(&good)]),
        ],
    )
    .await;
    result.unwrap();
    assert_eq!(text(&events), "hi");
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(fixture.connections(), 2);
    assert_eq!(fixture.body(0), fixture.body(1));
}
#[tokio::test]
async fn retry_after_over_the_limit_is_final() {
    let (fixture, result, _) = once(
        Wire::ChatCompletions,
        vec![status("429 Too Many Requests", "Retry-After: 60\r\n", "{}")],
    )
    .await;
    assert_eq!(
        result,
        Err(LlmError::RateLimit {
            retry_after: Some(Duration::from_secs(60))
        })
    );
    assert_eq!(fixture.connections(), 1);
}
#[tokio::test]
async fn rate_limit_without_retry_after_retries_twice() {
    let reply = || {
        status(
            "429 Too Many Requests",
            "",
            "{\"error\":{\"message\":\"slow down\"}}",
        )
    };
    let (fixture, result, _) = once(Wire::ChatCompletions, vec![reply(), reply(), reply()]).await;
    assert_eq!(result, Err(LlmError::RateLimit { retry_after: None }));
    assert_eq!(fixture.connections(), 3);
}
#[tokio::test]
async fn spend_cap_quota_or_credit_without_retry_after_is_final() {
    for message in [
        "You exceeded your current quota",
        "workspace spend limit reached",
        "out of credit",
    ] {
        let body = json!({"error":{"message":message}}).to_string();
        let (fixture, result, _) = once(
            Wire::ChatCompletions,
            vec![status("429 Too Many Requests", "", &body)],
        )
        .await;
        assert_eq!(result, Err(LlmError::QuotaExhausted), "{message}");
        assert_eq!(fixture.connections(), 1);
    }
}
#[tokio::test]
async fn http_529_and_overloaded_event_are_overloaded_and_retried() {
    let overloaded =
        "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}";
    let reply = || status("529 Overloaded", "", overloaded);
    let (fixture, result, _) = once(Wire::Messages, vec![reply(), reply(), reply()]).await;
    assert_eq!(result, Err(LlmError::Overloaded));
    assert_eq!(fixture.connections(), 3);
    let event = format!("event: error\ndata: {overloaded}\n\n");
    let good = messages_text("hi") + &messages_frame("message_stop", json!({}));
    let (fixture, result, events) = once(
        Wire::Messages,
        vec![ok(vec![sse(&event)]), ok(vec![sse(&good)])],
    )
    .await;
    result.unwrap();
    assert_eq!(text(&events), "hi");
    assert_eq!(fixture.connections(), 2);
}
#[tokio::test]
async fn provider_errors_do_not_echo_their_message() {
    let body = format!("{{\"error\":{{\"message\":\"bad input: my private dictation {KEY}\",\"code\":\"invalid_prompt\"}}}}");
    let (_, result, _) = once(
        Wire::ChatCompletions,
        vec![status("400 Bad Request", "", &body)],
    )
    .await;
    let text = result.unwrap_err().to_string();
    assert_eq!(
        text,
        "the provider returned an error (HTTP 400, invalid_prompt)"
    );
}
#[tokio::test]
async fn connect_failure_is_retried_and_reported_without_the_url() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/x", listener.local_addr().unwrap());
    drop(listener);
    let gateway = gateway(vec![route(Wire::ChatCompletions, "custom", &url)]);
    let (result, _) = run(&gateway, &request()).await;
    // A loopback server that refuses is a local server that is not running.
    assert_eq!(
        result,
        Err(LlmError::Network("the local server is not running".into()))
    );
}

// Messages
#[tokio::test]
async fn messages_parses_tools_at_block_stop_and_uses_cumulative_usage() {
    let stream = [
        messages_frame("message_start", json!({"message":{"usage":{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":1}}})),
        messages_frame("ping", json!({})),
        messages_frame("content_block_start", json!({"index":0,"content_block":{"type":"text","text":""}})),
        messages_frame("content_block_delta", json!({"index":0,"delta":{"type":"text_delta","text":"Hi"}})),
        messages_frame("content_block_stop", json!({"index":0})),
        messages_frame("content_block_start", json!({"index":1,"content_block":{"type":"tool_use","id":"t1","name":"search","input":{}}})),
        // The first `partial_json` may be "".
        messages_frame("content_block_delta", json!({"index":1,"delta":{"type":"input_json_delta","partial_json":""}})),
        messages_frame("content_block_delta", json!({"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}})),
        messages_frame("content_block_delta", json!({"index":1,"delta":{"type":"input_json_delta","partial_json":"1}"}})),
        messages_frame("content_block_stop", json!({"index":1})),
        messages_frame("message_delta", json!({"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}})),
        messages_frame("message_delta", json!({"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}})),
        messages_frame("message_stop", json!({})),
    ]
    .concat();
    let (fixture, result, events) = once(Wire::Messages, vec![ok(vec![sse(&stream)])]).await;
    result.unwrap();
    assert_eq!(
        events,
        [
            Event::Text("Hi".into()),
            tool("t1", "search", json!({"q":1})),
            Event::Usage {
                input: 15,
                output: 7,
                cached: 5
            },
            Event::Done(Stop::ToolUse),
        ]
    );
    let raw = fixture.requests.lock().unwrap()[0].to_ascii_lowercase();
    assert!(raw.contains(&format!("x-api-key: {}", KEY.to_ascii_lowercase())));
    assert!(raw.contains("anthropic-version: 2023-06-01"));
    assert!(!raw.contains("authorization:"));
}
#[tokio::test]
async fn messages_max_tokens_mid_json_is_a_clean_length_stop() {
    let stream = [
        messages_frame(
            "content_block_start",
            json!({"index":0,"content_block":{"type":"tool_use","id":"t1","name":"search"}}),
        ),
        messages_frame(
            "content_block_delta",
            json!({"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":\"tru"}}),
        ),
        messages_frame("content_block_stop", json!({"index":0})),
        messages_frame(
            "message_delta",
            json!({"delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":50}}),
        ),
        messages_frame("message_stop", json!({})),
    ]
    .concat();
    let (_, result, events) = once(Wire::Messages, vec![ok(vec![sse(&stream)])]).await;
    result.unwrap();
    assert_eq!(*events.last().unwrap(), Event::Done(Stop::Length));
    assert!(!events.iter().any(|e| matches!(e, Event::ToolCall(_))));
}
#[tokio::test]
async fn messages_bad_json_without_a_length_cut_is_malformed() {
    let stream = [
        messages_frame(
            "content_block_start",
            json!({"index":0,"content_block":{"type":"tool_use","id":"t1","name":"search"}}),
        ),
        messages_frame(
            "content_block_delta",
            json!({"index":0,"delta":{"type":"input_json_delta","partial_json":"{oops"}}),
        ),
        messages_frame("content_block_stop", json!({"index":0})),
        messages_frame("message_delta", json!({"delta":{"stop_reason":"tool_use"}})),
        messages_frame("message_stop", json!({})),
    ]
    .concat();
    let (_, result, _) = once(Wire::Messages, vec![ok(vec![sse(&stream)])]).await;
    assert_eq!(result, Err(malformed()));
}
#[test]
fn messages_body_groups_results_and_flattens_tools_on_the_forced_answer() {
    let mut asked = request();
    asked.messages.push(Message::Assistant {
        text: String::new(),
        tool_calls: vec![
            ToolCall {
                id: "a".into(),
                name: "search".into(),
                arguments: json!({"q":1}),
            },
            ToolCall {
                id: "b".into(),
                name: "search".into(),
                arguments: json!({"q":2}),
            },
        ],
        opaque: vec![],
    });
    asked.messages.push(Message::ToolResult {
        call_id: "a".into(),
        content: "one".into(),
        is_error: false,
    });
    asked.messages.push(Message::ToolResult {
        call_id: "b".into(),
        content: "two".into(),
        is_error: true,
    });
    let body = messages::body(&asked, "m");
    let list = body["messages"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[1]["role"], "assistant");
    assert_eq!(list[1]["content"].as_array().unwrap().len(), 2);
    assert_eq!(list[2]["role"], "user");
    assert_eq!(
        list[2]["content"][1],
        json!({"type":"tool_result","tool_use_id":"b","content":"two","is_error":true})
    );
    assert_eq!(body["system"], "sys");
    // A request without tools must carry no tool blocks.
    asked.tools.clear();
    let body = messages::body(&asked, "m").to_string();
    assert!(
        !body.contains("tool_use") && !body.contains("tool_result"),
        "{body}"
    );
    assert!(
        body.contains("[called search") && body.contains("[tool result] two"),
        "{body}"
    );
}

// Responses
#[tokio::test]
async fn responses_assembles_calls_by_output_index() {
    let stream = [
        responses_frame(json!({"type":"response.created","response":{}})),
        responses_text("Hi"),
        responses_frame(json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","call_id":"c2","name":"second","arguments":""}})),
        responses_frame(json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"c1","name":"first","arguments":""}})),
        responses_frame(json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"q\""})),
        responses_frame(json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{}"})),
        responses_frame(json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":":1}"})),
        responses_frame(json!({"type":"response.function_call_arguments.done","output_index":1,"arguments":"{\"q\":1}"})),
        responses_frame(json!({"type":"response.function_call_arguments.done","output_index":2,"arguments":"{}"})),
        // Repeats the finished call; it must not be sent twice.
        responses_frame(json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"c1","name":"first","arguments":"{\"q\":1}"}})),
        responses_done(),
    ]
    .concat();
    let (fixture, result, events) = once(Wire::Responses, vec![ok(vec![sse(&stream)])]).await;
    result.unwrap();
    assert_eq!(
        events,
        [
            Event::Text("Hi".into()),
            tool("c1", "first", json!({"q":1})),
            tool("c2", "second", json!({})),
            Event::Usage {
                input: 5,
                output: 2,
                cached: 1
            },
            Event::Done(Stop::ToolUse),
        ]
    );
    let body = fixture.body(0);
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["instructions"], "sys");
    assert_eq!(body["tools"][0]["strict"], false);
    assert_eq!(body["max_output_tokens"], 50);
}
#[tokio::test]
async fn responses_replays_reasoning_verbatim() {
    let item = json!({"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"AAAA"});
    let stream =
        responses_frame(json!({"type":"response.output_item.done","output_index":0,"item":item}))
            + &responses_text("Hi")
            + &responses_done();
    let fixture = serve(vec![
        ok(vec![sse(&stream)]),
        ok(vec![sse(&responses_done())]),
    ])
    .await;
    let gateway = gateway(vec![route(Wire::Responses, "openai", &fixture.url)]);
    let (result, events) = run(&gateway, &request()).await;
    result.unwrap();
    assert_eq!(events[0], Event::Opaque(item.clone()));
    let mut next = request();
    next.messages.push(Message::Assistant {
        text: "Hi".into(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "search".into(),
            arguments: json!({}),
        }],
        opaque: vec![item.clone()],
    });
    next.messages.push(Message::ToolResult {
        call_id: "c1".into(),
        content: "r".into(),
        is_error: false,
    });
    run(&gateway, &next).await.0.unwrap();
    let input = fixture.body(1)["input"].clone();
    assert_eq!(
        input[1], item,
        "the reasoning item leads its turn, untouched"
    );
    assert_eq!(input[2], json!({"role":"assistant","content":"Hi"}));
    assert_eq!(input[3]["type"], "function_call");
    assert_eq!(
        input[4],
        json!({"type":"function_call_output","call_id":"c1","output":"r"})
    );
    assert_eq!(fixture.body(1)["include"][0], "reasoning.encrypted_content");
}
#[tokio::test]
async fn responses_incomplete_mid_json_is_a_clean_length_stop() {
    let stream = [
        responses_text("Hi"),
        responses_frame(json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"c1","name":"first"}})),
        responses_frame(json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"q\":\"tru"})),
        responses_frame(json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":1,"output_tokens":50}}})),
    ]
    .concat();
    let (_, result, events) = once(Wire::Responses, vec![ok(vec![sse(&stream)])]).await;
    result.unwrap();
    assert_eq!(*events.last().unwrap(), Event::Done(Stop::Length));
    assert!(!events.iter().any(|e| matches!(e, Event::ToolCall(_))));
}
#[tokio::test]
async fn chat_length_cut_mid_json_is_a_clean_length_stop() {
    let stream = "data: ".to_owned()
        + &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"x","arguments":"{\"q\":\"tru"}}]},"finish_reason":"length"}]}).to_string()
        + "\n\ndata: [DONE]\n\n";
    let (_, result, events) = once(Wire::ChatCompletions, vec![ok(vec![sse(&stream)])]).await;
    result.unwrap();
    assert_eq!(events, [Event::Done(Stop::Length)]);
}
#[tokio::test]
async fn responses_failure_events_classify_and_retry_leaves_nothing_behind() {
    let item = json!({"type":"reasoning","id":"rs_1"});
    let failed = responses_frame(
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
    ) + &responses_frame(
        json!({"type":"response.failed","response":{"error":{"code":"server_error","message":"x"}}}),
    );
    let good =
        responses_frame(json!({"type":"response.output_item.done","output_index":0,"item":item}))
            + &responses_text("Hi")
            + &responses_done();
    let (fixture, result, events) = once(
        Wire::Responses,
        vec![ok(vec![sse(&failed)]), ok(vec![sse(&good)])],
    )
    .await;
    result.unwrap();
    assert_eq!(fixture.connections(), 2);
    let opaque = events
        .iter()
        .filter(|e| matches!(e, Event::Opaque(_)))
        .count();
    assert_eq!(
        opaque, 1,
        "the failed attempt's reasoning must not be replayed"
    );
    let usage = events
        .iter()
        .filter(|e| matches!(e, Event::Usage { .. }))
        .count();
    assert_eq!(usage, 1);
    // The bare `error` event, with a final classification.
    let error = responses_frame(json!({"type":"error","code":"insufficient_quota","message":"no"}));
    let (fixture, result, _) = once(Wire::Responses, vec![ok(vec![sse(&error)])]).await;
    assert_eq!(result, Err(LlmError::QuotaExhausted));
    assert_eq!(fixture.connections(), 1);
}

// Interruptions, on every wire
fn partial_for(wire: Wire) -> String {
    match wire {
        Wire::ChatCompletions => chat_text("kept"),
        Wire::Responses => responses_text("kept"),
        Wire::Messages => messages_text("kept"),
    }
}
const WIRES: [Wire; 3] = [Wire::ChatCompletions, Wire::Responses, Wire::Messages];
#[tokio::test]
async fn connection_cut_mid_stream_keeps_text_and_never_retries() {
    for wire in WIRES {
        let (fixture, result, events) =
            once(wire, vec![ok(vec![sse(&partial_for(wire)), Cut])]).await;
        assert_eq!(
            result,
            Err(LlmError::Interrupted { partial: true }),
            "{wire:?}"
        );
        assert_eq!(text(&events), "kept", "{wire:?}");
        assert_eq!(fixture.connections(), 1, "{wire:?}");
        assert!(!events.iter().any(|e| matches!(e, Event::Done(_))));
    }
}
#[tokio::test]
async fn clean_eof_without_a_terminal_event_is_interrupted_too() {
    for wire in WIRES {
        let (fixture, result, events) = once(wire, vec![ok(vec![sse(&partial_for(wire))])]).await;
        assert_eq!(
            result,
            Err(LlmError::Interrupted { partial: true }),
            "{wire:?}"
        );
        assert_eq!(text(&events), "kept");
        assert_eq!(fixture.connections(), 1);
    }
    let (_, result, events) = once(Wire::Responses, vec![ok(vec![Cut])]).await;
    assert_eq!(result, Err(LlmError::Interrupted { partial: false }));
    assert!(events.is_empty());
}
#[tokio::test]
async fn cancel_mid_stream_aborts_the_connection() {
    for wire in WIRES {
        let fixture = serve(vec![ok(vec![sse(&partial_for(wire)), Hold])]).await;
        let gateway = gateway(vec![route(wire, "openai", &fixture.url)]);
        let asked = request();
        let (tx, mut rx) = mpsc::channel(8);
        {
            let stream = gateway.stream(&asked, &tx);
            tokio::pin!(stream);
            tokio::select! {
                _ = &mut stream => panic!("the stream should still be open"),
                event = rx.recv() => assert_eq!(event, Some(Event::Text("kept".into()))),
            }
            // Leaving this scope drops the future: that is the cancel.
        }
        let closed = fixture.closed.clone();
        tokio::time::timeout(Duration::from_secs(2), closed.notified())
            .await
            .expect("dropping the future must close the socket");
    }
}
#[tokio::test]
async fn a_dropped_receiver_stops_the_stream() {
    let fixture = serve(vec![ok(vec![
        sse(&chat_text("a")),
        sse(&chat_text("b")),
        Hold,
    ])])
    .await;
    let gateway = gateway(vec![route(Wire::ChatCompletions, "custom", &fixture.url)]);
    let (tx, rx) = mpsc::channel(8);
    drop(rx);
    let result = gateway.stream(&request(), &tx).await;
    assert_eq!(result, Err(LlmError::Interrupted { partial: true }));
    assert_eq!(fixture.connections(), 1);
}
#[tokio::test]
async fn stall_after_output_hits_the_idle_timeout_and_comments_reset_it() {
    let keep_alive = ": OPENROUTER PROCESSING\n\n";
    let fixture = serve(vec![ok(vec![
        sse(&chat_text("kept")),
        Wait(Duration::from_secs(40)),
        sse(keep_alive),
        Hold,
    ])])
    .await;
    let gateway = gateway(vec![route(Wire::ChatCompletions, "custom", &fixture.url)]);
    let asked = request();
    let (tx, mut rx) = mpsc::channel(8);
    let stream = gateway.stream(&asked, &tx);
    tokio::pin!(stream);
    tokio::select! {
        _ = &mut stream => panic!("the stream should still be open"),
        event = rx.recv() => assert_eq!(event, Some(Event::Text("kept".into()))),
    }
    // Everything up to here used real sockets and time; only the stall is simulated.
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    assert_eq!(stream.await, Err(LlmError::Interrupted { partial: true }));
    let waited = start.elapsed();
    // 40 s to the comment, then a full idle period; without the reset it would be 45.
    assert!(waited >= Duration::from_secs(85), "{waited:?}");
    assert!(waited < Duration::from_secs(90), "{waited:?}");
    assert_eq!(fixture.connections(), 1);
}
#[tokio::test]
async fn silence_before_the_first_byte_times_out_without_retry() {
    let fixture = serve(vec![Reply {
        status: "200 OK",
        headers: "Content-Type: text/event-stream\r\n",
        steps: vec![Hold],
    }])
    .await;
    let gateway = gateway(vec![route(Wire::ChatCompletions, "custom", &fixture.url)]);
    let asked = request();
    let (tx, _rx) = mpsc::channel(8);
    let stream = gateway.stream(&asked, &tx);
    tokio::pin!(stream);
    let received = fixture.received.clone();
    tokio::select! {
        _ = &mut stream => panic!("the stream should still be open"),
        _ = received.notified() => {}
    }
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    assert_eq!(stream.await, Err(LlmError::Timeout));
    let waited = start.elapsed();
    assert!(waited > FIRST_BYTE - Duration::from_secs(5), "{waited:?}");
    assert!(waited < FIRST_BYTE + Duration::from_secs(1), "{waited:?}");
    assert_eq!(fixture.connections(), 1);
}

// Fallback
#[tokio::test]
async fn fallback_serves_its_own_model_only_before_output() {
    let busy = || status("503 Service Unavailable", "", "{}");
    let primary = serve(vec![busy(), busy(), busy()]).await;
    let good = responses_text("from fallback") + &responses_done();
    let backup = serve(vec![ok(vec![sse(&good)])]).await;
    let both = gateway(vec![
        route(Wire::ChatCompletions, "openrouter", &primary.url),
        route(Wire::Responses, "openai", &backup.url),
    ]);
    let (result, events) = run(&both, &request()).await;
    result.unwrap();
    assert_eq!(text(&events), "from fallback");
    assert_eq!(primary.connections(), 3);
    assert_eq!(primary.body(0)["model"], "m");
    assert_eq!(backup.body(0)["model"], "route-model");

    let primary = serve(vec![ok(vec![sse(&chat_text("kept")), Cut])]).await;
    let backup = serve(vec![]).await;
    let both = gateway(vec![
        route(Wire::ChatCompletions, "custom", &primary.url),
        route(Wire::Responses, "openai", &backup.url),
    ]);
    let (result, events) = run(&both, &request()).await;
    assert_eq!(result, Err(LlmError::Interrupted { partial: true }));
    assert_eq!(text(&events), "kept");
    assert_eq!(backup.connections(), 0);
}
#[tokio::test]
async fn context_too_long_does_not_go_to_the_fallback() {
    let primary = serve(vec![status(
        "400 Bad Request",
        "",
        "{\"error\":{\"message\":\"prompt is too long\"}}",
    )])
    .await;
    let backup = serve(vec![]).await;
    let both = gateway(vec![
        route(Wire::ChatCompletions, "custom", &primary.url),
        route(Wire::Responses, "openai", &backup.url),
    ]);
    let (result, _) = run(&both, &request()).await;
    assert_eq!(result, Err(LlmError::ContextTooLong));
    assert_eq!(backup.connections(), 0);
}
#[tokio::test]
async fn a_failed_fallback_reports_the_primary_error() {
    let primary = serve(vec![status("401 Unauthorized", "", "{}")]).await;
    let backup = serve(vec![status("400 Bad Request", "", "{}")]).await;
    let both = gateway(vec![
        route(Wire::ChatCompletions, "custom", &primary.url),
        route(Wire::Responses, "openai", &backup.url),
    ]);
    assert_eq!(run(&both, &request()).await.0, Err(LlmError::Auth));
}

// Building
fn ai(provider: &str) -> AiConfig {
    AiConfig {
        provider: provider.into(),
        ..AiConfig::default()
    }
}
fn route_ids(result: Result<Gateway, LlmError>) -> Vec<String> {
    result
        .ok()
        .unwrap()
        .routes
        .iter()
        .map(|r| r.provider.clone())
        .collect()
}
#[test]
fn every_catalog_provider_builds_with_its_wire() {
    for (id, wire) in [
        ("openai", Wire::Responses),
        ("anthropic", Wire::Messages),
        ("openrouter", Wire::ChatCompletions),
    ] {
        let gateway = build_provider(&ai(id), false).ok().unwrap();
        assert_eq!(gateway.routes[0].wire, wire);
        assert!(gateway.routes[0].url.scheme() == "https");
        let info = find_provider(ProviderKind::Chat, id).unwrap();
        assert_eq!(gateway.routes[0].model, info.default_model);
    }
    assert_eq!(
        find_provider(ProviderKind::Chat, "openai")
            .unwrap()
            .default_model,
        "gpt-6-luna"
    );
    assert_eq!(
        find_provider(ProviderKind::Chat, "anthropic")
            .unwrap()
            .default_model,
        "claude-haiku-5-5"
    );
    assert_eq!(
        find_provider(ProviderKind::Chat, "openrouter")
            .unwrap()
            .default_model,
        "openai/gpt-oss-120b"
    );
}
#[test]
fn offline_refuses_every_remote_provider_and_leaves_remote_fallbacks_out() {
    for id in ["openai", "anthropic", "openrouter"] {
        assert_eq!(build_provider(&ai(id), true).err(), Some(LlmError::Offline));
    }
    let local = AiConfig {
        endpoint: Some("http://127.0.0.1:9/v1/chat/completions".into()),
        model: Some("m".into()),
        fallback: vec!["openai".into()],
        consented: vec!["openai".into()],
        ..ai("custom")
    };
    assert_eq!(route_ids(build_provider(&local, true)), ["custom"]);
    assert_eq!(
        route_ids(build_provider(&local, false)),
        ["custom", "openai"]
    );
}
#[test]
fn a_fallback_without_saved_consent_gets_nothing() {
    let config = AiConfig {
        fallback: vec!["anthropic".into(), "openrouter".into(), "openai".into()],
        consented: vec!["openrouter".into()],
        ..ai("openai")
    };
    assert_eq!(
        route_ids(build_provider(&config, false)),
        ["openai", "openrouter"]
    );
}
#[test]
fn custom_is_loopback_only_and_stored_keys_stay_home() {
    let custom = |endpoint: &str| AiConfig {
        endpoint: Some(endpoint.into()),
        model: Some("m".into()),
        ..ai("custom")
    };
    assert!(build_provider(&custom("http://127.0.0.1:8080/v1/chat/completions"), false).is_ok());
    for bad in [
        "https://example.com/v1/chat/completions",
        "http://example.com/v1/chat/completions",
        "http://localhost:8080/v1/chat/completions",
    ] {
        assert!(build_provider(&custom(bad), false).is_err(), "{bad}");
    }
    assert!(build_provider(&ai("custom"), false).is_err());
    let no_model = AiConfig {
        endpoint: Some("http://127.0.0.1:1/x".into()),
        ..ai("custom")
    };
    assert!(build_provider(&no_model, false).is_err());
    let misrouted = AiConfig {
        endpoint: Some("http://127.0.0.1:1/x".into()),
        ..ai("openai")
    };
    assert!(build_provider(&misrouted, false).is_err());
    assert!(build_provider(&ai("nonsense"), false).is_err());
}
#[tokio::test]
async fn a_custom_server_gets_no_authorization_header() {
    let fixture = serve(vec![ok(vec![sse(CHAT_END)])]).await;
    let config = AiConfig {
        endpoint: Some(fixture.url.clone()),
        model: Some("m".into()),
        ..ai("custom")
    };
    let gateway = build_provider(&config, false).ok().unwrap();
    run(&gateway, &request()).await.0.unwrap();
    assert!(!fixture.requests.lock().unwrap()[0]
        .to_ascii_lowercase()
        .contains("authorization"));
}
#[tokio::test]
async fn a_missing_key_is_not_logged_in() {
    let fixture = serve(vec![]).await;
    let mut only = route(Wire::ChatCompletions, "openrouter", &fixture.url);
    only.credential = Credentials::new(
        "openrouter",
        Some("XFLOW_LLM_TEST_UNSET".into()),
        false,
        false,
    )
    .unwrap();
    let (result, _) = run(&gateway(vec![only]), &request()).await;
    assert_eq!(result, Err(LlmError::NotLoggedIn));
    assert_eq!(fixture.connections(), 0);
}
#[tokio::test]
async fn warm_opens_the_connection_with_credentials() {
    let fixture = serve(vec![status("200 OK", "", "{}")]).await;
    let gateway = gateway(vec![route(Wire::Messages, "anthropic", &fixture.url)]);
    gateway.warm().await.unwrap();
    let raw = fixture.requests.lock().unwrap()[0].clone();
    assert!(raw.starts_with("GET /v1/models"), "{raw}");
    assert!(raw.to_ascii_lowercase().contains("x-api-key"));

    let fixture = serve(vec![status("401 Unauthorized", "", "{}")]).await;
    let gateway = self::gateway(vec![route(Wire::ChatCompletions, "custom", &fixture.url)]);
    assert_eq!(gateway.warm().await, Err(LlmError::Auth));
}

/// `cargo test --release -p xflow-providers --lib gateway_overhead -- --ignored --nocapture`
/// First-token delay above the server's own delay, on a single-thread runtime like
/// the CLI's, with the server on its own thread and a fresh connection per run.
#[test]
#[ignore = "measurement, not a check"]
fn gateway_overhead() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/test", listener.local_addr().unwrap());
    const RUNS: usize = 200;
    let (delays_tx, delays_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for socket in listener.incoming().take(RUNS) {
            let mut socket = socket.unwrap();
            socket.set_nodelay(true).unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 8192];
            loop {
                let n = socket.read(&mut buf).unwrap();
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let size: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            let received = Instant::now();
            std::thread::sleep(Duration::from_millis(10));
            let event = chat_text("first");
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{event}\r\n",
                event.len()
            );
            delays_tx.send(received.elapsed()).unwrap();
            socket.write_all(reply.as_bytes()).unwrap();
            let end = format!("{:x}\r\n{CHAT_END}\r\n0\r\n\r\n", CHAT_END.len());
            let _ = socket.write_all(end.as_bytes());
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut overheads: Vec<Duration> = runtime.block_on(async {
        let gateway = gateway(vec![route(Wire::ChatCompletions, "custom", &url)]);
        let asked = request();
        let mut overheads = Vec::with_capacity(RUNS);
        for _ in 0..RUNS {
            let (tx, mut rx) = mpsc::channel(8);
            let started = Instant::now();
            let stream = async {
                let result = gateway.stream(&asked, &tx).await;
                drop(tx);
                result
            };
            let consume = async {
                let event = rx.recv().await;
                assert!(matches!(event, Some(Event::Text(_))));
                let first = started.elapsed();
                while rx.recv().await.is_some() {}
                first
            };
            let (result, first) = tokio::join!(stream, consume);
            result.unwrap();
            overheads.push(first.saturating_sub(delays_rx.recv().unwrap()));
        }
        overheads
    });
    overheads.sort();
    println!(
        "gateway overhead over {RUNS} runs: p50 {:?}, p95 {:?}, max {:?}",
        overheads[RUNS / 2],
        overheads[RUNS * 95 / 100],
        overheads[RUNS - 1]
    );
}

#[test]
fn quick_default_reuses_groq_route_and_key_identity_without_consent() {
    let cleanup = CleanupConfig {
        provider: Some("groq".into()),
        model: Some("chosen".into()),
        ..Default::default()
    };
    let (selected, gateway) =
        build_provider_with_cleanup(&AiConfig::default(), &cleanup, false).unwrap();
    let route = &gateway.routes[0];
    assert_eq!(selected.provider, "groq");
    assert!(selected.consented.is_empty());
    assert_eq!(
        route.url.as_str(),
        "https://api.groq.com/openai/v1/chat/completions"
    );
    assert_eq!(route.model, "chosen");
    assert_eq!(route.wire, Wire::ChatCompletions);
    assert_eq!(route.credential.provider, "groq");
    assert_eq!(route.credential.env.as_deref(), Some("GROQ_API_KEY"));
    assert!(route.credential.allow_store);
    let direct = build_provider(&ai("groq"), false).unwrap();
    assert_eq!(
        direct.routes[0].credential.provider,
        route.credential.provider
    );
    assert_eq!(direct.routes[0].wire, route.wire);
    let config = AiConfig {
        model: Some("ai-override".into()),
        ..Default::default()
    };
    let (selected, _) = build_provider_with_cleanup(&config, &cleanup, false).unwrap();
    assert_eq!(selected.model.as_deref(), Some("ai-override"));
}
#[test]
fn quick_default_explicit_ai_wins_and_non_chat_cleanup_is_rejected() {
    let cleanup = CleanupConfig {
        provider: Some("gemini".into()),
        ..Default::default()
    };
    let (selected, gateway) = build_provider_with_cleanup(&ai("openai"), &cleanup, false).unwrap();
    assert_eq!(selected.provider, "openai");
    assert_eq!(gateway.routes[0].wire, Wire::Responses);
    let error = build_provider_with_cleanup(&AiConfig::default(), &cleanup, false)
        .err()
        .unwrap();
    assert!(error.to_string().contains("not Chat Completions"));
    assert!(
        build_provider_with_cleanup(&AiConfig::default(), &CleanupConfig::default(), false)
            .is_err()
    );
    let cleanup = CleanupConfig {
        provider: Some("openai".into()),
        ..Default::default()
    };
    let (_, gateway) = build_provider_with_cleanup(&AiConfig::default(), &cleanup, false).unwrap();
    assert_eq!(gateway.routes[0].wire, Wire::ChatCompletions);
}
#[test]
fn quick_default_compatible_base_and_full_urls_preserve_security() {
    for (input, path) in [
        ("https://example.invalid", "/v1/chat/completions"),
        (
            "https://example.invalid/api/v1/",
            "/api/v1/chat/completions",
        ),
        (
            "https://example.invalid/api/v1/chat/completions",
            "/api/v1/chat/completions",
        ),
    ] {
        let config = AiConfig {
            endpoint: Some(input.into()),
            model: Some("m".into()),
            ..ai("openai-compatible")
        };
        let gateway = build_provider(&config, false).unwrap();
        let route = &gateway.routes[0];
        assert_eq!(route.url.path(), path);
        assert_eq!(route.credential.env.as_deref(), Some("XFLOW_AI_API_KEY"));
        assert!(!route.credential.allow_store);
        assert!(!route.credential.allow_anonymous);
        assert_eq!(build_provider(&config, true).err(), Some(LlmError::Offline));
    }
    for url in [
        "http://example.invalid",
        "https://user:secret@example.invalid",
        "https://example.invalid?key=secret",
        "https://example.invalid/#secret",
    ] {
        let config = AiConfig {
            endpoint: Some(url.into()),
            model: Some("m".into()),
            ..ai("openai-compatible")
        };
        assert!(build_provider(&config, false).is_err());
    }
    // Local setup still means a literal loopback endpoint, never a remote custom host.
    let cleanup = CleanupConfig {
        provider: Some("custom".into()),
        endpoint: Some("https://example.invalid/v1".into()),
        model: Some("m".into()),
        ..Default::default()
    };
    assert!(build_provider_with_cleanup(&AiConfig::default(), &cleanup, false).is_err());
}
#[tokio::test]
async fn quick_default_override_never_uses_stored_provider_key() {
    let mut cleanup = CleanupConfig {
        provider: Some("groq".into()),
        endpoint: Some("https://example.invalid/v1".into()),
        ..Default::default()
    };
    assert!(build_provider_with_cleanup(&AiConfig::default(), &cleanup, false).is_err());
    cleanup.api_key_env = "XFLOW_TEST_QUICK_MISSING_CUSTOM_KEY".into();
    let (selected, gateway) =
        build_provider_with_cleanup(&AiConfig::default(), &cleanup, false).unwrap();
    assert_eq!(selected.provider, "openai-compatible");
    let credential = &gateway.routes[0].credential;
    assert!(!credential.allow_store);
    assert_eq!(
        credential.env.as_deref(),
        Some("XFLOW_TEST_QUICK_MISSING_CUSTOM_KEY")
    );
    assert!(credential.header_for(Auth::Bearer).await.is_err());
    // Provider-less custom remote settings also cannot fall back to the cleanup keystore entry.
    cleanup.provider = None;
    cleanup.model = Some("m".into());
    let (_, gateway) = build_provider_with_cleanup(&AiConfig::default(), &cleanup, false).unwrap();
    assert!(!gateway.routes[0].credential.allow_store);
}
#[tokio::test]
async fn quick_default_loopback_is_anonymous_even_with_cleanup_key_configured() {
    let fixture = serve(vec![ok(vec![sse("data: [DONE]\n\n")])]).await;
    let cleanup = CleanupConfig {
        provider: Some("groq".into()),
        endpoint: Some(fixture.url.clone()),
        api_key_env: "GROQ_API_KEY".into(),
        ..Default::default()
    };
    let (selected, gateway) =
        build_provider_with_cleanup(&AiConfig::default(), &cleanup, true).unwrap();
    assert_eq!(selected.provider, "custom");
    assert!(selected.consented.is_empty());
    assert!(!gateway.routes[0].credential.allow_store);
    assert!(gateway.routes[0].credential.env.is_none());
    let (result, _) = run(&gateway, &request()).await;
    result.unwrap();
    assert!(!fixture.requests.lock().unwrap()[0]
        .to_ascii_lowercase()
        .contains("authorization:"));
    let explicit = AiConfig {
        endpoint: Some(fixture.url),
        model: Some("m".into()),
        ..ai("openai-compatible")
    };
    let gateway = build_provider(&explicit, true).unwrap();
    assert!(gateway.routes[0]
        .credential
        .header_for(Auth::Bearer)
        .await
        .unwrap()
        .is_none());
}
