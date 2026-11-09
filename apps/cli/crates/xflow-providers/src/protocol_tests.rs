use super::tests::{body, clip, config, server, set_test_key};
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use xflow_core::{TimedSpan, Timing};

pub(super) async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        assert!(n > 0, "unexpected connection close");
        bytes.extend_from_slice(&buf[..n]);
        if let Some(offset) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..offset]).to_ascii_lowercase();
            let size = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap_or("0")
                .parse::<usize>()
                .unwrap();
            if bytes.len() >= offset + 4 + size {
                return bytes;
            }
        }
    }
}
pub(super) async fn scripted(
    replies: Vec<(&'static str, Value)>,
    path: &str,
) -> (String, tokio::task::JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}{path}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, value) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_request(&mut socket).await);
            let body = value.to_string();
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    (url, task)
}
#[tokio::test]
async fn new_multipart_protocols_send_exact_fields() {
    for (id, fields, absent) in [
        (
            "openai",
            vec![
                "name=\"languages[]\"\r\n\r\nen",
                "name=\"keywords[]\"\r\n\r\nPostgres",
                "gpt-transcribe",
            ],
            vec!["name=\"language\"", "name=\"prompt\""],
        ),
        (
            "mistral",
            vec![
                "name=\"context_bias\"\r\n\r\nPostgres",
                "voxtral-mini-latest",
            ],
            vec!["name=\"response_format\"", "name=\"prompt\""],
        ),
        (
            "elevenlabs",
            vec![
                "xi-api-key: test-secret-123",
                "name=\"model_id\"",
                "name=\"keyterms\"",
                "name=\"tag_audio_events\"\r\n\r\nfalse",
            ],
            vec!["authorization:", "name=\"prompt\""],
        ),
        (
            "together",
            vec![
                "authorization: Bearer test-secret-123",
                "openai/whisper-large-v3",
                "name=\"prompt\"",
            ],
            vec!["input_audio"],
        ),
        (
            "deepinfra",
            vec![
                "authorization: Bearer test-secret-123",
                "openai/whisper-large-v3-turbo",
                "name=\"prompt\"",
            ],
            vec!["input_audio"],
        ),
    ] {
        let (url, rx) = server(
            "200 OK",
            br#"{"text":" hi ","languages":[{"code":"en"}]}"#,
            Duration::ZERO,
            "",
        )
        .await;
        let mut cfg = config(id, url);
        cfg.language = Some("en".into());
        cfg.vocabulary = vec!["Postgres".into()];
        let env = set_test_key(&mut cfg);
        let result = build_stt(&cfg, true)
            .unwrap()
            .transcribe(clip(), Default::default())
            .await
            .unwrap();
        assert_eq!(result.text, "hi");
        assert_eq!(result.language.as_deref(), Some("en"));
        let bytes = rx.await.unwrap();
        let request = String::from_utf8_lossy(&bytes);
        assert!(request.starts_with("POST /transcribe "));
        for field in fields {
            assert!(request.contains(field), "{id}: missing {field}");
        }
        for field in absent {
            assert!(!request.contains(field), "{id}: unexpected {field}");
        }
        std::env::remove_var(env);
    }
}
#[tokio::test]
async fn deepgram_raw_query_auth_and_language() {
    let (url,rx)=server("200 OK",br#"{"results":{"channels":[{"detected_language":"fr","alternatives":[{"transcript":"bonjour"}]}]}}"#,Duration::ZERO,"").await;
    let mut cfg = config("deepgram", url);
    cfg.vocabulary = vec!["Postgres".into()];
    let env = set_test_key(&mut cfg);
    let result = build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap();
    assert_eq!(result.language.as_deref(), Some("fr"));
    let bytes = rx.await.unwrap();
    let request = String::from_utf8_lossy(&bytes);
    for field in [
        "authorization: Token test-secret-123",
        "model=nova-3",
        "detect_language=true",
        "keyterm=Postgres",
        "mip_opt_out=true",
        "filler_words=true",
    ] {
        assert!(request.contains(field), "{field}");
    }
    assert_eq!(body(&bytes), encode_wav(&clip()).unwrap());
    std::env::remove_var(env);
}
#[tokio::test]
async fn gemini_json_and_non_thought_text() {
    let (url,rx)=server("200 OK",br#"{"candidates":[{"finishReason":"STOP","content":{"parts":[{"thought":true,"text":"private thought"},{"text":"hello "},{"text":"world"}]}}]}"#,Duration::ZERO,"").await;
    let mut cfg = config("gemini", url);
    cfg.language = Some("en".into());
    cfg.vocabulary = vec!["Postgres".into()];
    let env = set_test_key(&mut cfg);
    let result = build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap();
    assert_eq!(result.text, "hello world");
    let bytes = rx.await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("x-goog-api-key: test-secret-123"));
    let value: Value = serde_json::from_slice(body(&bytes)).unwrap();
    assert_eq!(
        value["generationConfig"]["audioTranscriptionConfig"]["mode"],
        "VERBATIM"
    );
    assert_eq!(
        value["generationConfig"]["audioTranscriptionConfig"]["languageCodes"],
        json!(["en-US"])
    );
    assert_eq!(
        value["generationConfig"]["audioTranscriptionConfig"]["customVocabulary"],
        json!(["Postgres"])
    );
    assert_eq!(
        STANDARD
            .decode(
                value["contents"][0]["parts"][0]["inlineData"]["data"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
        encode_wav(&clip()).unwrap()
    );
    std::env::remove_var(env);
}
#[tokio::test]
async fn assembly_upload_submit_and_poll_never_resubmit() {
    let (url, task) = scripted(
        vec![
            ("200 OK", json!({"upload_url":"https://cdn.example/upload"})),
            ("200 OK", json!({"id":"abc-123","status":"queued"})),
            (
                "200 OK",
                json!({"id":"abc-123","status":"completed","text":"done","language_code":"fr"}),
            ),
        ],
        "/v2/transcript",
    )
    .await;
    let mut cfg = config("assemblyai", url);
    let env = set_test_key(&mut cfg);
    let result = build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap();
    assert_eq!(result.text, "done");
    let req = task.await.unwrap();
    assert!(req[0].starts_with(b"POST /v2/upload "));
    assert!(req[1].starts_with(b"POST /v2/transcript "));
    assert!(req[2].starts_with(b"GET /v2/transcript/abc-123 "));
    for r in &req {
        assert!(String::from_utf8_lossy(r).contains("authorization: test-secret-123"));
    }
    assert_eq!(body(&req[0]), encode_wav(&clip()).unwrap());
    let cfg: Value = serde_json::from_slice(body(&req[1])).unwrap();
    assert_eq!(cfg["language_detection"], true);
    assert_eq!(cfg["speech_models"], json!(["universal-3-5-pro"]));
    std::env::remove_var(env);
}
#[tokio::test]
async fn cache_survives_requests_and_invalidates_on_auth_failure() {
    let (url, task) = scripted(
        vec![
            ("200 OK", json!({"text":"one"})),
            ("401 Unauthorized", json!({"secret":"redact"})),
            ("200 OK", json!({"text":"two"})),
        ],
        "/v1/audio/transcriptions",
    )
    .await;
    let mut cfg = config("groq", url);
    let env = set_test_key(&mut cfg);
    let provider = build_stt(&cfg, true).unwrap();
    provider
        .transcribe(clip(), Default::default())
        .await
        .unwrap();
    std::env::set_var(&env, "replacement-test-secret");
    let error = provider
        .transcribe(clip(), Default::default())
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("redact"));
    provider
        .transcribe(clip(), Default::default())
        .await
        .unwrap();
    let requests = task.await.unwrap();
    assert!(String::from_utf8_lossy(&requests[1]).contains("test-secret-123"));
    assert!(String::from_utf8_lossy(&requests[2]).contains("replacement-test-secret"));
    std::env::remove_var(env);
}
#[tokio::test]
async fn warm_reuses_one_connection_for_real_upload() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/v1/audio/transcriptions",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let warm = read_request(&mut socket).await;
        let payload = b"{}";
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(payload).await.unwrap();
        let upload = tokio::time::timeout(Duration::from_secs(2), read_request(&mut socket))
            .await
            .expect("warm-up socket was not reused");
        let payload = br#"{"text":"warm"}"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(payload).await.unwrap();
        (warm, upload)
    });
    let cfg = SttConfig {
        provider: "custom".into(),
        endpoint: Some(url),
        model: Some("test".into()),
        ..Default::default()
    };
    let provider = build_stt(&cfg, true).unwrap();
    provider.warm().await.unwrap();
    provider
        .transcribe(clip(), Default::default())
        .await
        .unwrap();
    let (warm, upload) = task.await.unwrap();
    assert!(warm.starts_with(b"GET /v1/models "));
    assert!(!warm.windows(4).any(|w| w == b"RIFF"));
    assert!(upload.starts_with(b"POST /v1/audio/transcriptions "));
}
#[tokio::test]
async fn cleanup_warm_opens_the_connection_the_edit_then_reuses() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let warm = read_request(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
            .await
            .unwrap();
        let edit = tokio::time::timeout(Duration::from_secs(2), read_request(&mut socket))
            .await
            .expect("warm-up socket was not reused");
        let payload = br#"{"choices":[{"message":{"content":"Warm."}}]}"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(payload).await.unwrap();
        (warm, edit)
    });
    let cfg = CleanupConfig {
        mode: CleanupMode::Light,
        endpoint: Some(url),
        model: Some("test".into()),
        api_key_env: String::new(),
        ..Default::default()
    };
    let transformer = build_transformer(&cfg, true).unwrap().unwrap();
    transformer.warm().await.unwrap();
    let text = transformer
        .transform(TransformRequest {
            text: "warm",
            mode: CleanupMode::Light,
            instructions: None,
            command: None,
            app_id: None,
            vocabulary: &[],
        })
        .await
        .unwrap();
    assert_eq!(text, "Warm.");
    let (warm, edit) = task.await.unwrap();
    assert!(
        warm.starts_with(b"GET /v1/models "),
        "a free model list, never text"
    );
    assert!(edit.starts_with(b"POST /v1/chat/completions "));
}
#[tokio::test]
async fn check_is_authenticated_and_never_sends_audio() {
    let (url, task) = scripted(vec![("200 OK", json!({"data":[]}))], "/transcribe").await;
    let mut cfg = config("openrouter", url);
    let env = set_test_key(&mut cfg);
    let report = check_stt(&cfg, true).await.unwrap();
    assert!(report.ok);
    let requests = task.await.unwrap();
    assert!(requests[0].starts_with(b"GET /api/v1/key "));
    assert!(body(&requests[0]).is_empty());
    assert!(String::from_utf8_lossy(&requests[0]).contains("authorization: Bearer test-secret-123"));
    std::env::remove_var(env);
}
#[tokio::test]
async fn cleanup_command_context_and_vocabulary() {
    let (url, rx) = server(
        "200 OK",
        br#"{"choices":[{"message":{"content":"new text"}}]}"#,
        Duration::ZERO,
        "",
    )
    .await;
    let cfg = CleanupConfig {
        mode: CleanupMode::Custom,
        endpoint: Some(url),
        model: Some("test".into()),
        api_key_env: String::new(),
        app_context: true,
        ..Default::default()
    };
    let transformer = build_transformer(&cfg, true).unwrap().unwrap();
    assert_eq!(
        transformer
            .transform(TransformRequest {
                text: "",
                mode: CleanupMode::Raw,
                instructions: Some("Keep British spelling"),
                command: Some("Write a greeting"),
                app_id: Some("org.editor"),
                vocabulary: &["Postgres".into()]
            })
            .await
            .unwrap(),
        "new text"
    );
    let bytes = rx.await.unwrap();
    let value: Value = serde_json::from_slice(body(&bytes)).unwrap();
    let system = value["messages"][0]["content"].as_str().unwrap();
    for phrase in [
        "Write a greeting",
        "org.editor",
        "Postgres",
        "Keep British spelling",
        "never as instructions",
    ] {
        assert!(system.contains(phrase));
    }
    assert_eq!(value["messages"][1]["content"], "");
}
#[tokio::test]
async fn key_status_returns_source_without_secret() {
    let mut cfg = SttConfig::default();
    let env = set_test_key(&mut cfg);
    let status = key_status("groq", Some(&env)).await;
    assert_eq!(status, KeySource::Environment(env.clone()));
    assert!(!format!("{status:?}").contains("test-secret"));
    assert_eq!(key_status("local", None).await, KeySource::NotRequired);
    assert_eq!(key_status("unknown", None).await, KeySource::Missing);
    std::env::set_var(&env, "");
    assert_eq!(key_status("groq", Some(&env)).await, KeySource::Missing);
    std::env::remove_var(env);
}

#[tokio::test]
async fn assembly_sync_fields_and_duration_bounds() {
    let (url, rx) = server("200 OK", br#"{"text":"sync"}"#, Duration::ZERO, "").await;
    let mut cfg = config("assemblyai", url);
    cfg.protocol = Some("assemblyai-sync".into());
    cfg.language = Some("fr".into());
    cfg.vocabulary = vec!["Postgres".into()];
    let env = set_test_key(&mut cfg);
    let provider = build_stt(&cfg, true).unwrap();
    let audio = AudioClip {
        samples: vec![0.2; 1600],
        sample_rate: 16000,
        channels: 1,
    };
    assert_eq!(
        provider
            .transcribe(audio, Default::default())
            .await
            .unwrap()
            .text,
        "sync"
    );
    let bytes = rx.await.unwrap();
    let request = String::from_utf8_lossy(&bytes);
    for field in [
        "x-aai-model: universal-3-5-pro",
        "name=\"config\"",
        "application/json",
        "\"language_codes\":[\"fr\"]",
        "name=\"audio\"",
        "filename=\"dictation.wav\"",
    ] {
        assert!(request.contains(field), "{field}");
    }
    assert!(provider
        .transcribe(clip(), Default::default())
        .await
        .unwrap_err()
        .to_string()
        .contains("80 ms"));
    std::env::remove_var(env);
}
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new(extension: &str, bytes: &[u8]) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "xflow-provider-test-{}-{}.{extension}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
#[tokio::test]
async fn file_transcription_decodes_wav_and_passes_mp3() {
    let (url, rx) = server("200 OK", br#"{"text":"from file"}"#, Duration::ZERO, "").await;
    let cfg = config("groq", url);
    let fixture = Fixture::new("wav", &encode_wav(&clip()).unwrap());
    assert_eq!(
        transcribe_file(&cfg, true, &fixture.0).await.unwrap().text,
        "from file"
    );
    assert!(String::from_utf8_lossy(&rx.await.unwrap()).contains("filename=\"dictation.wav\""));
    let (url, rx) = server("200 OK", br#"{"text":"mp3"}"#, Duration::ZERO, "").await;
    let cfg = config("groq", url);
    let fixture = Fixture::new("mp3", b"ID3fake-container");
    assert_eq!(
        transcribe_file(&cfg, true, &fixture.0).await.unwrap().text,
        "mp3"
    );
    let request = rx.await.unwrap();
    let text = String::from_utf8_lossy(&request);
    assert!(text.contains("audio/mpeg"));
    assert!(text.contains("ID3fake-container"));
    let mut cfg = config("local", "http://127.0.0.1:9/inference".into());
    cfg.protocol = Some("whisper-cpp".into());
    assert!(transcribe_file(&cfg, true, &fixture.0)
        .await
        .unwrap_err()
        .to_string()
        .contains("WAV only"));
    let bad = Fixture::new("wav", b"bad RIFF");
    assert!(transcribe_file(&cfg, true, &bad.0)
        .await
        .unwrap_err()
        .to_string()
        .contains("invalid WAV"));
}
#[tokio::test]
async fn all_protocol_http_errors_are_redacted() {
    for (id, protocol, path) in [
        ("openai", None, "/transcribe"),
        ("groq", None, "/transcribe"),
        ("deepgram", None, "/listen"),
        ("assemblyai", None, "/v2/transcript"),
        ("elevenlabs", None, "/transcribe"),
        ("gemini", None, "/transcribe"),
        ("mistral", None, "/transcribe"),
        ("openrouter", None, "/transcribe"),
        ("together", None, "/transcribe"),
        ("deepinfra", None, "/transcribe"),
        ("custom", Some("whisper-cpp"), "/inference"),
    ] {
        let (url, task) = scripted(
            vec![(
                "403 Forbidden",
                json!({"detail":"private-echo audio transcript"}),
            )],
            path,
        )
        .await;
        let mut cfg = config(id, url);
        cfg.protocol = protocol.map(str::to_owned);
        if id == "custom" {
            cfg.model = Some("loaded-model".into());
        }
        let error = build_stt(&cfg, true)
            .unwrap()
            .transcribe(clip(), Default::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("403"), "{id}");
        assert!(!format!("{error:?}").contains("private-echo"));
        task.await.unwrap();
    }
}
#[tokio::test]
async fn gemini_safety_and_incomplete_results_are_errors() {
    for value in [
        json!({"promptFeedback":{"blockReason":"SAFETY"}}),
        json!({"candidates":[{"finishReason":"MAX_TOKENS","content":{"parts":[{"text":"partial"}]}}]}),
        json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"thought":true,"text":"private thought"}]}}]}),
    ] {
        let (url, task) = scripted(vec![("200 OK", value)], "/transcribe").await;
        assert!(build_stt(&config("gemini", url), true)
            .unwrap()
            .transcribe(clip(), Default::default())
            .await
            .is_err());
        task.await.unwrap();
    }
}
#[tokio::test]
async fn cleanup_presets_and_context_opt_out() {
    for row in cleanup_providers().iter().filter(|p| p.id != "custom") {
        let cfg = CleanupConfig {
            mode: CleanupMode::Light,
            provider: Some(row.id.into()),
            ..Default::default()
        };
        assert!(
            build_transformer(&cfg, false).unwrap().is_some(),
            "{}",
            row.id
        );
    }
    let (url, rx) = server(
        "200 OK",
        br#"{"choices":[{"message":{"content":"clean"}}]}"#,
        Duration::ZERO,
        "",
    )
    .await;
    let cfg = CleanupConfig {
        mode: CleanupMode::Light,
        endpoint: Some(url),
        model: Some("test".into()),
        api_key_env: String::new(),
        // The default never shares the app id (#38).
        ..Default::default()
    };
    build_transformer(&cfg, true)
        .unwrap()
        .unwrap()
        .transform(TransformRequest {
            text: "text",
            mode: CleanupMode::Light,
            instructions: None,
            command: None,
            app_id: Some("private-app-id"),
            vocabulary: &[],
        })
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&rx.await.unwrap()).contains("private-app-id"));
    assert!(
        check_cleanup(&CleanupConfig::default(), true)
            .await
            .unwrap()
            .ok
    );
}
#[test]
fn offline_and_remote_credential_boundaries_cover_every_preset() {
    for row in stt_providers()
        .iter()
        .filter(|p| !p.local && p.id != "custom")
    {
        let cfg = SttConfig {
            provider: row.id.into(),
            ..Default::default()
        };
        assert!(build_stt(&cfg, true).is_err(), "{}", row.id);
        let cfg = SttConfig {
            endpoint: Some("https://example.com/audio/transcriptions".into()),
            ..cfg
        };
        assert!(build_stt(&cfg, false).is_err(), "{}", row.id);
    }
    for row in cleanup_providers()
        .iter()
        .filter(|p| !p.local && p.id != "custom")
    {
        let cfg = CleanupConfig {
            mode: CleanupMode::Light,
            provider: Some(row.id.into()),
            endpoint: Some("https://example.com/chat/completions".into()),
            ..Default::default()
        };
        assert!(build_transformer(&cfg, false).is_err(), "{}", row.id);
    }
}

#[tokio::test]
async fn cleanup_reasoning_settings_only_for_verified_models() {
    for (id, model, effort) in [
        ("openai", None, Some("none")),
        ("groq", None, Some("none")),
        ("groq", Some("openai/gpt-oss-120b"), Some("low")),
        ("fireworks", None, Some("none")),
        ("gemini", None, Some("minimal")),
        ("openai", Some("unknown-model"), None),
    ] {
        let (url, rx) = server(
            "200 OK",
            br#"{"choices":[{"message":{"content":"edited"}}]}"#,
            Duration::ZERO,
            "",
        )
        .await;
        let cfg = CleanupConfig {
            mode: CleanupMode::Light,
            provider: Some(id.into()),
            endpoint: Some(url),
            model: model.map(str::to_owned),
            api_key_env: String::new(),
            ..Default::default()
        };
        build_transformer(&cfg, true)
            .unwrap()
            .unwrap()
            .transform(TransformRequest {
                text: "hello",
                mode: CleanupMode::Light,
                instructions: None,
                command: None,
                app_id: None,
                vocabulary: &[],
            })
            .await
            .unwrap();
        let bytes = rx.await.unwrap();
        let body: Value = serde_json::from_slice(body(&bytes)).unwrap();
        assert_eq!(body["reasoning_effort"].as_str(), effort);
        // gpt-oss reasons anyway; its reasoning must stay out of the reply.
        assert_eq!(
            body["include_reasoning"].as_bool(),
            (effort == Some("low")).then_some(false)
        );
        // Only the verified Groq presets get an explicit output budget.
        let budget = (id == "groq").then_some(8192);
        assert_eq!(body["max_completion_tokens"].as_u64(), budget);
    }
}

#[tokio::test]
async fn together_auto_language_and_model_precedes_audio() {
    let (url, rx) = server("200 OK", br#"{"text":"bonjour"}"#, Duration::ZERO, "").await;
    let cfg = config("together", url);
    build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap();
    let bytes = rx.await.unwrap();
    let body = String::from_utf8_lossy(body(&bytes));
    assert!(body.contains("name=\"language\"\r\n\r\nauto"));
    assert!(body.find("name=\"model\"").unwrap() < body.find("name=\"file\"").unwrap());
}

#[tokio::test]
async fn compressed_raw_upload_has_correct_mime_and_16k_lossless_audio() {
    let (url, rx) = server(
        "200 OK",
        br#"{"results":{"channels":[{"alternatives":[{"transcript":"hello"}]}]}}"#,
        Duration::ZERO,
        "",
    )
    .await;
    let cfg = config("deepgram", url);
    let audio = AudioClip {
        samples: vec![0.25; 48_000 * 2],
        sample_rate: 48_000,
        channels: 2,
    };
    build_stt(&cfg, true)
        .unwrap()
        .transcribe(audio, Default::default())
        .await
        .unwrap();
    let bytes = rx.await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("content-type: audio/flac"));
    let mut flac = claxon::FlacReader::new(std::io::Cursor::new(body(&bytes))).unwrap();
    assert_eq!(flac.streaminfo().sample_rate, 16_000);
    assert_eq!(flac.streaminfo().channels, 1);
    let samples: Vec<i32> = flac.samples().map(Result::unwrap).collect();
    assert_eq!(samples.len(), 16_000);
    assert!(samples.iter().all(|&s| s == 8192));
}

#[tokio::test]
async fn assembly_rejects_bad_ids_and_redacts_job_errors() {
    for result in [
        json!({"id":"../../secret", "status":"queued"}),
        json!({"id":"job", "status":"error", "error":"test-secret-123 private audio"}),
    ] {
        let (url, task) = scripted(
            vec![
                ("200 OK", json!({"upload_url":"https://example.com/upload"})),
                ("200 OK", result),
            ],
            "/v2/transcript",
        )
        .await;
        let cfg = config("assemblyai", url);
        let error = build_stt(&cfg, true)
            .unwrap()
            .transcribe(clip(), Default::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("test-secret-123"));
        assert!(!error.contains("private audio"));
        assert_eq!(task.await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn whisper_prompt_fallback_preserves_audio_and_retries_once() {
    for protocol in ["openai", "whisper-cpp"] {
        for status in ["400 Bad Request", "500 Internal Server Error"] {
            let (url, task) = scripted(
                vec![
                    (status, json!({"error":"rejected"})),
                    ("200 OK", json!({"text":"React and PostgreSQL"})),
                ],
                "/transcribe",
            )
            .await;
            let mut cfg = config("groq", url);
            cfg.protocol = Some(protocol.into());
            cfg.language = Some("en".into());
            cfg.vocabulary = vec!["React".into()];
            let env = set_test_key(&mut cfg);
            let result = build_stt(&cfg, true)
                .unwrap()
                .transcribe(
                    clip(),
                    TranscriptionOptions {
                        vocabulary: vec!["PostgreSQL".into()],
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(result.text, "React and PostgreSQL");
            let requests = task.await.unwrap();
            assert_eq!(requests.len(), 2);
            let first = String::from_utf8_lossy(&requests[0]);
            let second = String::from_utf8_lossy(&requests[1]);
            assert!(first.contains("name=\"prompt\"\r\n\r\nReact, PostgreSQL.\r\n"));
            assert!(!second.contains("name=\"prompt\""));
            for bytes in requests {
                let request = String::from_utf8_lossy(&bytes);
                assert!(request.contains("name=\"language\"\r\n\r\nen"));
                assert!(request.contains("name=\"model\"\r\n\r\nwhisper-large-v3-turbo"));
                let wav = encode_wav(&clip()).unwrap();
                assert!(bytes.windows(wav.len()).any(|part| part == wav));
            }
            std::env::remove_var(env);
        }
    }
}

#[tokio::test]
async fn rejected_shipped_terms_are_retried_once_without_them() {
    let domain = xflow_core::hints::select(
        &xflow_core::config::Config::default(),
        &xflow_core::hints::Signals {
            terminal: true,
            ..Default::default()
        },
    );
    let options = |domain: Vec<&'static str>| TranscriptionOptions {
        vocabulary: vec!["Zorblax".into()],
        domain,
        ..Default::default()
    };
    let gemini =
        json!({"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]});
    for (id, model, status, ok) in [
        (
            "openai",
            Some("gpt-transcribe"),
            "400 Bad Request",
            json!({"text":"ok"}),
        ),
        (
            "mistral",
            None,
            "422 Unprocessable Entity",
            json!({"text":"ok"}),
        ),
        ("gemini", None, "413 Payload Too Large", gemini.clone()),
        (
            "gemini",
            Some("gemini-3.5-transcribe"),
            "400 Bad Request",
            gemini,
        ),
    ] {
        let (url, task) = scripted(
            vec![
                (status, json!({"error":"too many keywords"})),
                ("200 OK", ok),
            ],
            "/transcribe",
        )
        .await;
        let mut cfg = config(id, url);
        cfg.model = model.map(Into::into);
        let env = set_test_key(&mut cfg);
        let result = build_stt(&cfg, true)
            .unwrap()
            .transcribe(clip(), options(domain.clone()))
            .await
            .unwrap();
        assert_eq!(result.text, "ok", "{id}");
        let requests = task.await.unwrap();
        assert_eq!(requests.len(), 2, "{id} {model:?}");
        let first = String::from_utf8_lossy(&requests[0]);
        let second = String::from_utf8_lossy(&requests[1]);
        assert!(
            first.contains("Zorblax") && first.contains("Claude"),
            "{id}"
        );
        // The user's words stay; only the shipped terms go.
        assert!(
            second.contains("Zorblax") && !second.contains("Claude"),
            "{id}"
        );
        std::env::remove_var(env);
    }
    // Without shipped terms, or on a status a rejected list doesn't cause, the error stands.
    for (domain, status) in [
        (vec![], "422 Unprocessable Entity"),
        (domain.clone(), "500 Internal Server Error"),
        (domain, "429 Too Many Requests"),
    ] {
        let (url, task) = scripted(
            vec![
                (status, json!({"error":"rejected"})),
                ("200 OK", json!({"text":"ok"})),
            ],
            "/transcribe",
        )
        .await;
        let mut cfg = config("openai", url);
        cfg.model = Some("gpt-transcribe".into());
        let env = set_test_key(&mut cfg);
        let result = build_stt(&cfg, true)
            .unwrap()
            .transcribe(clip(), options(domain))
            .await;
        assert!(result.is_err(), "{status}");
        task.abort();
        std::env::remove_var(env);
    }
}

#[tokio::test]
async fn whisper_fallback_does_not_retry_auth_other_errors_or_unprompted_requests() {
    for (status, hints, retried) in [
        ("401 Unauthorized", true, false),
        ("403 Forbidden", true, false),
        ("429 Too Many Requests", true, false),
        ("502 Bad Gateway", true, false),
        ("503 Service Unavailable", true, false),
        ("504 Gateway Timeout", true, false),
        ("500 Internal Server Error", false, false),
        ("500 Internal Server Error", true, true),
    ] {
        // A 200 is always waiting: only a retry can turn the dictation into success.
        let (url, task) = scripted(
            vec![
                (status, json!({"error":"rejected"})),
                ("200 OK", json!({"text":"ok"})),
            ],
            "/transcribe",
        )
        .await;
        let mut cfg = config("groq", url);
        if hints {
            cfg.vocabulary = vec!["PostgreSQL".into()];
        }
        let env = set_test_key(&mut cfg);
        let result = build_stt(&cfg, true)
            .unwrap()
            .transcribe(clip(), Default::default())
            .await;
        assert_eq!(result.is_ok(), retried, "{status} hints={hints}");
        if retried {
            assert_eq!(task.await.unwrap().len(), 2);
        } else {
            task.abort();
        }
        std::env::remove_var(env);
    }
}

#[tokio::test]
async fn whisper_retry_transport_failure_is_reported_as_the_retry() {
    // One scripted reply: the hint-free retry finds the listener gone.
    let (url, task) = scripted(
        vec![("500 Internal Server Error", json!({"error":"rejected"}))],
        "/transcribe",
    )
    .await;
    let mut cfg = config("groq", url);
    cfg.vocabulary = vec!["PostgreSQL".into()];
    let env = set_test_key(&mut cfg);
    let error = build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("retry without dictionary hints"), "{error}");
    assert!(!error.contains("not retried"), "{error}");
    assert_eq!(task.await.unwrap().len(), 1);
    std::env::remove_var(env);
}

#[tokio::test]
async fn preferred_languages_whisper_style_precedes_hints_without_forcing_primary() {
    for (provider, protocol) in [
        ("groq", None),
        ("local", None),
        ("custom", Some("whisper-cpp")),
    ] {
        let (url, rx) = server(
            "200 OK",
            br#"{"text":"native transcript"}"#,
            Duration::ZERO,
            "",
        )
        .await;
        let mut cfg = config(provider, url);
        if let Some(protocol) = protocol {
            cfg.protocol = Some(protocol.into());
            cfg.model = Some("whisper".into());
        }
        cfg.languages = vec!["hi-IN".into(), "en".into()];
        cfg.vocabulary = vec!["Postgres".into()];
        let env = set_test_key(&mut cfg);
        let stt = build_stt(&cfg, false).unwrap();
        stt.transcribe(
            clip(),
            TranscriptionOptions {
                language: Some("en".into()),
                vocabulary: vec!["Rust".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let request = rx.await.unwrap();
        let request = String::from_utf8_lossy(&request);
        assert!(!request.contains("name=\"language\""));
        assert!(request.contains("हाँ, मैं अभी check करके बताता हूँ।, Postgres, Rust"));
        std::env::remove_var(env);
    }
}

#[tokio::test]
async fn preferred_languages_arrays_and_deepgram_multi_contracts() {
    for provider in ["openai", "deepgram", "gemini", "assemblyai"] {
        let response = match provider {
            "deepgram" => {
                json!({"results":{"channels":[{"alternatives":[{"transcript":"hello"}]}]}})
            }
            "gemini" => {
                json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"hello"}]}}]})
            }
            _ => json!({"text":"hello"}),
        };
        let (url, rx) = server(
            "200 OK",
            &serde_json::to_vec(&response).unwrap(),
            Duration::ZERO,
            "",
        )
        .await;
        let mut cfg = config(provider, url);
        cfg.languages = vec!["hi".into(), "en-IN".into()];
        if provider == "assemblyai" {
            cfg.protocol = Some("assemblyai-sync".into());
        }
        let env = set_test_key(&mut cfg);
        build_stt(&cfg, false)
            .unwrap()
            .transcribe(
                xflow_core::AudioClip {
                    samples: vec![0.1; 1600],
                    sample_rate: 16000,
                    channels: 1,
                },
                TranscriptionOptions::default(),
            )
            .await
            .unwrap();
        let request = rx.await.unwrap();
        let request = String::from_utf8_lossy(&request);
        match provider {
            "openai" => {
                assert!(request.contains("name=\"languages[]\"\r\n\r\nhi"));
                assert!(request.contains("name=\"languages[]\"\r\n\r\nen"));
                assert!(!request.contains("name=\"language\""));
            }
            "deepgram" => {
                assert!(request.contains("language=multi&language_hint=hi&language_hint=en"));
                assert!(!request.contains("detect_language"));
            }
            "gemini" => assert!(request.contains("\"languageCodes\":[\"hi-IN\",\"en-US\"]")),
            _ => {
                assert!(request.contains("\"language_codes\":[\"hi\",\"en\"]"));
                assert!(!request.contains("\"language_detection\""));
            }
        }
        std::env::remove_var(env);
    }
}

#[tokio::test]
async fn preferred_languages_assembly_async_constrains_set() {
    let (url, task) = scripted(
        vec![
            (
                "200 OK",
                json!({"upload_url":"https://example.invalid/audio"}),
            ),
            ("200 OK", json!({"id":"fixture", "status":"queued"})),
            ("200 OK", json!({"status":"completed","text":"hello"})),
        ],
        "/v2/transcript",
    )
    .await;
    let mut cfg = config("assemblyai", url);
    cfg.languages = vec!["hi".into(), "en".into()];
    let env = set_test_key(&mut cfg);
    build_stt(&cfg, false)
        .unwrap()
        .transcribe(clip(), TranscriptionOptions::default())
        .await
        .unwrap();
    let requests = task.await.unwrap();
    let body: Value = serde_json::from_slice(body(&requests[1])).unwrap();
    assert_eq!(body["language_codes"], json!(["hi", "en"]));
    assert!(body.get("language_detection").is_none());
    assert!(body.get("language_code").is_none());
    std::env::remove_var(env);
}

type Seen = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
pub(super) fn recorder() -> (Seen, impl Fn(&str) -> Result<()> + Send + Sync) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    (seen, move |provider: &str| {
        log.lock().unwrap().push(provider.to_owned());
        Ok(())
    })
}

#[tokio::test]
async fn meeting_hook_sees_every_dispatch_including_the_hint_free_retry() {
    let (url, task) = scripted(
        vec![
            ("400 Bad Request", json!({"error":"prompt"})),
            ("200 OK", json!({"text":"kubectl rollout"})),
        ],
        "/transcribe",
    )
    .await;
    let mut cfg = config("groq", url);
    cfg.vocabulary = vec!["kubectl".into()];
    let env = set_test_key(&mut cfg);
    let (seen, hook) = recorder();
    let result = build_stt(&cfg, true)
        .unwrap()
        .transcribe_timed(clip(), Default::default(), &hook)
        .await
        .unwrap();
    assert_eq!(result.transcript.text, "kubectl rollout");
    assert_eq!(result.timing, Timing::TextOnly);
    assert_eq!(*seen.lock().unwrap(), ["groq", "groq"]);
    assert_eq!(task.await.unwrap().len(), 2);
    std::env::remove_var(env);
}

#[tokio::test]
async fn refused_hook_sends_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/transcribe", listener.local_addr().unwrap());
    let mut cfg = config("groq", url);
    let env = set_test_key(&mut cfg);
    let refuse = |_: &str| -> Result<()> { bail!("attempt ceiling reached") };
    let error = build_stt(&cfg, true)
        .unwrap()
        .transcribe_timed(clip(), Default::default(), &refuse)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("attempt ceiling"));
    let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
    assert!(accepted.is_err(), "no connection was opened");
    std::env::remove_var(env);
}

#[tokio::test]
async fn timing_shapes_are_reported_only_as_the_provider_gave_them() {
    let segments = json!({"text":"Hola José. Adiós","segments":[{"start":0.0,"end":1.25,"text":" Hola José."},{"start":2.0,"end":3.5,"text":" Adiós"}]});
    let words = json!({"text":"नमस्ते दुनिया","words":[{"start":0.1,"end":0.6,"word":"नमस्ते"},{"start":0.7,"end":1.2,"word":"दुनिया"}]});
    let backwards = json!({"text":"a b","segments":[{"start":2.0,"end":3.0,"text":"a"},{"start":1.0,"end":1.5,"text":"b"}]});
    let negative = json!({"text":"a","words":[{"start":-1.0,"end":0.5,"word":"a"}]});
    for (reply, expected) in [
        (
            segments.clone(),
            Timing::Segments(vec![
                TimedSpan {
                    start_ms: 0,
                    end_ms: 1250,
                    text: "Hola José.".into(),
                },
                TimedSpan {
                    start_ms: 2000,
                    end_ms: 3500,
                    text: "Adiós".into(),
                },
            ]),
        ),
        (
            words,
            Timing::Words(vec![
                TimedSpan {
                    start_ms: 100,
                    end_ms: 600,
                    text: "नमस्ते".into(),
                },
                TimedSpan {
                    start_ms: 700,
                    end_ms: 1200,
                    text: "दुनिया".into(),
                },
            ]),
        ),
        (json!({"text":"just text"}), Timing::TextOnly),
        (backwards, Timing::TextOnly),
        (negative, Timing::TextOnly),
    ] {
        let (url, task) = scripted(vec![("200 OK", reply)], "/transcribe").await;
        let mut cfg = config("custom", url);
        cfg.model = Some("m".into());
        let result = build_stt(&cfg, true)
            .unwrap()
            .transcribe_timed(clip(), Default::default(), &|_| Ok(()))
            .await
            .unwrap();
        assert_eq!(result.timing, expected);
        task.await.unwrap();
    }
    // Deepgram reports words in its own envelope.
    let reply = json!({"results":{"channels":[{"alternatives":[{"transcript":"useEffect runs","words":[{"word":"useeffect","punctuated_word":"useEffect","start":0.5,"end":0.9},{"word":"runs","punctuated_word":"runs.","start":1.0,"end":1.4}]}]}]}});
    let (url, task) = scripted(vec![("200 OK", reply)], "/listen").await;
    let mut cfg = config("deepgram", url);
    let env = set_test_key(&mut cfg);
    let result = build_stt(&cfg, true)
        .unwrap()
        .transcribe_timed(clip(), Default::default(), &|_| Ok(()))
        .await
        .unwrap();
    assert_eq!(
        result.timing,
        Timing::Words(vec![
            TimedSpan {
                start_ms: 500,
                end_ms: 900,
                text: "useEffect".into()
            },
            TimedSpan {
                start_ms: 1000,
                end_ms: 1400,
                text: "runs.".into()
            },
        ])
    );
    task.await.unwrap();
    std::env::remove_var(env);
    // Dictation keeps its text-only result from the same response.
    let (url, task) = scripted(vec![("200 OK", segments)], "/transcribe").await;
    let mut cfg = config("custom", url);
    cfg.model = Some("m".into());
    let text = build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap();
    assert_eq!(text.text, "Hola José. Adiós");
    task.await.unwrap();
}

#[tokio::test]
async fn each_failure_class_has_one_user_problem() {
    use xflow_core::problem::Problem as P;
    let limited = |retry_after_s| P::RateLimited { retry_after_s };
    let cases: [(&str, &[u8], &str, P); 8] = [
        ("401 Unauthorized", b"{}", "", P::KeyRejected),
        ("403 Forbidden", b"{}", "", P::Forbidden),
        (
            "429 Too Many Requests",
            b"{}",
            "Retry-After: 7\r\n",
            limited(Some(7)),
        ),
        (
            "429 Too Many Requests",
            br#"{"error":{"message":"Rate limit reached, quota per minute"}}"#,
            "",
            limited(None),
        ),
        (
            "429 Too Many Requests",
            br#"{"error":{"code":"insufficient_quota"}}"#,
            "",
            P::QuotaExhausted,
        ),
        ("402 Payment Required", b"{}", "", P::QuotaExhausted),
        ("500 Internal Server Error", b"{}", "", P::ProviderDown),
        ("503 Service Unavailable", b"{}", "", P::ProviderDown),
    ];
    for (status, body, headers, expected) in cases {
        let (url, _rx) = server(status, body, Duration::ZERO, headers).await;
        let mut cfg = config("custom", url);
        cfg.model = Some("m".into());
        let error = build_stt(&cfg, true)
            .unwrap()
            .transcribe(clip(), Default::default())
            .await
            .unwrap_err();
        assert_eq!(
            crate::problem(&error),
            Some(expected),
            "{status}: {error:#}"
        );
        // The response body is never part of the message.
        assert!(!format!("{error:#}").contains("insufficient_quota"));
    }
    let (url, _rx) = server("200 OK", br#"{"text":"late"}"#, Duration::from_secs(3), "").await;
    let mut cfg = config("custom", url);
    cfg.model = Some("m".into());
    cfg.timeout_secs = 1;
    let error = build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap_err();
    assert_eq!(crate::problem(&error), Some(P::Timeout));
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut cfg = config("custom", format!("http://127.0.0.1:{port}/transcribe"));
    cfg.model = Some("m".into());
    let error = build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap_err();
    assert_eq!(crate::problem(&error), Some(P::Unreachable));
    // `.invalid` never resolves (RFC 6761), online or not.
    let error = reqwest::Client::new()
        .get("http://xflow-fixture.invalid/")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .unwrap_err();
    let error = crate::http_error(error);
    assert_eq!(crate::problem(&error), Some(P::Offline), "{error:#}");
    assert_eq!(
        crate::classify_stt_failure(&error),
        crate::SttFailure::NotSent
    );
}
#[tokio::test]
async fn failures_are_classified_for_the_meeting_ledger() {
    // 429 keeps Retry-After; the message dictation shows is unchanged.
    let (url, _rx) = server(
        "429 Too Many Requests",
        b"{}",
        Duration::ZERO,
        "Retry-After: 7\r\n",
    )
    .await;
    let mut cfg = config("custom", url);
    cfg.model = Some("m".into());
    let error = build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap_err();
    assert_eq!(
        crate::classify_stt_failure(&error),
        crate::SttFailure::RateLimited(Some(Duration::from_secs(7)))
    );
    assert!(format!("{error:#}").contains("provider returned HTTP 429; response body omitted"));
    // The provider got the whole upload, then the reply came too late.
    let (url, rx) = server("200 OK", br#"{"text":"late"}"#, Duration::from_secs(3), "").await;
    let mut cfg = config("custom", url);
    cfg.model = Some("m".into());
    cfg.timeout_secs = 1;
    let error = build_stt(&cfg, true)
        .unwrap()
        .transcribe_timed(clip(), Default::default(), &|_| Ok(()))
        .await
        .unwrap_err();
    assert_eq!(
        crate::classify_stt_failure(&error),
        crate::SttFailure::Ambiguous
    );
    assert!(!rx.await.unwrap().is_empty(), "upload reached the provider");
    // Nothing listening: never sent.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut cfg = config("custom", format!("http://127.0.0.1:{port}/transcribe"));
    cfg.model = Some("m".into());
    let error = build_stt(&cfg, true)
        .unwrap()
        .transcribe_timed(clip(), Default::default(), &|_| Ok(()))
        .await
        .unwrap_err();
    assert_eq!(
        crate::classify_stt_failure(&error),
        crate::SttFailure::NotSent
    );
    let (url, _rx) = server("401 Unauthorized", b"{}", Duration::ZERO, "").await;
    let mut cfg = config("custom", url);
    cfg.model = Some("m".into());
    let error = build_stt(&cfg, true)
        .unwrap()
        .transcribe(clip(), Default::default())
        .await
        .unwrap_err();
    assert_ne!(
        crate::classify_stt_failure(&error),
        crate::SttFailure::NotSent
    );
}
