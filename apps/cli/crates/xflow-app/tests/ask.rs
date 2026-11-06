#![cfg(unix)]
//! `xflow ask` as a user runs it: real binary, loopback server, synthetic data.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

fn command(root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_xflow"));
    c.env_clear()
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("PATH", root.join("no-programs"))
        .env("XFLOW_NO_KEYCHAIN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}
fn config_path(root: &Path) -> PathBuf {
    root.join("config/xflow/config.toml")
}
fn write_config(root: &Path, text: &str) {
    let path = config_path(root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
fn custom_config(url: &str) -> String {
    format!("[ai]\nprovider = \"custom\"\nendpoint = \"{url}\"\nmodel = \"local-model\"\n")
}
fn seed(root: &Path, rows: &[&str]) {
    let path = root.join("data/xflow/history.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("CREATE TABLE history(id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL DEFAULT(unixepoch()), text TEXT NOT NULL, provider TEXT NOT NULL); PRAGMA user_version=3;").unwrap();
    for row in rows {
        conn.execute("INSERT INTO history(text,provider) VALUES(?1,'t')", [row])
            .unwrap();
    }
}
fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut buf = [0; 8192];
    loop {
        let n = socket.read(&mut buf).unwrap();
        assert!(n > 0, "the client closed before finishing its request");
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
                return serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap();
            }
        }
    }
}
fn chunk(socket: &mut TcpStream, data: &str) {
    write!(socket, "{:x}\r\n{data}\r\n", data.len()).unwrap();
    socket.flush().unwrap();
}
const HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
fn delta(text: &str) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":{"content":text}}]})
    )
}
/// Answers each request with `words`, then records its body.
fn answering(words: &'static [&'static str]) -> (String, mpsc::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let mut socket = socket.unwrap();
            let body = read_request(&mut socket);
            let _ = tx.send(body);
            socket.write_all(HEAD.as_bytes()).unwrap();
            for word in words {
                chunk(&mut socket, &delta(word));
            }
            chunk(&mut socket, "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n");
            chunk(&mut socket, "data: [DONE]\n\n");
            socket.write_all(b"0\r\n\r\n").unwrap();
        }
    });
    (url, rx)
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
fn one_line_error(out: &Output) -> String {
    let stderr = text(&out.stderr);
    assert_eq!(stderr.trim_end().lines().count(), 1, "{stderr}");
    assert!(stderr.starts_with("error: "), "{stderr}");
    stderr
}
fn run(c: &mut Command) -> Output {
    c.output().unwrap()
}

#[test]
fn not_set_up_says_how() {
    let d = tempfile::tempdir().unwrap();
    let out = run(command(d.path()).args(["ask", "hello"]));
    assert_eq!(out.status.code(), Some(4));
    let error = one_line_error(&out);
    assert!(
        error.contains("xflow key set") && error.contains("ai.provider"),
        "{error}"
    );
}
#[test]
fn offline_refuses_before_any_consent_question() {
    let d = tempfile::tempdir().unwrap();
    write_config(d.path(), "[privacy]\noffline = true\n");
    for provider in ["openai", "anthropic", "openrouter"] {
        let out = run(command(d.path()).args(["ask", "--provider", provider, "--yes", "hello"]));
        assert_eq!(out.status.code(), Some(5));
        let error = one_line_error(&out);
        assert!(
            error.contains("offline") && error.contains(provider),
            "{error}"
        );
    }
    let (url, rx) = answering(&["local"]);
    write_config(
        d.path(),
        &format!("[privacy]\noffline = true\n{}", custom_config(&url)),
    );
    let out = run(command(d.path()).args(["ask", "--yes", "hello"]));
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "local\n");
    rx.recv().unwrap();
}
#[test]
fn consent_gates_transcripts_off_a_terminal_and_yes_does_not_save() {
    let d = tempfile::tempdir().unwrap();
    let (url, rx) = answering(&["fine"]);
    let config = custom_config(&url);
    write_config(d.path(), &config);
    seed(d.path(), &["budget meeting with the synthetic team"]);
    let out = run(command(d.path()).args(["ask", "budget"]));
    assert_eq!(out.status.code(), Some(2));
    let error = one_line_error(&out);
    assert!(
        error.contains("--yes") && error.contains("custom"),
        "{error}"
    );
    assert!(rx.try_recv().is_err(), "nothing may leave without consent");

    let out = run(command(d.path()).args(["ask", "--yes", "budget"]));
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        std::fs::read_to_string(config_path(d.path())).unwrap(),
        config,
        "--yes holds for one run only"
    );
    let body = rx.recv().unwrap();
    let sent = body["messages"].to_string();
    assert!(sent.contains("untrusted_transcript"), "{sent}");
    assert!(sent.contains("budget meeting"), "{sent}");
    assert!(body["tools"].to_string().contains("search_dictations"));
    assert!(body["stream"].as_bool().unwrap());
    assert!(text(&out.stderr).contains("read: search_dictations"));

    write_config(d.path(), &format!("{config}consented = [\"custom\"]\n"));
    let out = run(command(d.path()).args(["ask", "budget"]));
    assert!(out.status.success(), "{}", text(&out.stderr));
    rx.recv().unwrap();
}
#[test]
fn no_context_sends_only_the_question_and_needs_no_consent() {
    let d = tempfile::tempdir().unwrap();
    let (url, rx) = answering(&["fine"]);
    write_config(d.path(), &custom_config(&url));
    seed(d.path(), &["secret budget words"]);
    let out = run(command(d.path()).args(["ask", "--no-context", "budget"]));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let body = rx.recv().unwrap();
    assert!(body.get("tools").is_none());
    assert!(!body.to_string().contains("secret budget"));
}
#[test]
fn history_off_sends_no_transcripts() {
    let d = tempfile::tempdir().unwrap();
    let (url, rx) = answering(&["fine"]);
    write_config(
        d.path(),
        &format!("[privacy]\nhistory = false\n{}", custom_config(&url)),
    );
    seed(d.path(), &["secret budget words"]);
    let out = run(command(d.path()).args(["ask", "--yes", "budget"]));
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("disabled"),
        "{}",
        text(&out.stderr)
    );
    assert!(!rx.recv().unwrap().to_string().contains("secret budget"));
}
#[test]
fn streams_json_stdin_and_timing() {
    let d = tempfile::tempdir().unwrap();
    let (url, rx) = answering(&["Hello", " there"]);
    write_config(d.path(), &custom_config(&url));
    let mut child = command(d.path())
        .args(["ask", "--yes", "--timing"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"question from stdin\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "Hello there\n");
    let stderr = text(&out.stderr);
    for field in [
        "retrieval",
        "gateway overhead",
        "first token",
        "total",
        "tokens 7 in",
    ] {
        assert!(stderr.contains(field), "{stderr}");
    }
    assert!(rx
        .recv()
        .unwrap()
        .to_string()
        .contains("question from stdin"));

    let out = run(command(d.path()).args(["ask", "--yes", "--json", "hello"]));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["text"], "Hello there");
    assert_eq!(value["stop"], "end");
    assert_eq!(value["provider"], "custom");
    assert_eq!(value["model"], "local-model");
    assert_eq!(value["timing"]["input_tokens"], 7);
}
#[test]
fn empty_question_and_connection_failure_are_one_line_errors() {
    let d = tempfile::tempdir().unwrap();
    let out = run(command(d.path()).args(["ask", "  "]));
    assert_eq!(out.status.code(), Some(2));
    one_line_error(&out);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    drop(listener);
    write_config(d.path(), &custom_config(&url));
    let out = run(command(d.path()).args(["ask", "--no-context", "hello"]));
    assert_eq!(out.status.code(), Some(5));
    let error = one_line_error(&out);
    assert!(error.contains("could not connect"), "{error}");
    assert!(!error.contains("127.0.0.1"), "{error}");
}
#[test]
fn missing_key_names_the_command_and_a_stored_key_is_used() {
    let d = tempfile::tempdir().unwrap();
    let out =
        run(command(d.path()).args(["ask", "--provider", "anthropic", "--no-context", "hello"]));
    assert_eq!(out.status.code(), Some(5));
    let error = one_line_error(&out);
    assert!(error.contains("xflow key set anthropic"), "{error}");

    let mut child = command(d.path())
        .args(["key", "set", "anthropic", "--stdin"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"synthetic-anthropic-key\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!text(&out.stdout).contains("synthetic-anthropic-key"));
    let out = run(command(d.path()).args(["key", "status", "anthropic", "--json"]));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["source"], "keystore");
}
#[test]
fn ctrl_c_cancels_promptly_and_closes_the_connection() {
    let d = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    write_config(d.path(), &custom_config(&url));
    let (closed_tx, closed_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        read_request(&mut socket);
        socket.write_all(HEAD.as_bytes()).unwrap();
        chunk(&mut socket, &delta("partial answer\n"));
        let mut buf = [0; 64];
        while socket.read(&mut buf).is_ok_and(|n| n > 0) {}
        closed_tx.send(()).unwrap();
    });
    let mut child = command(d.path())
        .args(["ask", "--yes", "hello"])
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, "partial answer\n");
    let sent = Instant::now();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let status = child.wait().unwrap();
    let elapsed = sent.elapsed();
    assert_eq!(status.code(), Some(130));
    closed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    // The release binary is measured under 100 ms; this bound is for a loaded debug run.
    println!("ctrl-c to exit: {elapsed:?}");
    assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
}
#[test]
fn a_cut_answer_keeps_its_text_and_fails() {
    let d = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    write_config(d.path(), &custom_config(&url));
    std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            read_request(&mut socket);
            socket.write_all(HEAD.as_bytes()).unwrap();
            chunk(&mut socket, &delta("partial answer"));
        }
    });
    let out = run(command(d.path()).args(["ask", "--no-context", "hello"]));
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(text(&out.stdout), "partial answer\n");
    assert!(one_line_error(&out).contains("cut off"));
    let out = run(command(d.path()).args(["ask", "--no-context", "--json", "hello"]));
    assert_eq!(out.status.code(), Some(5));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["text"], "partial answer");
}
#[test]
fn a_closed_pipe_ends_quietly() {
    let d = tempfile::tempdir().unwrap();
    let (url, _rx) = answering(&["word "; 4]);
    write_config(d.path(), &custom_config(&url));
    let mut child = command(d.path())
        .args(["ask", "--yes", "hello"])
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(
        !text(&out.stderr).contains("panicked"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn cleanup_default_still_requires_consent_and_uses_only_chat_prompt() {
    let d = tempfile::tempdir().unwrap();
    let (url, rx) = answering(&["cleanup model answer"]);
    let config = format!("[cleanup]\nprovider = \"groq\"\nendpoint = \"{url}\"\nmodel = \"fast-model\"\nprompt = \"CLEANUP_ONLY_DO_NOT_SEND\"\n");
    write_config(d.path(), &config);
    let refused = run(command(d.path()).args(["ask", "question"]));
    assert!(!refused.status.success());
    assert!(text(&refused.stderr).contains("--yes"));
    assert!(rx.try_recv().is_err());
    let allowed = run(command(d.path())
        .env("GROQ_API_KEY", "synthetic-not-a-key")
        .args(["ask", "--yes", "question"]));
    assert!(allowed.status.success(), "{}", text(&allowed.stderr));
    let body = rx.recv().unwrap();
    assert_eq!(body["model"], "fast-model");
    assert_eq!(body["stream"], true);
    assert!(!body.to_string().contains("CLEANUP_ONLY_DO_NOT_SEND"));
    assert!(!std::fs::read_to_string(config_path(d.path()))
        .unwrap()
        .contains("consented"));
}
#[test]
fn gateway_owns_a_single_ask_command() {
    let d = tempfile::tempdir().unwrap();
    let output = run(command(d.path()).arg("--help"));
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout)
            .lines()
            .filter(|line| line.trim_start().starts_with("ask "))
            .count(),
        1
    );
}
