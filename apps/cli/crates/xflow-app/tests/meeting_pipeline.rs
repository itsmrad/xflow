//! Meeting audio journal, job ledger and STT through the real HTTP provider
//! against a loopback stub. Scratch directories only; no network, no daemon.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, Once,
    },
    time::Duration,
};
use xflow_app::meeting::{
    jobs,
    journal::{self, Capture, CaptureSpec, Limits, PIECE_SAMPLES},
    transcribe::{self, Report},
    MeetingReader, MeetingStore,
};
use xflow_core::{config::SttConfig, meeting::MeetingId, SpeechToText};

fn init_sqlite() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        xflow_app::store::skip_bulk_page_cache();
    });
}

/// One scripted reply per request: (status line, extra headers, JSON body, delay).
type Reply = (&'static str, String, Value, Duration);
struct Stub {
    url: String,
    requests: Arc<AtomicUsize>,
}
fn stub(handler: impl Fn(usize, &[u8]) -> Reply + Send + Sync + 'static) -> Stub {
    let handler = Arc::new(handler);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/audio/transcriptions",
        listener.local_addr().unwrap()
    );
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { return };
            let (handler, count) = (handler.clone(), count.clone());
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let mut buf = [0u8; 65536];
                let body = loop {
                    let Ok(n) = socket.read(&mut buf) else {
                        break None;
                    };
                    if n == 0 {
                        break None;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..at]).to_ascii_lowercase();
                        let size: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if bytes.len() >= at + 4 + size {
                            break Some(bytes[at + 4..].to_vec());
                        }
                    }
                };
                let Some(body) = body else { return };
                let n = count.fetch_add(1, Ordering::SeqCst);
                let (status, headers, value, delay) = handler(n, &body);
                std::thread::sleep(delay);
                let body = value.to_string();
                let _ = socket.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n{headers}Connection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            });
        }
    });
    Stub { url, requests }
}
fn ok(text: &str) -> Reply {
    (
        "200 OK",
        String::new(),
        json!({ "text": text }),
        Duration::ZERO,
    )
}
fn provider(url: &str, timeout_secs: u64) -> Arc<dyn SpeechToText> {
    xflow_providers::build_stt(
        &SttConfig {
            provider: "custom".into(),
            model: Some("stub-model".into()),
            endpoint: Some(url.into()),
            timeout_secs,
            ..SttConfig::default()
        },
        true,
    )
    .unwrap()
}

struct Scratch {
    _dir: tempfile::TempDir,
    db: PathBuf,
}
fn scratch() -> Scratch {
    init_sqlite();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("xflow");
    xflow_app::paths::private_dir(&data).unwrap();
    Scratch {
        db: data.join("meetings.db"),
        _dir: dir,
    }
}
fn limits() -> Limits {
    Limits {
        session_bytes: 1 << 30,
        global_bytes: 2 << 30,
        reserve_bytes: 0,
    }
}
/// Deterministic, nonsilent, different per source and position.
fn samples(source: usize, from: usize, count: usize) -> Vec<i16> {
    (from..from + count)
        .map(|i| (((i * (source + 3)) % 2000) as i16 - 1000) * 8)
        .collect()
}
fn capture(db: &Path, seconds: &[usize]) -> MeetingId {
    let mut capture = Capture::begin(
        db,
        CaptureSpec {
            title: Some("Pipeline test".into()),
            labels: seconds.iter().map(|_| "fixture.wav".into()).collect(),
            expected_ms: Some(*seconds.iter().max().unwrap() as i64 * 1000),
            queued: false,
        },
        limits(),
    )
    .unwrap();
    for (source, &secs) in seconds.iter().enumerate() {
        let total = secs * 16_000;
        let mut at = 0;
        while at < total {
            let n = 12_345.min(total - at);
            capture.push(source, &samples(source, at, n)).unwrap();
            at += n;
        }
    }
    capture.finish().unwrap()
}
async fn process(db: &Path, meeting: &MeetingId, stt: &dyn SpeechToText) -> Report {
    let _lock = transcribe::lock_worker(db, false).unwrap().unwrap();
    journal::recover(db).unwrap();
    let (_tx, rx) = tokio::sync::watch::channel(false);
    transcribe::run(db, &[meeting.id], stt, || async { false }, rx, &mut |_| {})
        .await
        .unwrap()
}
fn open_ro(db: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
}
fn sql(db: &Path, query: &str) -> i64 {
    open_ro(db)
        .unwrap()
        .query_row(query, [], |r| r.get(0))
        .unwrap()
}
fn audio_files(db: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in std::fs::read_dir(journal::spool_root(db))
        .into_iter()
        .flatten()
        .flatten()
    {
        for file in std::fs::read_dir(dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            if file.file_name().to_string_lossy().ends_with(".pcm") {
                out.push(file.path());
            }
        }
    }
    out.sort();
    out
}
fn texts(db: &Path, meeting: &MeetingId) -> Vec<(String, i64, i64, String)> {
    let reader = MeetingReader::at(db.to_path_buf());
    let mut out = Vec::new();
    let mut start = 0;
    while start < 3 * 3_600_000 + 60_000 {
        let mut cursor = None;
        loop {
            let page = reader
                .transcript(&xflow_core::meeting::TranscriptQuery {
                    meeting: meeting.clone(),
                    range: xflow_core::meeting::TimeRange {
                        start_ms: start,
                        end_ms: start + 600_000,
                    },
                    snapshot: None,
                    cursor: cursor.clone(),
                })
                .unwrap();
            for s in &page.segments {
                if s.range.start_ms >= start {
                    out.push((
                        s.source_id.clone(),
                        s.range.start_ms,
                        s.range.end_ms,
                        s.text.clone(),
                    ));
                }
            }
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        start += 600_000;
    }
    out
}

#[tokio::test]
async fn paired_audio_becomes_timestamped_text_and_audio_is_deleted() {
    let s = scratch();
    let stub = stub(|n, _| ok(&format!("part {n} of the standup")));
    let meeting = capture(&s.db, &[70, 40]);
    // 70 s -> 14 pieces, 40 s -> 8 pieces; windows 3 + 2.
    assert_eq!(audio_files(&s.db).len(), 22);
    assert_eq!(sql(&s.db, "SELECT count(*) FROM jobs"), 5);
    let report = process(&s.db, &meeting, provider(&stub.url, 5).as_ref()).await;
    assert_eq!((report.accepted, report.failed, report.waiting), (5, 0, 0));
    assert_eq!(stub.requests.load(Ordering::SeqCst), 5);
    let stored = texts(&s.db, &meeting);
    let spans: Vec<_> = stored.iter().map(|t| (t.0.as_str(), t.1, t.2)).collect();
    assert_eq!(
        spans,
        [
            ("audio1", 0, 30_000),
            ("audio2", 0, 30_000),
            ("audio1", 28_000, 60_000),
            ("audio2", 28_000, 40_000),
            ("audio1", 58_000, 70_000)
        ]
    );
    assert!(audio_files(&s.db).is_empty(), "accepted audio is deleted");
    assert_eq!(sql(&s.db, "SELECT count(*) FROM pieces"), 0);
    let status = jobs::status(&s.db, Some(meeting.id), limits(), journal::now_ms()).unwrap();
    assert_eq!(status.meetings[0].windows.accepted, 5);
    assert_eq!(status.meetings[0].pending_audio.pieces, 0);
    let found = MeetingReader::at(s.db.clone())
        .search("standup", None)
        .unwrap();
    assert_eq!(found.segments.len(), 5);
    let get = MeetingReader::at(s.db.clone()).status(&meeting).unwrap();
    assert_eq!(
        get.summary.provenance,
        xflow_core::meeting::Provenance::TranscribedAudio
    );
    assert_eq!(get.sources[0].provider.as_deref(), Some("custom"));
    assert_eq!(get.sources[0].model.as_deref(), Some("stub-model"));
}

#[tokio::test]
async fn overlap_audio_waits_for_the_next_window_and_failed_audio_survives_restart() {
    let s = scratch();
    // The second window is refused by the provider (final), the others succeed.
    let stub = stub(|n, _| {
        if n == 1 {
            ("400 Bad Request", String::new(), json!({}), Duration::ZERO)
        } else {
            ok(&format!("window text {n}"))
        }
    });
    let meeting = capture(&s.db, &[70]);
    let report = process(&s.db, &meeting, provider(&stub.url, 5).as_ref()).await;
    assert_eq!((report.accepted, report.failed), (2, 1));
    // Window 2 failed: its pieces 6..11 stay, and piece 5 (25-30 s), which
    // window 2 reads as overlap, stays too. Window 3 needs 11 as overlap.
    let left: Vec<String> = audio_files(&s.db)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        (5..=11)
            .map(|i| format!("s0-{i:06}.pcm"))
            .collect::<Vec<_>>()
    );
    // A restart (recovery) keeps them; status shows the failure.
    journal::recover(&s.db).unwrap();
    assert_eq!(audio_files(&s.db).len(), 7);
    let status = jobs::status(&s.db, Some(meeting.id), limits(), journal::now_ms()).unwrap();
    assert_eq!(status.meetings[0].windows.failed, 1);
    assert_eq!(status.meetings[0].pending_audio.seconds, 35);
    // Explicit retry with three fresh attempts: everything completes and goes.
    jobs::reopen(&s.db, Some(meeting.id)).unwrap();
    let report = process(&s.db, &meeting, provider(&stub.url, 5).as_ref()).await;
    assert_eq!((report.accepted, report.failed), (1, 0));
    assert!(audio_files(&s.db).is_empty());
    assert_eq!(texts(&s.db, &meeting).len(), 3);
}

#[tokio::test]
async fn attempt_ceiling_rate_limits_timeouts_and_empty_results() {
    let s = scratch();
    // Request order: w1 429 (Retry-After 1); w2 times out after the provider
    // has the audio; w3 text; w1 empty text; w2 503, 503 -> ceiling.
    let stub = stub(|n, _| match n {
        0 => (
            "429 Too Many Requests",
            "Retry-After: 1\r\n".into(),
            json!({}),
            Duration::ZERO,
        ),
        1 => ok("late").into_delayed(Duration::from_millis(2_500)),
        2 => ok("useEffect runs after render"),
        3 => ok(""),
        _ => (
            "503 Service Unavailable",
            String::new(),
            json!({}),
            Duration::ZERO,
        ),
    });
    let meeting = capture(&s.db, &[70]);
    let started = std::time::Instant::now();
    let report = process(&s.db, &meeting, provider(&stub.url, 1).as_ref()).await;
    assert_eq!(report.accepted, 2, "{report:?}");
    assert_eq!(report.empty, 1);
    assert_eq!(report.failed, 1);
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "Retry-After honored"
    );
    // Windows run in time order while earlier ones wait: window 1 is sent
    // twice (429, then empty), window 2 three times, window 3 once.
    assert_eq!(stub.requests.load(Ordering::SeqCst), 6);
    assert_eq!(
        sql(
            &s.db,
            "SELECT count(*) FROM attempts WHERE state='ambiguous'"
        ),
        1
    );
    assert_eq!(sql(&s.db, "SELECT max(attempts) FROM jobs"), 3);
    let stored = texts(&s.db, &meeting);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].3, "useEffect runs after render");
    // Window 3 failed: its audio and the shared overlap stay.
    assert!(!audio_files(&s.db).is_empty());
    let status = jobs::status(&s.db, Some(meeting.id), limits(), journal::now_ms()).unwrap();
    let m = &status.meetings[0];
    assert_eq!((m.windows.empty, m.windows.failed), (1, 1));
    assert!(m.last_error.as_deref().unwrap_or("").contains("503"));
}
trait Delayed {
    fn into_delayed(self, delay: Duration) -> Reply;
}
impl Delayed for Reply {
    fn into_delayed(self, delay: Duration) -> Reply {
        (self.0, self.1, self.2, delay)
    }
}

#[tokio::test]
async fn missing_provider_is_unavailable_and_sends_nothing() {
    let s = scratch();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let meeting = capture(&s.db, &[12]);
    let stt = provider(
        &format!("http://127.0.0.1:{port}/v1/audio/transcriptions"),
        2,
    );
    let report = process(&s.db, &meeting, stt.as_ref()).await;
    assert!(report.unavailable.is_some());
    assert_eq!(report.accepted + report.failed, 0);
    assert_eq!(
        sql(&s.db, "SELECT attempts FROM jobs"),
        0,
        "refused connection is not an attempt"
    );
    assert_eq!(
        sql(&s.db, "SELECT count(*) FROM jobs WHERE state='pending'"),
        1
    );
    assert_eq!(audio_files(&s.db).len(), 3);
}

#[tokio::test]
async fn cancellation_keeps_audio_and_counts_the_request() {
    let s = scratch();
    let stub = stub(|_, _| ok("slow").into_delayed(Duration::from_secs(2)));
    let meeting = capture(&s.db, &[10]);
    let stt = provider(&stub.url, 5);
    let _lock = transcribe::lock_worker(&s.db, false).unwrap().unwrap();
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        tx.send(true).unwrap();
    });
    let report = transcribe::run(
        &s.db,
        &[meeting.id],
        stt.as_ref(),
        || async { false },
        rx,
        &mut |_| {},
    )
    .await
    .unwrap();
    assert!(report.cancelled);
    assert_eq!(
        sql(
            &s.db,
            "SELECT count(*) FROM attempts WHERE state='ambiguous'"
        ),
        1
    );
    assert_eq!(sql(&s.db, "SELECT count(*) FROM segments"), 0);
    assert_eq!(audio_files(&s.db).len(), 2);
}

#[tokio::test]
async fn delete_during_processing_drops_the_result_and_removes_audio() {
    let s = scratch();
    let meeting = capture(&s.db, &[40]);
    let db = s.db.clone();
    let target = meeting.clone();
    let deleted = Arc::new(Mutex::new(false));
    let flag = deleted.clone();
    // The provider deletes the meeting while it "works", as another process would.
    let stub = stub(move |_, _| {
        let mut done = flag.lock().unwrap();
        if !*done {
            MeetingStore::at(db.clone()).delete(&target).unwrap();
            *done = true;
        }
        ok("this text must never be stored")
    });
    let report = process(&s.db, &meeting, provider(&stub.url, 5).as_ref()).await;
    assert!(*deleted.lock().unwrap());
    assert_eq!(report.accepted, 0);
    assert_eq!(report.dropped, 1);
    assert_eq!(sql(&s.db, "SELECT count(*) FROM segments"), 0);
    assert_eq!(sql(&s.db, "SELECT count(*) FROM jobs"), 0);
    assert!(audio_files(&s.db).is_empty());
    assert!(!journal::spool_root(&s.db)
        .join(format!("{}-{}", meeting.store_id, meeting.id))
        .exists());
    // A recovery afterwards resurrects nothing.
    journal::recover(&s.db).unwrap();
    assert_eq!(sql(&s.db, "SELECT count(*) FROM meetings"), 0);
    assert_eq!(
        stub.requests.load(Ordering::SeqCst),
        1,
        "the second window was never sent"
    );
}

#[tokio::test]
async fn corrupt_torn_and_orphan_pieces_are_recovered_or_quarantined() {
    let s = scratch();
    let meeting = capture(&s.db, &[40]);
    let files = audio_files(&s.db);
    assert_eq!(files.len(), 8);
    let dir = files[0].parent().unwrap().to_path_buf();
    // Bad checksum in piece 1, corrupt header in piece 2.
    let mut bytes = std::fs::read(&files[1]).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x55;
    std::fs::write(&files[1], &bytes).unwrap();
    let mut bytes = std::fs::read(&files[2]).unwrap();
    bytes[3] ^= 1;
    std::fs::write(&files[2], &bytes).unwrap();
    // A stray file with a piece name but foreign content is quarantined.
    std::fs::write(dir.join("s0-000099.pcm"), b"not audio").unwrap();
    let recovered = journal::recover(&s.db).unwrap();
    assert_eq!(recovered.quarantined, 1);
    let stub = stub(|n, _| ok(&format!("text {n}")));
    let report = process(&s.db, &meeting, provider(&stub.url, 5).as_ref()).await;
    assert_eq!(report.accepted, 2);
    let gaps: Vec<String> = MeetingReader::at(s.db.clone())
        .status(&meeting)
        .unwrap()
        .gaps
        .iter()
        .map(|g| format!("{}-{}", g.range.start_ms, g.range.end_ms))
        .collect();
    assert_eq!(
        gaps,
        ["5000-10000", "10000-15000"],
        "damaged pieces are known gaps"
    );
    let quarantine: Vec<_> = std::fs::read_dir(dir.join("quarantine"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(quarantine.len(), 3, "damaged pieces are kept, not deleted");
    let status = jobs::status(&s.db, None, limits(), journal::now_ms()).unwrap();
    assert_eq!(status.spool.quarantined_files, 3);
    assert!(status.warnings.iter().any(|w| w.contains("quarantine")));
}

#[tokio::test]
async fn quotas_refuse_or_stop_visibly_and_never_delete() {
    let s = scratch();
    // One finished, untranscribed meeting already holds audio.
    let first = capture(&s.db, &[10]);
    let before = audio_files(&s.db);
    let piece = (88 + PIECE_SAMPLES * 2) as u64;
    // Refused before anything is created: needs 3 pieces, 1 is free.
    let tight = Limits {
        session_bytes: 1 << 30,
        global_bytes: piece * 3,
        reserve_bytes: 0,
    };
    let refused = Capture::begin(
        &s.db,
        CaptureSpec {
            title: None,
            labels: vec!["x.wav".into()],
            expected_ms: Some(15_000),
            queued: false,
        },
        tight,
    );
    let error = format!("{:#}", refused.err().unwrap());
    assert!(error.contains("limit"), "{error}");
    assert_eq!(sql(&s.db, "SELECT count(*) FROM meetings"), 1);
    // Live audio (unknown length) stops visibly at the limit; earlier pieces stay.
    let mut capture = Capture::begin(
        &s.db,
        CaptureSpec {
            title: None,
            labels: vec!["mic".into()],
            expected_ms: None,
            queued: false,
        },
        Limits {
            global_bytes: piece * 5,
            ..tight
        },
    )
    .unwrap();
    let mut stopped = None;
    for i in 0..10 {
        if let Err(e) = capture.push(0, &samples(0, i * PIECE_SAMPLES, PIECE_SAMPLES)) {
            stopped = Some(format!("{e:#}"));
            break;
        }
    }
    assert!(stopped.unwrap().contains("limit"));
    assert!(capture.warnings.iter().any(|w| w.contains("80%")));
    let second = capture.id();
    drop(capture);
    for path in &before {
        assert!(path.exists(), "quota never evicts older audio");
    }
    assert_eq!(audio_files(&s.db).len(), 2 + 3);
    let status = jobs::status(&s.db, Some(second.id), limits(), journal::now_ms()).unwrap();
    assert_eq!(status.meetings[0].capture, "stopped");
    // The stopped meeting's saved audio is still transcribed.
    let stub = stub(|n, _| ok(&format!("kept {n}")));
    process(&s.db, &second, provider(&stub.url, 5).as_ref()).await;
    assert_eq!(texts(&s.db, &second).len(), 1);
    assert!(before.iter().all(|p| p.exists()));
    let _ = first;
}

#[tokio::test]
async fn audio_waiting_a_day_warns_and_is_never_expired() {
    let s = scratch();
    let meeting = capture(&s.db, &[12]);
    let later = journal::now_ms() + 25 * 3_600_000;
    let status = jobs::status(&s.db, Some(meeting.id), limits(), later).unwrap();
    let m = &status.meetings[0];
    assert!(m.pending_audio.age_ms.unwrap() >= jobs::WARN_AFTER_MS);
    assert!(m.warnings.iter().any(|w| w.contains("24 hours")));
    let early = jobs::status(&s.db, Some(meeting.id), limits(), journal::now_ms()).unwrap();
    assert!(early.meetings[0].warnings.is_empty());
    // Status, recovery and repeated restarts never remove it.
    for _ in 0..3 {
        journal::recover(&s.db).unwrap();
    }
    assert_eq!(audio_files(&s.db).len(), 3);
}

#[test]
fn v1_database_migrates_on_first_pipeline_use() {
    init_sqlite();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("xflow");
    xflow_app::paths::private_dir(&data).unwrap();
    let db = data.join("meetings.db");
    // A v2 store still reads and imports M1 transcripts.
    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/meeting-notes/mixed-script.json"),
    )
    .unwrap();
    let imported = MeetingStore::at(db.clone()).import_json(&fixture).unwrap();
    assert_eq!(sql(&db, "PRAGMA user_version"), 3);
    let summary = MeetingReader::at(db.clone())
        .status(&imported.meeting)
        .unwrap();
    assert_eq!(
        summary.summary.provenance,
        xflow_core::meeting::Provenance::ImportedUnverified
    );
    let status = jobs::status(&db, None, limits(), 0).unwrap();
    assert!(status.meetings.is_empty());
}

/// Varied sentences through the stub: storage, dedup and search must keep
/// them exactly. Not a speech-recognition quality measurement.
#[tokio::test]
async fn varied_language_text_is_stored_and_searchable_exactly() {
    let cases: &[(&str, &str, &str)] = &[
        ("easy", "Let's meet at three tomorrow.", "three tomorrow"),
        ("long", "After the quarterly review we agreed that the onboarding flow should be shortened, the pricing page rewritten, and the support rota extended to cover the weekend in Asia.", "support rota"),
        ("tongue twister", "She sells seashells by the seashore, and the shells she sells are surely seashells.", "seashore"),
        ("developer", "Run kubectl rollout restart, then git rebase onto main; the useEffect hook re-fetches the PostgreSQL rows over OAuth.", "git rebase"),
        ("numbers", "Revenue grew 17.5% to $4,280,000 in Q3 2026, versus 3.2 million in 2025.", "4,280,000"),
        ("names", "Priya Raghunathan, Oluwaseun Adeyemi, Siobhán Ní Bhriain, Nguyễn Thị Minh and Zhang Wei joined.", "Siobhán"),
        ("hindi", "कल की मीटिंग में हमने बजट पर चर्चा की।", "बजट"),
        ("hindi-english", "Deadline Friday hai, toh please PR aaj merge kar do, warna release slip ho jayega.", "merge kar do"),
        ("japanese", "来週の月曜日に東京オフィスで新しいデザインを確認します。", "東京オフィス"),
        ("spanish", "¿Podemos mover la reunión al jueves? José no llega hasta las diez.", "jueves"),
        ("portuguese", "A atualização do aplicativo foi publicada ontem à noite, sem regressões.", "atualização"),
        ("punctuation", "Wait — really?! \"Ship it,\" she said; (then) we did… finally.", "Ship it"),
    ];
    let s = scratch();
    let replies: Vec<String> = cases.iter().map(|c| c.1.to_owned()).collect();
    let stub = stub(move |n, _| ok(replies.get(n).map(String::as_str).unwrap_or("")));
    // 12 windows on one source: 6 minutes.
    let meeting = capture(&s.db, &[360]);
    let report = process(&s.db, &meeting, provider(&stub.url, 5).as_ref()).await;
    assert_eq!(report.accepted, 12);
    let stored = texts(&s.db, &meeting);
    let reader = MeetingReader::at(s.db.clone());
    let mut lines = Vec::new();
    for (i, (category, sentence, term)) in cases.iter().enumerate() {
        let actual = stored.get(i).map(|t| t.3.as_str()).unwrap_or("");
        let found = reader
            .search(term, None)
            .unwrap()
            .segments
            .iter()
            .any(|seg| seg.text == *sentence);
        lines.push(format!(
            "{category}: stored {} search {}",
            if actual == *sentence {
                "exact"
            } else {
                "DIFFERENT"
            },
            if found { "found" } else { "MISSING" }
        ));
        assert_eq!(actual, *sentence, "{category}");
        assert!(found, "{category}: {term}");
    }
    println!("{}", lines.join("\n"));
}

/// About 3 hours of two-source audio, generated as it streams, with the
/// worker draining jobs concurrently. Accelerated: no realtime claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "accelerated 3 h run; run explicitly"]
async fn accelerated_three_hours_of_paired_audio() {
    let s = scratch();
    let stub = stub(|n, _| ok(&format!("synthetic window {n}")));
    let stt = provider(&stub.url, 5);
    let total = 3 * 3600 * 16_000usize;
    let started = std::time::Instant::now();
    let db = s.db.clone();
    let writer = std::thread::spawn(move || {
        let mut capture = Capture::begin(
            &db,
            CaptureSpec {
                title: Some("3 h synthetic".into()),
                labels: vec!["mic".into(), "remote".into()],
                expected_ms: Some(3 * 3_600_000),
                queued: false,
            },
            limits(),
        )
        .unwrap();
        let id = capture.id();
        let mut at = 0;
        let mut written = 0u64;
        // 20 ms blocks alternating sources, like a recorder.
        while at < total {
            for source in 0..2 {
                capture.push(source, &samples(source, at, 320)).unwrap();
            }
            at += 320;
            written += 2 * 640;
        }
        capture.finish().unwrap();
        (id, written)
    });
    let mut meeting = None;
    let mut max_pieces = 0;
    let mut report = Report::default();
    loop {
        let finished = writer.is_finished();
        let id: Option<i64> = open_ro(&s.db)
            .ok()
            .and_then(|c| {
                c.query_row("SELECT max(id) FROM meetings", [], |r| r.get(0))
                    .ok()
            })
            .flatten();
        if let Some(id) = id {
            meeting = Some(id);
            let (_tx, rx) = tokio::sync::watch::channel(false);
            let _lock = transcribe::lock_worker(&s.db, false).unwrap().unwrap();
            let r = transcribe::run(
                &s.db,
                &[id],
                stt.as_ref(),
                || async { false },
                rx,
                &mut |_| {},
            )
            .await
            .unwrap();
            report.accepted += r.accepted;
            report.failed += r.failed;
            max_pieces = max_pieces.max(sql(&s.db, "SELECT count(*) FROM pieces"));
        }
        if finished {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (id, written) = writer.join().unwrap();
    assert_eq!(Some(id.id), meeting);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let r = transcribe::run(
        &s.db,
        &[id.id],
        stt.as_ref(),
        || async { false },
        rx,
        &mut |_| {},
    )
    .await
    .unwrap();
    report.accepted += r.accepted;
    let wall = started.elapsed();
    let rss = std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("VmHWM"))
        .unwrap_or("VmHWM: unknown")
        .to_owned();
    assert_eq!(report.accepted, 720);
    assert_eq!(report.failed, 0);
    assert_eq!(sql(&s.db, "SELECT count(*) FROM segments"), 720);
    assert_eq!(sql(&s.db, "SELECT count(*) FROM pieces"), 0);
    assert_eq!(
        sql(&s.db, "SELECT count(*) FROM gaps"),
        0,
        "no overlap was deleted early"
    );
    assert!(audio_files(&s.db).is_empty());
    println!(
        "ACCELERATED 3h two-source: wall {:.1}s, {rss}, PCM bytes streamed {written}, max retained pieces {max_pieces}, requests {}",
        wall.as_secs_f64(),
        stub.requests.load(Ordering::SeqCst)
    );
}

/// Live capture: window 1 is accepted before window 2 exists. Its last piece
/// is window 2's overlap and must survive until window 2 is accepted.
#[tokio::test]
async fn overlap_survives_while_capture_is_still_running() {
    let s = scratch();
    let stub = stub(|n, _| ok(&format!("live {n}")));
    let mut capture = Capture::begin(
        &s.db,
        CaptureSpec {
            title: None,
            labels: vec!["mic".into()],
            expected_ms: None,
            queued: true,
        },
        limits(),
    )
    .unwrap();
    let meeting = capture.id();
    capture.push(0, &samples(0, 0, 480_000)).unwrap();
    let stt = provider(&stub.url, 5);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let report = transcribe::run(
        &s.db,
        &[meeting.id],
        stt.as_ref(),
        || async { false },
        rx,
        &mut |_| {},
    )
    .await
    .unwrap();
    assert_eq!(report.accepted, 1);
    let left: Vec<String> = audio_files(&s.db)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        ["s0-000005.pcm"],
        "the 25-30 s piece is window 2's overlap"
    );
    capture.push(0, &samples(0, 480_000, 160_000)).unwrap();
    capture.finish().unwrap();
    let (_tx, rx) = tokio::sync::watch::channel(false);
    transcribe::run(
        &s.db,
        &[meeting.id],
        stt.as_ref(),
        || async { false },
        rx,
        &mut |_| {},
    )
    .await
    .unwrap();
    assert!(audio_files(&s.db).is_empty());
    assert_eq!(sql(&s.db, "SELECT count(*) FROM gaps"), 0);
    assert_eq!(texts(&s.db, &meeting).len(), 2);
}

fn assert_tiny_tail_retained(count: usize) {
    let s = scratch();
    let wav = s._dir.path().join("original.wav");
    let mut writer = hound::WavWriter::create(
        &wav,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for sample in samples(0, 0, count) {
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
    let original = std::fs::read(&wav).unwrap();
    let mut capture = Capture::begin(
        &s.db,
        CaptureSpec {
            title: None,
            labels: vec!["original.wav".into()],
            expected_ms: None,
            queued: false,
        },
        limits(),
    )
    .unwrap();
    let pcm = hound::WavReader::open(&wav)
        .unwrap()
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    capture.push(0, &pcm).unwrap();
    let meeting = capture.finish().unwrap();
    // Simulate the first 30 s having been durably accepted; the fresh 50 ms was not.
    let db = Connection::open(&s.db).unwrap();
    db.execute(
        "UPDATE jobs SET state='accepted' WHERE win=0 AND end_sample>=480000",
        [],
    )
    .unwrap();
    drop(db);
    for _ in 0..2 {
        journal::recover(&s.db).unwrap();
    }
    assert!(
        !audio_files(&s.db).is_empty(),
        "{count} samples lost untranscribed audio"
    );
    assert_eq!(
        sql(
            &s.db,
            "SELECT count(*) FROM pieces WHERE start_sample+samples>480000 OR start_sample=0"
        ),
        1
    );
    assert_eq!(std::fs::read(&wav).unwrap(), original);
    let status = jobs::status(&s.db, Some(meeting.id), limits(), journal::now_ms()).unwrap();
    assert!(status.meetings[0].pending_audio.bytes > 0);
    assert_eq!(status.meetings[0].windows.retained, 1);
    assert_eq!(status.meetings[0].windows.abandoned, 0);
}

#[test]
fn fresh_30_second_wav_tail_survives_recovery() {
    assert_tiny_tail_retained(30 * 16_000 + 800);
}

#[test]
fn tiny_wav_tails_survive_recovery_without_accepted_text() {
    assert_tiny_tail_retained(800);
}

fn assert_legacy_unlink_intent_retained(job_state: &str) {
    let s = scratch();
    let mut capture = Capture::begin(
        &s.db,
        CaptureSpec {
            title: None,
            labels: vec!["tiny.wav".into()],
            expected_ms: None,
            queued: false,
        },
        limits(),
    )
    .unwrap();
    capture.push(0, &samples(0, 0, 800)).unwrap();
    let meeting = capture.finish().unwrap();
    let files = audio_files(&s.db);
    assert_eq!(files.len(), 1);
    let bytes = std::fs::read(&files[0]).unwrap();
    let db = Connection::open(&s.db).unwrap();
    // Old release committed this intent before a crash, without accepted text.
    db.execute(
        "UPDATE jobs SET state=?1,error='under 100 ms; not sent'",
        [job_state],
    )
    .unwrap();
    db.execute("UPDATE pieces SET state='unlink_pending'", [])
        .unwrap();
    drop(db);
    for _ in 0..2 {
        journal::recover(&s.db).unwrap();
        assert_eq!(
            std::fs::read(&files[0]).expect("legacy unprocessed tail deleted"),
            bytes
        );
        assert_eq!(
            sql(&s.db, "SELECT count(*) FROM pieces WHERE state='ready'"),
            1
        );
        assert_eq!(
            sql(&s.db, "SELECT count(*) FROM jobs WHERE state='retained'"),
            1
        );
        assert!(texts(&s.db, &meeting).is_empty());
    }
    // Explicit user deletion still removes retained private audio.
    let store = MeetingStore::at(s.db.clone());
    store.delete(&meeting).unwrap();
    journal::recover(&s.db).unwrap();
    assert!(!files[0].exists());
}

#[test]
fn legacy_abandoned_tail_with_unlink_intent_is_retained() {
    assert_legacy_unlink_intent_retained("abandoned");
}

#[test]
fn legacy_already_upgraded_tail_with_unlink_intent_is_retained() {
    assert_legacy_unlink_intent_retained("retained");
}

/// A live source that stops delivering keeps its later audio at the right
/// time: the missing range is a gap, never compressed or invented audio.
#[tokio::test]
async fn live_gap_keeps_later_audio_in_place_and_is_reported() {
    let s = scratch();
    let stub = stub(|n, _| ok(&format!("live part {n}")));
    let mut capture = Capture::begin(
        &s.db,
        CaptureSpec {
            title: Some("Live".into()),
            labels: vec!["Microphone".into()],
            expected_ms: None,
            queued: true,
        },
        limits(),
    )
    .unwrap();
    let meeting = capture.id();
    capture.push(0, &samples(0, 0, 10 * 16_000)).unwrap();
    // Still silent at the next check: the same gap grows, no second row.
    for _ in 0..2 {
        capture
            .gap(0, 10 * 16_000, "no audio arrived from this source")
            .unwrap();
    }
    assert_eq!(capture.position(0), 30 * 16_000);
    capture
        .push(0, &samples(0, 30 * 16_000, 25 * 16_000))
        .unwrap();
    capture.finish().unwrap();
    let gaps: Vec<String> = open_ro(&s.db)
        .unwrap()
        .prepare("SELECT json FROM gaps")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    let gap: Value = serde_json::from_str(&gaps[0]).unwrap();
    assert_eq!(gap["range"], json!({"start_ms": 10_000, "end_ms": 30_000}));
    let starts: Vec<i64> = open_ro(&s.db)
        .unwrap()
        .prepare("SELECT start_sample FROM pieces ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        starts[2],
        30 * 16_000,
        "audio after the gap keeps its time: {starts:?}"
    );
    let report = process(&s.db, &meeting, provider(&stub.url, 5).as_ref()).await;
    assert_eq!(report.failed, 0);
    let stored = texts(&s.db, &meeting);
    assert_eq!(stored.last().unwrap().2, 55_000, "{stored:?}");
    assert!(
        audio_files(&s.db).is_empty(),
        "audio goes only after its text is stored"
    );
}
