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

pub struct Request<'a> {
    pub app: Option<&'a str>,
    pub title: Option<&'a str>,
    pub mic: bool,
    pub yes: bool,
    pub control_stdin: bool,
}

enum Msg {
    Native(u8, Vec<u8>),
    NativeGone,
    Mic(xflow_app::meeting_mic::Frame),
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
            Ok(Msg::Native(b'C', body)) => break body,
            Ok(Msg::Native(b'D', _)) | Ok(Msg::Stop(_)) => {
                native.quit();
                out.text("Not recorded")?;
                return Ok(());
            }
            Ok(Msg::Native(b'E', body)) => {
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
                    if tx.send(Msg::Mic(frame)).is_err() || end {
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
        let result: anyhow::Result<()> = match msg {
            Ok(Msg::Mic(xflow_app::meeting_mic::Frame::Audio {
                rate,
                lost,
                samples,
            })) => match sources.mic {
                Some(s) => tracks[s].push(&mut capture, s, rate, lost, &samples),
                None => Ok(()),
            },
            Ok(Msg::Mic(xflow_app::meeting_mic::Frame::End(why))) => {
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
            Ok(Msg::Native(b'A', body)) if body.len() >= 12 => match sources.app {
                Some(s) => {
                    let rate = u32::from_le_bytes(body[0..4].try_into().unwrap());
                    let lost = u64::from_le_bytes(body[4..12].try_into().unwrap());
                    let samples: Vec<f32> = body[12..]
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                        .collect();
                    tracks[s].push(&mut capture, s, rate, lost, &samples)
                }
                None => Ok(()),
            },
            Ok(Msg::Native(b'S', body)) => {
                stop_with(
                    String::from_utf8_lossy(&body).into_owned(),
                    false,
                    &mut reason,
                    &mut failed,
                );
                Ok(())
            }
            Ok(Msg::Native(b'E', body)) => {
                stop_with(
                    String::from_utf8_lossy(&body).into_owned(),
                    true,
                    &mut reason,
                    &mut failed,
                );
                Ok(())
            }
            Ok(Msg::Native(b'X', _)) | Ok(Msg::NativeGone) => {
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
            let now = (elapsed.as_millis() as i64) * RATE / 1000;
            for s in [sources.mic, sources.app].into_iter().flatten() {
                if let Err(e) = tracks[s].catch_up(&mut capture, s, now) {
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
}
impl Track {
    fn push(
        &mut self,
        capture: &mut Capture,
        source: usize,
        rate: u32,
        lost: u64,
        samples: &[f32],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            (8_000..=192_000).contains(&rate),
            "invalid audio rate {rate}"
        );
        if rate != self.rate {
            self.flush(capture, source)?;
            self.rate = rate;
        }
        if lost > 0 && self.seen {
            self.flush(capture, source)?;
            capture.gap(
                source,
                (lost * RATE as u64 / u64::from(rate)) as i64,
                "audio frames were dropped",
            )?;
        }
        self.seen = true;
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
    fn catch_up(&mut self, capture: &mut Capture, source: usize, now: i64) -> anyhow::Result<()> {
        if !self.seen {
            return Ok(());
        }
        let buffered = self.pending.len() as i64 * RATE / i64::from(self.rate.max(1));
        let behind = now - capture.position(source) - buffered;
        if behind > STALL_SAMPLES {
            self.flush(capture, source)?;
            capture.gap(source, behind, "no audio arrived from this source")?;
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
        if tx.send(Msg::Native(head[0], body)).is_err() {
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

    #[test]
    fn whole_seconds_resample_to_exactly_16000() {
        for rate in [44_100, 48_000, 16_000, 96_000] {
            let samples = vec![0.25f32; rate as usize];
            assert_eq!(pcm16(&samples, rate).unwrap().len(), 16_000, "{rate}");
        }
    }
}
