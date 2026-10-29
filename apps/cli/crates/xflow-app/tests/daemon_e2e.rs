#![cfg(all(unix, feature = "test-support"))]
//! Runs the real xflowd, transport, SQLite store and HTTP provider. Every child
//! gets an empty environment, private XDG paths and fake audio/desktop adapters.
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UnixStream},
    sync::{mpsc, Semaphore},
    task::{JoinHandle, JoinSet},
    time::{sleep, timeout},
};
use xflow_core::{
    ipc::{Response, MAX_MESSAGE_BYTES, PROTOCOL_VERSION},
    State,
};

const DEADLINE: Duration = Duration::from_secs(10);

struct Daemon {
    child: Child,
    root: TempDir,
}

impl Daemon {
    fn spawn(endpoint: &str, audio: &str, offline: bool) -> Self {
        Self::spawn_with_config(config(endpoint, offline, None), audio, |_, _| {})
    }

    fn spawn_with_protocol(endpoint: &str, audio: &str, offline: bool, protocol: &str) -> Self {
        Self::spawn_with_config(config(endpoint, offline, Some(protocol)), audio, |_, _| {})
    }

    fn spawn_with(
        endpoint: &str,
        audio: &str,
        offline: bool,
        customize: impl FnOnce(&mut Command, &Path),
    ) -> Self {
        Self::spawn_with_config(config(endpoint, offline, None), audio, customize)
    }

    fn spawn_with_config(
        contents: String,
        audio: &str,
        customize: impl FnOnce(&mut Command, &Path),
    ) -> Self {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for subdir in ["run", "config/xflow", "data", "home"] {
            fs::create_dir_all(root.path().join(subdir)).unwrap();
        }
        fs::set_permissions(root.path().join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.path().join("config/xflow/config.toml"), contents).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_xflowd"));
        command
            .env_clear()
            .env("HOME", root.path().join("home"))
            .env("XDG_RUNTIME_DIR", root.path().join("run"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            // Intentionally unavailable private bus: never reach the desktop bus.
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/no-bus", root.path().display()),
            )
            .env("XFLOW_TEST_ROOT", root.path())
            .env("XFLOW_NO_KEYCHAIN", "1")
            .env("XFLOW_TEST_AUDIO", audio)
            .env("XFLOW_TEST_SINK", root.path().join("delivered.txt"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(root.path().join("daemon.log")).unwrap());
        customize(&mut command, root.path());
        let child = command.spawn().unwrap();
        Self { child, root }
    }

    fn socket(&self) -> PathBuf {
        self.root.path().join("run/xflow/daemon.sock")
    }
    fn sink(&self) -> PathBuf {
        self.root.path().join("delivered.txt")
    }

    async fn ready(&mut self) {
        timeout(DEADLINE, async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    panic!(
                        "daemon exited early ({status}): {}",
                        fs::read_to_string(self.root.path().join("daemon.log")).unwrap()
                    );
                }
                if UnixStream::connect(self.socket()).await.is_ok() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("daemon startup timed out");
        let status = self.request(json!({"command":"status"})).await;
        assert!(status.ok);
        assert_eq!(status.protocol, Some(PROTOCOL_VERSION));
    }

    async fn request(&self, request: Value) -> Response {
        timeout(DEADLINE, async {
            let mut stream = UnixStream::connect(self.socket()).await.unwrap();
            stream
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).await.unwrap();
            assert!(
                !line.is_empty(),
                "daemon closed before replying to {request}: {}",
                fs::read_to_string(self.root.path().join("daemon.log")).unwrap()
            );
            serde_json::from_str(&line).unwrap()
        })
        .await
        .expect("IPC request timed out")
    }

    async fn events(&self) -> BufReader<UnixStream> {
        let mut stream = UnixStream::connect(self.socket()).await.unwrap();
        stream
            .write_all(b"{\"command\":\"subscribe\"}\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        assert!(event(&mut reader).await.ok);
        reader
    }

    async fn history(&self) -> Response {
        self.request(json!({"command":"history", "limit":50})).await
    }

    async fn stop_process(&mut self) {
        assert!(self.request(json!({"command":"shutdown"})).await.ok);
        timeout(DEADLINE, async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("shutdown timed out");
        assert!(!self.socket().exists());
    }

    async fn failed_start(&mut self, expected_error: &str) {
        let status = timeout(DEADLINE, async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("invalid configuration did not stop daemon");
        assert!(!status.success());
        let log = fs::read_to_string(self.root.path().join("daemon.log")).unwrap();
        assert!(
            log.contains(expected_error),
            "expected {expected_error}, got {log}"
        );
        assert!(!self.sink().exists());
        assert!(!self.socket().exists());
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Kill only the child we created, including when an assertion unwinds.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn config(endpoint: &str, offline: bool, protocol: Option<&str>) -> String {
    let protocol =
        protocol.map_or_else(String::new, |protocol| format!("protocol = {protocol:?}\n"));
    format!("[stt]\nprovider = \"custom\"\nmodel = \"fixture-model\"\nendpoint = {endpoint:?}\n{protocol}timeout_secs = 5\n[cleanup]\nmode = \"raw\"\n[recording]\nmin_ms = 0\n[injection]\ntrailing_space = false\n[sounds]\nenabled = false\n[notifications]\nenabled = false\n[privacy]\noffline = {offline}\n")
}

async fn event(reader: &mut BufReader<UnixStream>) -> Response {
    let mut line = String::new();
    timeout(DEADLINE, reader.read_line(&mut line))
        .await
        .expect("event timed out")
        .unwrap();
    assert!(!line.is_empty(), "subscription closed");
    serde_json::from_str(&line).unwrap()
}

async fn state(reader: &mut BufReader<UnixStream>, expected: State) -> Response {
    timeout(DEADLINE, async {
        loop {
            let response = event(reader).await;
            if response.state == expected {
                return response;
            }
        }
    })
    .await
    .expect("state transition timed out")
}

struct Plan {
    status: u16,
    text: &'static str,
    gate: Option<Arc<Semaphore>>,
}
impl Plan {
    fn success(text: &'static str) -> Self {
        Self {
            status: 200,
            text,
            gate: None,
        }
    }
}

struct MockStt {
    endpoint: String,
    uploads: mpsc::UnboundedReceiver<Vec<u8>>,
    probes: mpsc::UnboundedReceiver<(String, String)>,
    task: JoinHandle<()>,
}
impl MockStt {
    async fn start(plans: Vec<Plan>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "http://{}/v1/audio/transcriptions",
            listener.local_addr().unwrap()
        );
        let plans = Arc::new(Mutex::new(VecDeque::from(plans)));
        let (sent, uploads) = mpsc::unbounded_channel();
        let (probed, probes) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let plans = plans.clone();
                        let sent = sent.clone();
                        let probed = probed.clone();
                        clients.spawn(async move {
                            // Preconnect/warm may close without sending a request.
                            handle_provider_request(stream, plans, sent, probed).await;
                        });
                    }
                    result = clients.join_next(), if !clients.is_empty() => { result.unwrap().unwrap(); }
                }
            }
        });
        Self {
            endpoint,
            uploads,
            probes,
            task,
        }
    }
    async fn upload(&mut self) -> Vec<u8> {
        timeout(DEADLINE, self.uploads.recv())
            .await
            .expect("STT upload timed out")
            .expect("mock server stopped")
    }

    async fn probe(&mut self) -> (String, String) {
        timeout(DEADLINE, self.probes.recv())
            .await
            .expect("provider warm-up probe timed out")
            .expect("mock server stopped")
    }
}
impl Drop for MockStt {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_provider_request(
    stream: TcpStream,
    plans: Arc<Mutex<VecDeque<Plan>>>,
    sent: mpsc::UnboundedSender<Vec<u8>>,
    probed: mpsc::UnboundedSender<(String, String)>,
) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        // An aborted warm-up probe resets its connection; treat that like EOF.
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let mut request = line.split_ascii_whitespace();
        let method = request.next().expect("request method").to_owned();
        let path = request.next().expect("request path").to_owned();
        assert_eq!(request.next(), Some("HTTP/1.1"));
        let mut length = None;
        let mut multipart = false;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            if line == "\r\n" {
                break;
            }
            let lower = line.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("content-length:") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
            if lower.starts_with("content-type: multipart/form-data;") {
                multipart = true;
            }
            assert!(
                !lower.starts_with("authorization:"),
                "loopback fixture must be anonymous"
            );
        }
        match (method.as_str(), path.as_str()) {
            ("GET", "/v1/models") => {
                // Read-only warm-up responses never touch the billable upload plans.
                probed.send((method, path)).unwrap();
                let body = r#"{"data":[]}"#;
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                if reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
            ("HEAD", "/") => {
                // AssemblyAI sync uses HEAD on the origin before its multipart POST.
                probed.send((method, path)).unwrap();
                if reader
                    .get_mut()
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
            ("POST", "/v1/audio/transcriptions") => (),
            _ => panic!("unexpected provider request: {method} {path}"),
        }
        assert!(multipart, "upload must use the multipart protocol");
        let length = length.expect("bounded content-length required");
        // A 60 s segment, the longest upload, is a 1.9 MB WAV.
        assert!(length < 2_000_000);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await.unwrap();
        sent.send(body).unwrap();
        let plan = plans
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected provider upload");
        if let Some(gate) = plan.gate {
            gate.acquire().await.unwrap().forget();
        }
        let body = if plan.status == 200 {
            json!({"text":plan.text}).to_string()
        } else {
            "private provider error details".into()
        };
        // Cancel may close the HTTP connection before the withheld response arrives.
        let response = format!("HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", plan.status, body.len(), body);
        let _ = reader.get_mut().write_all(response.as_bytes()).await;
        return;
    }
}

async fn dictate(daemon: &Daemon, events: &mut BufReader<UnixStream>) {
    assert!(daemon.request(json!({"command":"start"})).await.ok);
    // Consume earlier sessions' final events before waiting for this result.
    state(events, State::Listening).await;
    assert!(daemon.request(json!({"command":"stop"})).await.ok);
    state(events, State::Success).await;
}

#[tokio::test]
async fn transcript_is_uploaded_delivered_and_persisted() {
    let mut mock = MockStt::start(vec![Plan::success("fixture transcript")]).await;
    let mut daemon = Daemon::spawn(&mock.endpoint, "voice", true);
    daemon.ready().await;
    let mut events = daemon.events().await;
    dictate(&daemon, &mut events).await;
    let upload = mock.upload().await;
    let wav = upload
        .windows(4)
        .position(|bytes| bytes == b"RIFF")
        .expect("PCM WAV in multipart upload");
    assert_eq!(&upload[wav + 8..wav + 12], b"WAVE");
    assert_eq!(
        u32::from_le_bytes(upload[wav + 24..wav + 28].try_into().unwrap()),
        16_000
    );
    assert!(upload.windows(13).any(|bytes| bytes == b"fixture-model"));
    assert_eq!(
        fs::read_to_string(daemon.sink()).unwrap(),
        "fixture transcript"
    );
    let history = daemon.history().await;
    assert!(history.ok);
    assert_eq!(history.history.len(), 1);
    assert_eq!(history.history[0].text, "fixture transcript");
    assert_eq!(history.history[0].provider, "custom");
    let db = rusqlite::Connection::open(daemon.root.path().join("data/xflow/history.db")).unwrap();
    let stored: String = db
        .query_row("SELECT text FROM history", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, "fixture transcript");
    assert_eq!(
        daemon
            .request(json!({"command":"last"}))
            .await
            .text
            .as_deref(),
        Some("fixture transcript")
    );
    daemon.stop_process().await;
}

#[tokio::test]
async fn get_and_head_warm_up_probes_do_not_consume_upload_plans() {
    for (protocol, expected_probe, transcript) in [
        (None, ("GET", "/v1/models"), "after GET warm-up"),
        (Some("assemblyai-sync"), ("HEAD", "/"), "after HEAD warm-up"),
    ] {
        let mut mock = MockStt::start(vec![Plan::success(transcript)]).await;
        let mut daemon = match protocol {
            Some(protocol) => Daemon::spawn_with_protocol(&mock.endpoint, "voice", true, protocol),
            None => Daemon::spawn(&mock.endpoint, "voice", true),
        };
        daemon.ready().await;
        let mut events = daemon.events().await;
        dictate(&daemon, &mut events).await;

        assert_eq!(
            mock.probe().await,
            (expected_probe.0.to_owned(), expected_probe.1.to_owned())
        );
        let upload = mock.upload().await;
        assert!(upload.windows(4).any(|bytes| bytes == b"RIFF"));
        assert_eq!(fs::read_to_string(daemon.sink()).unwrap(), transcript);
        assert_eq!(daemon.history().await.history[0].text, transcript);
        daemon.stop_process().await;
    }
}

#[tokio::test]
async fn processing_cancel_discards_late_result_and_allows_next_session() {
    let gate = Arc::new(Semaphore::new(0));
    let mut mock = MockStt::start(vec![
        Plan {
            status: 200,
            text: "cancelled transcript",
            gate: Some(gate.clone()),
        },
        Plan::success("next session"),
    ])
    .await;
    let mut daemon = Daemon::spawn(&mock.endpoint, "voice", false);
    daemon.ready().await;
    let mut events = daemon.events().await;
    assert!(daemon.request(json!({"command":"start"})).await.ok);
    assert!(daemon.request(json!({"command":"stop"})).await.ok);
    mock.upload().await;
    assert_eq!(
        daemon.request(json!({"command":"status"})).await.state,
        State::Processing
    );
    assert!(daemon.request(json!({"command":"cancel"})).await.ok);
    state(&mut events, State::Idle).await;
    gate.add_permits(1);
    dictate(&daemon, &mut events).await;
    mock.upload().await;
    assert_eq!(fs::read_to_string(daemon.sink()).unwrap(), "next session");
    let history = daemon.history().await.history;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "next session");
    daemon.stop_process().await;
}

#[tokio::test]
async fn provider_500_is_redacted_and_next_session_recovers() {
    let mut mock = MockStt::start(vec![
        Plan {
            status: 500,
            text: "",
            gate: None,
        },
        Plan::success("recovered"),
    ])
    .await;
    let mut daemon = Daemon::spawn(&mock.endpoint, "voice", false);
    daemon.ready().await;
    let mut events = daemon.events().await;
    assert!(daemon.request(json!({"command":"start"})).await.ok);
    assert!(daemon.request(json!({"command":"stop"})).await.ok);
    let failed = state(&mut events, State::Error).await;
    assert!(!failed
        .message
        .unwrap()
        .contains("private provider error details"));
    assert!(daemon.history().await.history.is_empty());
    assert!(!daemon.sink().exists());
    dictate(&daemon, &mut events).await;
    mock.upload().await;
    mock.upload().await;
    assert_eq!(fs::read_to_string(daemon.sink()).unwrap(), "recovered");
    assert_eq!(daemon.history().await.history.len(), 1);
    daemon.stop_process().await;
}

#[tokio::test]
async fn silence_never_uploads() {
    let mut mock = MockStt::start(vec![]).await;
    let mut daemon = Daemon::spawn(&mock.endpoint, "silence", true);
    daemon.ready().await;
    assert!(daemon.request(json!({"command":"start"})).await.ok);
    let stopped = daemon.request(json!({"command":"stop"})).await;
    assert!(stopped.ok);
    assert_eq!(stopped.state, State::Idle);
    assert_eq!(stopped.message.as_deref(), Some("Didn't catch anything"));
    assert!(timeout(Duration::from_millis(150), mock.uploads.recv())
        .await
        .is_err());
    assert!(!daemon.sink().exists());
    assert!(daemon.history().await.history.is_empty());
    daemon.stop_process().await;
}

#[tokio::test]
async fn offline_rejects_remote_endpoint_before_services_start() {
    let mut daemon = Daemon::spawn(
        "https://example.invalid/v1/audio/transcriptions",
        "voice",
        true,
    );
    daemon.failed_start("offline mode").await;
}

#[tokio::test]
async fn unsafe_or_incomplete_test_environment_is_rejected_before_startup() {
    type Mutation = fn(&mut Command, &Path);
    let cases: [(Mutation, &str); 6] = [
        (
            |command, _| {
                command.env_remove("XFLOW_TEST_AUDIO");
            },
            "must be set together",
        ),
        (
            |command, _| {
                command.env_remove("XFLOW_TEST_SINK");
            },
            "must be set together",
        ),
        (
            |command, _| {
                command.env("DISPLAY", ":99");
            },
            "require DISPLAY",
        ),
        (
            |command, _| {
                command.env("WAYLAND_DISPLAY", "fixture-display");
            },
            "require DISPLAY",
        ),
        (
            |command, root| {
                command.env("XFLOW_TEST_SINK", root.join("elsewhere.txt"));
            },
            "test sink must be",
        ),
        (
            |command, root| {
                let alias = root.join("run-alias");
                std::os::unix::fs::symlink(root.join("run"), &alias).unwrap();
                command.env("XDG_RUNTIME_DIR", alias);
            },
            "test XDG directories",
        ),
    ];
    for (customize, message) in cases {
        let mut daemon = Daemon::spawn_with(
            "http://127.0.0.1:9/transcriptions",
            "voice",
            true,
            customize,
        );
        daemon.failed_start(message).await;
        assert!(!daemon.root.path().join("data/xflow").exists());
    }
}

#[tokio::test]
async fn concurrent_clients_and_malformed_frames_leave_daemon_responsive() {
    let mock = MockStt::start(vec![]).await;
    let mut daemon = Daemon::spawn(&mock.endpoint, "voice", true);
    daemon.ready().await;
    let mut requests = Vec::new();
    for _ in 0..16 {
        requests.push(daemon.request(json!({"command":"status"})));
    }
    for response in futures_util::future::join_all(requests).await {
        assert!(response.ok);
    }
    let responses =
        futures_util::future::join_all((0..8).map(|_| daemon.request(json!({"command":"start"}))))
            .await;
    assert_eq!(responses.iter().filter(|response| response.ok).count(), 1);
    assert!(daemon.request(json!({"command":"cancel"})).await.ok);
    for frame in [
        b"not json\n".to_vec(),
        b"{\"command\":\"unknown\"}\n".to_vec(),
        b"{\"command\":\"status\"}".to_vec(),
        // Reject at the read limit without racing a close against remaining writes.
        vec![b'x'; MAX_MESSAGE_BYTES + 1],
    ] {
        let mut stream = UnixStream::connect(daemon.socket()).await.unwrap();
        stream.write_all(&frame).await.unwrap();
        // Only truncated frames need EOF; the daemon may already have closed the others.
        if frame.len() <= MAX_MESSAGE_BYTES && !frame.ends_with(b"\n") {
            stream.shutdown().await.unwrap();
        }
        let response = event(&mut BufReader::new(stream)).await;
        assert!(!response.ok);
        assert_eq!(response.protocol, Some(PROTOCOL_VERSION));
        assert!(daemon.request(json!({"command":"status"})).await.ok);
    }
    daemon.stop_process().await;
}

#[tokio::test]
async fn reload_changes_provider_and_invalid_config_keeps_running_config() {
    let mut original = MockStt::start(vec![Plan::success("original")]).await;
    let mut replacement = MockStt::start(vec![
        Plan::success("replacement"),
        Plan::success("retained"),
    ])
    .await;
    let mut daemon = Daemon::spawn(&original.endpoint, "voice", true);
    daemon.ready().await;
    let mut events = daemon.events().await;
    dictate(&daemon, &mut events).await;
    original.upload().await;
    let path = daemon.root.path().join("config/xflow/config.toml");
    // Changing recording settings exercises both audio and desktop rebuilding.
    let reconfigured = config(&replacement.endpoint, true, None)
        .replace("min_ms = 0", "min_ms = 0\nmax_seconds = 60");
    fs::write(&path, reconfigured).unwrap();
    assert!(daemon.request(json!({"command":"reload"})).await.ok);
    dictate(&daemon, &mut events).await;
    replacement.upload().await;
    assert_eq!(fs::read_to_string(daemon.sink()).unwrap(), "replacement");
    fs::write(&path, "[recording]\nmax_seconds = 0\n").unwrap();
    assert!(!daemon.request(json!({"command":"reload"})).await.ok);
    dictate(&daemon, &mut events).await;
    replacement.upload().await;
    assert_eq!(fs::read_to_string(daemon.sink()).unwrap(), "retained");
    assert_eq!(daemon.history().await.history.len(), 3);
    daemon.stop_process().await;
}

#[tokio::test]
async fn shutdown_flushes_reply_and_closes_live_subscribers() {
    let mock = MockStt::start(vec![]).await;
    for attempt in 0..16 {
        let mut daemon = Daemon::spawn_with(&mock.endpoint, "voice", true, |command, _| {
            command.env("TOKIO_WORKER_THREADS", "2");
        });
        daemon.ready().await;
        let mut events = daemon.events().await;
        let _stalled = if attempt == 0 {
            let stream = UnixStream::connect(daemon.socket()).await.unwrap();
            assert!(daemon.request(json!({"command":"status"})).await.ok);
            Some(stream)
        } else {
            None
        };
        daemon.stop_process().await;
        timeout(DEADLINE, async {
            let mut line = String::new();
            while events.read_line(&mut line).await.unwrap() != 0 {
                line.clear();
            }
        })
        .await
        .expect("subscriber stayed open after shutdown");
    }
}

#[tokio::test]
async fn corrupt_history_is_preserved_while_dictation_continues() {
    let mut mock = MockStt::start(vec![Plan::success("Recovered dictation")]).await;
    let mut daemon = Daemon::spawn_with(&mock.endpoint, "voice", true, |_, root| {
        fs::create_dir_all(root.join("data/xflow")).unwrap();
        fs::write(
            root.join("data/xflow/history.db"),
            b"original corrupt fixture",
        )
        .unwrap();
    });
    daemon.ready().await;
    assert!(daemon
        .request(json!({"command":"status"}))
        .await
        .message
        .unwrap()
        .contains("History unavailable"));
    let mut events = daemon.events().await;
    daemon.request(json!({"command":"start"})).await;
    daemon.request(json!({"command":"stop"})).await;
    let final_event = state(&mut events, State::Success).await;
    assert_eq!(final_event.state, State::Success);
    assert!(final_event.message.unwrap().contains("History unavailable"));
    assert_eq!(
        fs::read(daemon.root.path().join("data/xflow/history.db")).unwrap(),
        b"original corrupt fixture"
    );
    mock.upload().await;
    daemon.stop_process().await;
}
#[tokio::test]
async fn sighup_reloads_only_the_isolated_daemon() {
    let mock = MockStt::start(vec![]).await;
    let mut daemon = Daemon::spawn(&mock.endpoint, "voice", true);
    daemon.ready().await;
    fs::write(
        daemon.root.path().join("config/xflow/config.toml"),
        config(&mock.endpoint, true, None)
            .replace("fixture-model", "hup-model")
            .replace("min_ms = 0", "min_ms = 5000"),
    )
    .unwrap();
    assert_eq!(
        unsafe { libc::kill(daemon.child.id() as libc::pid_t, libc::SIGHUP) },
        0
    );
    timeout(DEADLINE, async {
        loop {
            if daemon
                .request(json!({"command":"status"}))
                .await
                .model
                .as_deref()
                == Some("hup-model")
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    daemon.request(json!({"command":"start"})).await;
    let stopped = daemon.request(json!({"command":"stop"})).await;
    assert_eq!(stopped.state, State::Idle);
    assert!(stopped.message.unwrap().contains("too short"));
    daemon.stop_process().await;
}

/// The real `xflow` binary, confined to this daemon's sandbox paths.
fn client(daemon: &Daemon) -> Command {
    let root = daemon.root.path();
    let mut command = Command::new(env!("CARGO_BIN_EXE_xflow"));
    command
        .env_clear()
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/no-bus", root.display()),
        );
    command
}

/// Kills the client on unwind, before the daemon it talks to is dropped.
struct Owner(Child);
impl Owner {
    fn kill(&mut self, signal: libc::c_int) -> std::time::Instant {
        assert_eq!(unsafe { libc::kill(self.0.id() as libc::pid_t, signal) }, 0);
        let killed = std::time::Instant::now();
        self.0.wait().unwrap();
        killed
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The owner died while Listening: xflowd alone must notice and release the mic.
async fn released(
    daemon: &Daemon,
    events: &mut BufReader<UnixStream>,
    mock: &mut MockStt,
    killed: std::time::Instant,
    what: &str,
) {
    state(events, State::Idle).await;
    let held = killed.elapsed();
    assert!(held < Duration::from_secs(1), "{what}: mic held {held:?}");
    eprintln!("{what}: mic released {held:?} after the owner died");
    let status = daemon.request(json!({"command":"status"})).await;
    assert_eq!(status.state, State::Idle, "{what}");
    assert!(
        timeout(Duration::from_millis(200), mock.uploads.recv())
            .await
            .is_err(),
        "{what}: an orphaned recording was uploaded"
    );
    assert!(daemon.history().await.history.is_empty(), "{what}");
    assert!(!daemon.sink().exists(), "{what}");
}

#[tokio::test]
async fn killing_xflow_listen_releases_the_microphone() {
    let mut mock = MockStt::start(vec![]).await;
    let mut daemon = Daemon::spawn(&mock.endpoint, "voice", true);
    daemon.ready().await;
    let mut events = daemon.events().await;
    for (signal, what) in [
        (libc::SIGTERM, "listen SIGTERM"),
        (libc::SIGHUP, "listen SIGHUP"),
        (libc::SIGKILL, "listen SIGKILL"),
    ] {
        let mut cli = Owner(
            client(&daemon)
                .args(["listen", "--seconds", "60"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        state(&mut events, State::Listening).await;
        let killed = cli.kill(signal);
        released(&daemon, &mut events, &mut mock, killed, what).await;
    }
    // Detached starts (`xflow start`, hotkeys, D-Bus) still outlive their requester.
    let started = client(&daemon)
        .arg("start")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    sleep(Duration::from_millis(200)).await;
    let status = daemon.request(json!({"command":"status"})).await;
    assert_eq!(status.state, State::Listening);
    assert!(daemon.request(json!({"command":"cancel"})).await.ok);
    daemon.stop_process().await;
}

/// A fresh pseudo-terminal: (master, terminal).
fn pty() -> (File, std::os::fd::OwnedFd) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // ptsname returns a static buffer; tests run on parallel threads.
    static NAME: Mutex<()> = Mutex::new(());
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0);
        let master = OwnedFd::from_raw_fd(master);
        assert_eq!(libc::grantpt(master.as_raw_fd()), 0);
        assert_eq!(libc::unlockpt(master.as_raw_fd()), 0);
        let path = {
            let _name = NAME.lock().unwrap();
            std::ffi::CStr::from_ptr(libc::ptsname(master.as_raw_fd())).to_owned()
        };
        let terminal = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        assert!(terminal >= 0);
        let terminal = OwnedFd::from_raw_fd(terminal);
        let size = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            libc::ioctl(terminal.as_raw_fd(), libc::TIOCSWINSZ, &size),
            0
        );
        (File::from(master), terminal)
    }
}

#[tokio::test]
async fn killing_the_tui_releases_the_microphone() {
    use std::{io::Read, io::Write, os::unix::process::CommandExt};
    let mut mock = MockStt::start(vec![]).await;
    let mut daemon = Daemon::spawn(&mock.endpoint, "voice", true);
    daemon.ready().await;
    let mut events = daemon.events().await;
    for (signal, what) in [
        (libc::SIGTERM, "TUI SIGTERM"),
        // What closing the terminal window sends.
        (libc::SIGHUP, "TUI SIGHUP"),
        (libc::SIGKILL, "TUI SIGKILL"),
    ] {
        let (mut master, terminal) = pty();
        let mut command = client(&daemon);
        command
            .arg("tui")
            .env("TERM", "xterm-256color")
            .stdin(terminal.try_clone().unwrap())
            .stdout(terminal.try_clone().unwrap())
            .stderr(terminal);
        // Its own session, with the pty as controlling terminal, like a terminal tab.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut tui = Owner(command.spawn().unwrap());
        drop(command);
        // Drain the screen so the TUI never blocks on a full pty buffer.
        let screen = Arc::new(Mutex::new(Vec::new()));
        let (mut reader, drawn) = (master.try_clone().unwrap(), screen.clone());
        std::thread::spawn(move || {
            let mut buf = [0; 4096];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                drawn.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        timeout(DEADLINE, async {
            while !String::from_utf8_lossy(&screen.lock().unwrap()).contains("dictate") {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{what}: TUI never drew its home screen: {}",
                String::from_utf8_lossy(&screen.lock().unwrap())
            )
        });
        master.write_all(b" ").unwrap();
        state(&mut events, State::Listening).await;
        let killed = tui.kill(signal);
        released(&daemon, &mut events, &mut mock, killed, what).await;
    }
    daemon.stop_process().await;
}

#[tokio::test]
async fn long_dictation_streams_segments_through_the_real_daemon() {
    const WORDS: [&str; 8] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight",
    ];
    let mut mock = MockStt::start(WORDS.iter().map(|w| Plan::success(w)).collect()).await;
    // Three minutes of 48 kHz "speech": 2.6 s tone bursts, then 0.4 s pauses.
    const RATE: u32 = 48_000;
    let mut daemon = Daemon::spawn_with(&mock.endpoint, "wav", true, |command, root| {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(root.join("input.wav"), spec).unwrap();
        for i in 0..RATE * 180 {
            let paused = i % (RATE * 3) >= RATE * 26 / 10;
            let s = if paused {
                0.0
            } else {
                0.3 * (i as f32 * 0.06).sin()
            };
            wav.write_sample((s * 32767.0) as i16).unwrap();
        }
        wav.finalize().unwrap();
        command.env("XFLOW_TEST_AUDIO_SPEED", "30");
    });
    daemon.ready().await;
    let mut events = daemon.events().await;
    assert!(daemon.request(json!({"command":"start"})).await.ok);
    state(&mut events, State::Listening).await;
    // The capture finishes at the end of the file, which stops the dictation.
    state(&mut events, State::Processing).await;
    // The Processing event is sent after stop() queues the tail. Under load the
    // bodies of segments already handed to the provider are still in
    // spawn_blocking, so try_recv at this instant races. The daemon records each
    // send before the stop line; that order is the "while recording" proof.
    let trace = fs::read_to_string(daemon.root.path().join("segments.trace")).unwrap();
    let while_recording = trace
        .lines()
        .take_while(|line| !line.starts_with("stop"))
        .filter(|line| line.starts_with("send"))
        .count();
    assert!(
        while_recording >= 2,
        "segments upload while recording ({while_recording} before stop)\n{trace}"
    );
    let mut uploads = Vec::new();
    while let Ok(upload) = mock.uploads.try_recv() {
        uploads.push(upload);
    }
    let done = state(&mut events, State::Success).await;
    while let Ok(upload) = timeout(Duration::from_millis(200), mock.upload()).await {
        uploads.push(upload);
    }
    assert!(
        (3..=6).contains(&uploads.len()),
        "{} segments",
        uploads.len()
    );
    assert_eq!(
        fs::read_to_string(daemon.sink()).unwrap(),
        WORDS[..uploads.len()].join(" ")
    );
    // Every input frame reaches the provider exactly once (16 kHz after resampling).
    let frames: usize = uploads
        .iter()
        .map(|upload| {
            let riff = upload.windows(4).position(|b| b == b"RIFF").unwrap();
            let data = riff
                + 12
                + upload[riff + 12..]
                    .windows(4)
                    .position(|b| b == b"data")
                    .unwrap();
            u32::from_le_bytes(upload[data + 4..data + 8].try_into().unwrap()) as usize / 2
        })
        .sum();
    assert!(
        frames.abs_diff(16_000 * 180) <= uploads.len(),
        "{frames} frames"
    );
    assert_eq!(done.timings.unwrap().audio_ms, 180_000);
    daemon.stop_process().await;
}
