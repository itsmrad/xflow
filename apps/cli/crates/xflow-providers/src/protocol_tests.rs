use super::tests::{body, clip, config, server, set_test_key};
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
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
async fn scripted(
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
        app_context: false,
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
