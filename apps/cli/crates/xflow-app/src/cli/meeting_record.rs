//! `xflow meeting record` and `meeting stop`: a live meeting written into the
//! private journal by this process, never by xflowd. The microphone comes
//! from the daemon's shared lease, so dictation keeps working; the app's
//! audio comes from `xflow meeting-native`, a child that shows consent and the
//! Stop indicator first. If that child dies, recording stops (fail-closed).
use super::{meeting, output::Output, Error, Result};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
use xflow_app::meeting::journal::{self, Capture, CaptureSpec, Limits, RATE};

/// Live meetings stop here; the journal holds at most three hours.
const MAX_LIVE: Duration = Duration::from_secs(3 * 3600);
/// A source this far behind the wall clock stopped delivering: a gap, not silence.
const STALL_SAMPLES: i64 = 2 * RATE;
const STALL: Duration = Duration::from_secs(2);

pub struct Request<'a> {
    pub app: Option<&'a str>,
    pub title: Option<&'a str>,
    pub mic: bool,
    pub yes: bool,
    pub control_stdin: bool,
}

enum Msg {
    /// Audio and helper frames carry when they arrived, so a slow loop never
    /// mistakes queued audio for a stalled source.
    Native(u8, Vec<u8>, Instant),
    NativeGone,
    Mic(xflow_app::meeting_mic::Frame, Instant),
    MicGone(String),
    Stop(&'static str),
}

pub fn record(req: &Request, config: &Path, out: &Output) -> Result<()> {
    if let Some(title) = req.title {
        if title.is_empty() || title.len() > 256 {
            return Err(meeting::usage("Title must be 1 to 256 bytes"));
        }
    }
    let (tx, rx) = mpsc::sync_channel::<Msg>(4096);
    // Before any thread starts, so only the signal thread sees these.
    signals(tx.clone())?;
    let db = meeting::db_path()?;
    let root = journal::spool_root(&db);
    xflow_app::paths::private_dir(&root)?;
    let lock_path = root.join("recorder.lock");
    let mut lock = journal::lock_file(&lock_path)?;
    if fs2::FileExt::try_lock_exclusive(&lock).is_err() {
        return Err(Error::new(
            1,
            "A meeting is already recording",
            "Stop it with the Xflow indicator or xflow meeting stop",
        ));
    }
    lock.set_len(0)?;
    write!(lock, "{}", std::process::id())?;
    if req.control_stdin {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut byte), Ok(1)) && byte[0] != b'q' {}
            let _ = tx.send(Msg::Stop("stopped by the meeting detector"));
        });
    }
    let mut native = spawn_native(req, tx.clone())?;
    // Consent first: nothing opens until the user chose Record.
    let consent = loop {
        match rx.recv() {
            Ok(Msg::Native(b'C', body, _)) => break body,
            Ok(Msg::Native(b'D', _, _)) | Ok(Msg::Stop(_)) => {
                native.quit();
                out.text("Not recorded")?;
                return Ok(());
            }
            Ok(Msg::Native(b'E', body, _)) => {
                native.quit();
                return Err(Error::new(
                    1,
                    String::from_utf8_lossy(&body).into_owned(),
                    "Run xflow meeting record --help",
                ));
            }
            Ok(Msg::NativeGone) | Err(_) => {
                return Err(Error::new(
                    1,
                    "The recording window closed before it started",
                    "Run xflow meeting record again",
                ));
            }
            Ok(_) => {}
        }
    };
    let app: serde_json::Value = serde_json::from_slice(&consent).unwrap_or_default();
    let app_name = app["app"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let mic = if req.mic {
        match xflow_app::meeting_mic::Client::acquire() {
            Ok(client) => Some(client),
            Err(e) => {
                native.quit();
                return Err(Error::new(
                    1,
                    format!("Could not share the microphone: {e:#}"),
                    "Start the daemon (xflow daemon start), or record without it: --no-mic",
                ));
            }
        }
    } else {
        None
    };
    let mut labels = Vec::new();
    if mic.is_some() {
        labels.push("Microphone".to_owned());
    }
    if let Some(name) = &app_name {
        labels.push(name.clone());
    }
    let mut capture = Capture::begin(
        &db,
        CaptureSpec {
            title: req.title.map(str::to_owned),
            labels,
            expected_ms: None,
            queued: true,
        },
        Limits::from_env(),
    )
    .map_err(|e| {
        native.quit();
        Error::new(
            1,
            format!("Meeting not recorded: {e:#}"),
            "Run xflow meeting status",
        )
    })?;
    let id = capture.id();
    let mut client = mic;
    if let Some(client) = &client {
        let mut reader = client.try_clone()?;
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            match reader.recv() {
                Ok(frame) => {
                    let end = matches!(frame, xflow_app::meeting_mic::Frame::End(_));
                    if tx.send(Msg::Mic(frame, Instant::now())).is_err() || end {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::MicGone(format!("{e:#}")));
                    return;
                }
            }
        });
    }
    native.send(b's');
    let started = Instant::now();
    if !out.json && !out.quiet {
        eprintln!(
            "xflow: recording meeting {}:{}. Stop with the Xflow indicator, Ctrl-C or xflow meeting stop.",
            id.store_id, id.id
        );
    }
    let sources = capture_sources(client.is_some(), app_name.is_some());
    let mut tracks = [Track::default(), Track::default()];
    let mut reason: Option<String> = None;
    let mut failed = false;
    let mut stopping: Option<Instant> = None;
    let (mut mic_done, mut native_done) = (client.is_none(), false);
    let mut nudged = Instant::now();
    while !(mic_done && native_done) {
        let msg = rx.recv_timeout(Duration::from_millis(250));
        let at = match &msg {
            Ok(Msg::Mic(_, at)) | Ok(Msg::Native(_, _, at)) => *at,
            _ => Instant::now(),
        };
        let result: anyhow::Result<()> = match msg {
            Ok(Msg::Mic(
                xflow_app::meeting_mic::Frame::Audio {
                    rate,
                    lost,
                    samples,
                },
                at,
            )) => match sources.mic {
                Some(s) => tracks[s].push(&mut capture, s, rate, lost, &samples, at),
                None => Ok(()),
            },
            Ok(Msg::Mic(xflow_app::meeting_mic::Frame::End(why), _)) => {
                mic_done = true;
                if why != "released" {
                    stop_with(
                        format!("microphone ended: {why}"),
                        true,
                        &mut reason,
                        &mut failed,
                    );
                }
                Ok(())
            }
            Ok(Msg::MicGone(why)) => {
                mic_done = true;
                stop_with(
                    format!("the xflow daemon stopped sharing the microphone ({why})"),
                    true,
                    &mut reason,
                    &mut failed,
                );
                Ok(())
            }
            Ok(Msg::Native(b'A', body, at)) if body.len() >= 12 => match sources.app {
                Some(s) => {
                    let rate = u32::from_le_bytes(body[0..4].try_into().unwrap());
                    let lost = u64::from_le_bytes(body[4..12].try_into().unwrap());
                    let samples: Vec<f32> = body[12..]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b))
                        .collect();
                    tracks[s].push(&mut capture, s, rate, lost, &samples, at)
                }
                None => Ok(()),
            },
            Ok(Msg::Native(b'R', _, _)) => {
                if !out.json && !out.quiet {
                    eprintln!(
                        "xflow: capturing {} audio",
                        app_name.as_deref().unwrap_or("app")
                    );
                }
                Ok(())
            }
            Ok(Msg::Native(b'S', body, _)) => {
                stop_with(
                    String::from_utf8_lossy(&body).into_owned(),
                    false,
                    &mut reason,
                    &mut failed,
                );
                Ok(())
            }
            Ok(Msg::Native(b'E', body, _)) => {
                stop_with(
                    String::from_utf8_lossy(&body).into_owned(),
                    true,
                    &mut reason,
                    &mut failed,
                );
                Ok(())
            }
            Ok(Msg::Native(b'X', _, _)) | Ok(Msg::NativeGone) => {
                native_done = true;
                // Fail-closed: no visible Stop indicator, no recording.
                stop_with(
                    "the Xflow indicator closed".into(),
                    stopping.is_none(),
                    &mut reason,
                    &mut failed,
                );
                Ok(())
            }
            Ok(Msg::Stop(why)) => {
                stop_with(why.into(), false, &mut reason, &mut failed);
                Ok(())
            }
            Ok(Msg::Native(..)) | Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Err(e) = result {
            // The journal stopped itself and kept every durable piece.
            stop_with(format!("{e:#}"), true, &mut reason, &mut failed);
            break;
        }
        let elapsed = started.elapsed();
        if elapsed >= MAX_LIVE {
            stop_with(
                "the 3 hour meeting limit was reached".into(),
                false,
                &mut reason,
                &mut failed,
            );
        }
        if stopping.is_none() {
            let now = (at.saturating_duration_since(started).as_millis() as i64) * RATE / 1000;
            for s in [sources.mic, sources.app].into_iter().flatten() {
                if let Err(e) = tracks[s].catch_up(&mut capture, s, now, at) {
                    stop_with(format!("{e:#}"), true, &mut reason, &mut failed);
                }
            }
            if nudged.elapsed() >= Duration::from_secs(30) {
                nudged = Instant::now();
                meeting::transcribe_quietly(Some(&id), config);
            }
        }
        if reason.is_some() && stopping.is_none() {
            stopping = Some(Instant::now());
            native.quit_async();
            if let Some(c) = &client {
                if c.release().is_err() {
                    mic_done = true;
                }
            }
        }
        if stopping.is_some_and(|t| t.elapsed() > Duration::from_secs(5)) {
            break;
        }
    }
    native.wait();
    client.take();
    let reason = reason.unwrap_or_else(|| "stopped".into());
    let closed = (|| -> anyhow::Result<_> {
        for s in [sources.mic, sources.app].into_iter().flatten() {
            tracks[s].flush(&mut capture, s)?;
        }
        Ok(())
    })();
    let closed = match closed {
        Ok(()) if !failed => capture.finish(),
        _ => capture.fail(&reason),
    };
    closed.map_err(|e| {
        Error::new(
            1,
            format!("Meeting audio was only partly stored: {e:#}"),
            "Run xflow meeting status",
        )
    })?;
    drop(lock);
    let seconds = started.elapsed().as_secs();
    let transcribing = meeting::transcribe_quietly(Some(&id), config);
    out.data(
        &serde_json::json!({
            "ok": !failed,
            "meeting": id,
            "seconds": seconds,
            "stopped": reason,
            "transcribing": transcribing,
        }),
        &format!(
            "Meeting {}:{} recorded ({seconds} s; {reason}). {}",
            id.store_id,
            id.id,
            if transcribing {
                "Transcribing in the background; see xflow meeting status"
            } else {
                "The audio is kept; run xflow meeting retry once transcription is set up"
            }
        ),
    )?;
    if failed {
        return Err(Error::new(
            1,
            format!(
                "Recording stopped early: {}. What was recorded is kept",
                xflow_app::meeting::display_text(&reason)
            ),
            "Run xflow meeting status",
        ));
    }
    Ok(())
}

/// `xflow meeting stop`: ask the running recorder to finish, then wait for it.
pub fn stop(out: &Output) -> Result<()> {
    let db = meeting::db_path()?;
    let lock_path = journal::spool_root(&db).join("recorder.lock");
    let Ok(mut lock) = std::fs::File::open(&lock_path) else {
        out.text("No meeting is recording")?;
        return Ok(());
    };
    if fs2::FileExt::try_lock_shared(&lock).is_ok() {
        out.text("No meeting is recording")?;
        return Ok(());
    }
    let mut pid = String::new();
    lock.read_to_string(&mut pid)?;
    let pid: i32 = pid.trim().parse().map_err(|_| {
        Error::new(
            1,
            "The recorder's lock is unreadable",
            "Use the Xflow indicator's Stop button",
        )
    })?;
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(60);
    while fs2::FileExt::try_lock_shared(&lock).is_err() {
        if Instant::now() > deadline {
            return Err(Error::new(
                1,
                "The recorder did not finish in a minute",
                "Run xflow meeting status",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(out.text("Meeting recording stopped")?)
}

/// The hidden helper: `xflow meeting-native`. Runs the native code and exits.
#[cfg(target_os = "macos")]
pub fn native(app: Option<&str>, mic: bool, yes: bool) -> Result<()> {
    unsafe extern "C" {
        fn xflow_meeting_native(app: *const std::ffi::c_char, mic: i32, yes: i32, mute: i32)
            -> i32;
    }
    // Muting what is tapped is a test-only knob: automated runs must not play
    // speech through the speakers. Release builds always leave it audible.
    let mute = cfg!(feature = "test-support") && std::env::var_os("XFLOW_TEST_MUTE_TAP").is_some();
    let app = app
        .map(std::ffi::CString::new)
        .transpose()
        .map_err(|_| meeting::usage("Invalid app"))?;
    let code = unsafe {
        xflow_meeting_native(
            app.as_ref().map_or(std::ptr::null(), |a| a.as_ptr()),
            mic.into(),
            yes.into(),
            mute.into(),
        )
    };
    std::process::exit(code)
}

fn stop_with(why: String, bad: bool, reason: &mut Option<String>, failed: &mut bool) {
    if reason.is_none() {
        *reason = Some(why);
        *failed = bad;
    }
}

struct Sources {
    mic: Option<usize>,
    app: Option<usize>,
}
fn capture_sources(mic: bool, app: bool) -> Sources {
    Sources {
        mic: mic.then_some(0),
        app: app.then_some(usize::from(mic)),
    }
}

/// One source's native audio, cut into whole seconds and resampled with the
/// same filter as dictation uploads.
#[derive(Default)]
struct Track {
    rate: u32,
    pending: Vec<f32>,
    seen: bool,
    /// When this source last delivered audio.
    last: Option<Instant>,
    /// Samples already gapped by `catch_up` since the last delivery, so a
    /// drop count for the same stall is not gapped twice.
    gapped: i64,
    /// Dropped native frames not yet a whole output sample.
    lost: u64,
}
impl Track {
    fn push(
        &mut self,
        capture: &mut Capture,
        source: usize,
        rate: u32,
        lost: u64,
        samples: &[f32],
        at: Instant,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            (8_000..=192_000).contains(&rate),
            "invalid audio rate {rate}"
        );
        if rate != self.rate {
            self.flush(capture, source)?;
            self.rate = rate;
            self.lost = 0;
        }
        if lost > 0 && self.seen {
            self.lost += lost;
            let whole = self.lost * RATE as u64 / u64::from(rate);
            self.lost -= whole * u64::from(rate) / RATE as u64;
            let missing = whole as i64 - self.gapped;
            if missing > 0 {
                self.flush(capture, source)?;
                capture.gap(source, missing, "audio frames were dropped")?;
            }
        }
        self.seen = true;
        self.last = Some(at);
        self.gapped = 0;
        self.pending.extend(samples.iter().map(|s| {
            if s.is_finite() {
                s.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        }));
        let second = rate as usize;
        while self.pending.len() >= second {
            let rest = self.pending.split_off(second);
            let chunk = std::mem::replace(&mut self.pending, rest);
            capture.push(source, &pcm16(&chunk, rate)?)?;
        }
        Ok(())
    }
    fn flush(&mut self, capture: &mut Capture, source: usize) -> anyhow::Result<()> {
        if !self.pending.is_empty() {
            let chunk = std::mem::take(&mut self.pending);
            capture.push(source, &pcm16(&chunk, self.rate)?)?;
        }
        Ok(())
    }
    /// A stalled source (device change, a frozen tap) becomes an honest gap
    /// up to now, so its later audio stays at the right time.
    fn catch_up(
        &mut self,
        capture: &mut Capture,
        source: usize,
        now: i64,
        at: Instant,
    ) -> anyhow::Result<()> {
        // Only a source that delivered nothing for 2 s has stalled; audio
        // still queued behind a slow write is not missing.
        if !self
            .last
            .is_some_and(|l| at.saturating_duration_since(l) > STALL)
        {
            return Ok(());
        }
        let buffered = self.pending.len() as i64 * RATE / i64::from(self.rate.max(1));
        let behind = now - capture.position(source) - buffered;
        if behind > STALL_SAMPLES {
            self.flush(capture, source)?;
            capture.gap(source, behind, "no audio arrived from this source")?;
            self.gapped += behind;
        }
        Ok(())
    }
}
fn pcm16(samples: &[f32], rate: u32) -> anyhow::Result<Vec<i16>> {
    let mono = if rate == RATE as u32 {
        samples.to_vec()
    } else {
        xflow_providers::upload_audio(&xflow_core::AudioClip {
            samples: samples.to_vec(),
            sample_rate: rate,
            channels: 1,
        })?
        .samples
    };
    Ok(mono
        .iter()
        .map(|&s| {
            let s = if s.is_finite() {
                s.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            (s * if s < 0.0 { 32768.0 } else { 32767.0 }).round() as i16
        })
        .collect())
}

struct Native {
    child: Child,
    stdin: Option<ChildStdin>,
}
impl Native {
    fn send(&mut self, byte: u8) {
        if let Some(stdin) = &mut self.stdin {
            let _ = stdin.write_all(&[byte]).and_then(|_| stdin.flush());
        }
    }
    fn quit_async(&mut self) {
        self.send(b'q');
        self.stdin.take();
    }
    fn wait(&mut self) {
        self.quit_async();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    fn quit(&mut self) {
        self.wait();
    }
}
fn spawn_native(req: &Request, tx: mpsc::SyncSender<Msg>) -> Result<Native> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("meeting-native");
    if let Some(app) = req.app {
        command.arg("--app").arg(app);
    }
    if !req.mic {
        command.arg("--no-mic");
    }
    if req.yes {
        command.arg("--yes");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| {
            Error::new(
                1,
                format!("Could not open the recording window: {e}"),
                "Run xflow doctor",
            )
        })?;
    let mut stdout = child.stdout.take().expect("piped");
    std::thread::spawn(move || loop {
        let mut head = [0u8; 5];
        if stdout.read_exact(&mut head).is_err() {
            let _ = tx.send(Msg::NativeGone);
            return;
        }
        let len = u32::from_le_bytes(head[1..5].try_into().unwrap()) as usize;
        let mut body = vec![0; len.min(1 << 20)];
        if len > 1 << 20 || stdout.read_exact(&mut body).is_err() {
            let _ = tx.send(Msg::NativeGone);
            return;
        }
        if tx.send(Msg::Native(head[0], body, Instant::now())).is_err() {
            return;
        }
    });
    Ok(Native {
        stdin: child.stdin.take(),
        child,
    })
}

/// SIGINT, SIGTERM and SIGHUP finish the meeting like the Stop button.
fn signals(tx: mpsc::SyncSender<Msg>) -> Result<()> {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(&mut set, s);
        }
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(Error::new(
                1,
                "Could not set up Ctrl-C handling",
                "Run xflow meeting record again",
            ));
        }
        std::thread::spawn(move || {
            let mut got = 0;
            libc::sigwait(&set, &mut got);
            let _ = tx.send(Msg::Stop(match got {
                libc::SIGTERM => "stopped with xflow meeting stop",
                libc::SIGHUP => "the terminal closed",
                _ => "stopped with Ctrl-C",
            }));
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_number_in_label_order() {
        let s = capture_sources(true, true);
        assert_eq!((s.mic, s.app), (Some(0), Some(1)));
        let s = capture_sources(false, true);
        assert_eq!((s.mic, s.app), (None, Some(0)));
    }

    fn gaps(db: &Path) -> Vec<(i64, i64)> {
        let db = rusqlite::Connection::open(db).unwrap();
        let mut statement = db.prepare("SELECT json FROM gaps ORDER BY rowid").unwrap();
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|j| {
                let v: serde_json::Value = serde_json::from_str(&j.unwrap()).unwrap();
                (
                    v["range"]["start_ms"].as_i64().unwrap(),
                    v["range"]["end_ms"].as_i64().unwrap(),
                )
            })
            .collect();
        rows
    }

    #[test]
    fn queued_audio_is_not_a_stall_but_silence_from_a_source_is() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("xflow");
        xflow_app::paths::private_dir(&data).unwrap();
        let db = data.join("meetings.db");
        let mut capture = Capture::begin(
            &db,
            CaptureSpec {
                title: None,
                labels: vec!["Microphone".into()],
                expected_ms: None,
                queued: false,
            },
            Limits {
                session_bytes: 1 << 30,
                global_bytes: 1 << 30,
                reserve_bytes: 0,
            },
        )
        .unwrap();
        let t0 = Instant::now();
        let mut track = Track::default();
        let second = vec![0.1f32; 48_000];
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let now = |ms: i64| ms * RATE / 1000;
        // Three seconds that arrived on time but were processed late: no gap.
        for s in 0..3u64 {
            track
                .push(&mut capture, 0, 48_000, 0, &second, at(s * 1000 + 1000))
                .unwrap();
            track
                .catch_up(
                    &mut capture,
                    0,
                    now(s as i64 * 1000 + 1000),
                    at(s * 1000 + 1000),
                )
                .unwrap();
        }
        assert!(gaps(&db).is_empty());
        // The source then delivers nothing for 5 s: one gap up to now.
        track
            .catch_up(&mut capture, 0, now(8_000), at(8_000))
            .unwrap();
        assert_eq!(gaps(&db), [(3_000, 8_000)]);
        // A drop count for that same stall is not gapped again.
        track
            .push(&mut capture, 0, 48_000, 5 * 48_000, &second, at(9_000))
            .unwrap();
        assert_eq!(gaps(&db).len(), 1);
        assert_eq!(
            capture.position(0),
            now(9_000),
            "8 s plus the second that arrived"
        );
        // Single dropped frames add up to a sample instead of vanishing:
        // 3 x 480 frames is 480 samples, plus 1 for the 3 dropped frames.
        for _ in 0..3 {
            track
                .push(&mut capture, 0, 48_000, 1, &second[..480], at(9_100))
                .unwrap();
        }
        track.flush(&mut capture, 0).unwrap();
        assert_eq!(capture.position(0), now(9_000) + 480 + 1);
        assert_eq!(track.lost, 0);
    }

    #[test]
    fn whole_seconds_resample_to_exactly_16000() {
        for rate in [44_100, 48_000, 16_000, 96_000] {
            let samples = vec![0.25f32; rate as usize];
            assert_eq!(pcm16(&samples, rate).unwrap().len(), 16_000, "{rate}");
        }
    }
}
