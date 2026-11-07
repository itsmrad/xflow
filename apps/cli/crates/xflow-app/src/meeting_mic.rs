//! The daemon's shared microphone, leased to one meeting recorder over a
//! private socket. The daemon only pumps PCM: no files, database or provider.
//! A vanished recorder ends its lease. A slow one gets counted gaps instead of
//! audio it could not take; dictation never waits on either.
use anyhow::{bail, ensure, Context, Result};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{
        mpsc::{error::TrySendError, Sender},
        watch,
    },
};
use xflow_core::{AudioCapture, CaptureEnvelope};

/// The shared ring holds about two seconds; pull well inside that.
const PUMP: Duration = Duration::from_millis(20);
const OPEN: Duration = Duration::from_secs(8);
/// Audio frames waiting for a slow recorder, about a third of a second. Past
/// that the pump keeps pulling and counts what it drops as a gap.
const BACKLOG: usize = 16;
/// A recorder that has taken nothing for this long is not coming back.
const GONE: Duration = Duration::from_secs(60);
/// How long a closing write may wait for the recorder.
const STALL: Duration = Duration::from_secs(2);
/// How long the last audio may take to reach a recorder that asked to stop.
const RELEASE_FLUSH: Duration = Duration::from_secs(5);
const MAX_FRAME: usize = 1 << 20;

const ACQUIRED: u8 = 1;
const AUDIO: u8 = 2;
const END: u8 = 3;
/// Sent when a second passes with no audio, so the recorder can tell a quiet
/// device from a daemon that went away.
const ALIVE: u8 = 4;
const ALIVE_EVERY: Duration = Duration::from_secs(1);
const RELEASE: u8 = b'r';

pub fn socket_path() -> Result<PathBuf> {
    Ok(crate::paths::runtime_dir()?.join("meeting-mic.sock"))
}

/// What the recorder receives.
#[derive(Debug, PartialEq)]
pub enum Frame {
    /// Mono PCM at `rate`, after `lost` frames the recorder never received.
    Audio {
        rate: u32,
        lost: u64,
        samples: Vec<f32>,
    },
    /// The lease is over: released, or ended by the daemon with this reason.
    End(String),
}

pub struct Server {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Serve the current microphone owner. Config reloads replace it through `audio`.
pub fn serve(audio: watch::Receiver<Arc<dyn AudioCapture>>) -> Result<Server> {
    serve_at(socket_path()?, audio)
}

pub fn serve_at(path: PathBuf, audio: watch::Receiver<Arc<dyn AudioCapture>>) -> Result<Server> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        ensure!(
            meta.file_type().is_socket(),
            "meeting-mic path is not a socket"
        );
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let busy = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            if !stream
                .peer_cred()
                .is_ok_and(|p| p.uid() == unsafe { libc::geteuid() })
            {
                continue;
            }
            if busy.swap(true, Ordering::AcqRel) {
                let _ = end(
                    &mut stream,
                    "another meeting is already using the microphone",
                )
                .await;
                continue;
            }
            let audio = audio.borrow().clone();
            let busy = busy.clone();
            tokio::spawn(async move {
                if let Err(e) = connection(stream, audio).await {
                    xflow_core::log("warn", &format!("meeting microphone: {e:#}"));
                }
                busy.store(false, Ordering::Release);
            });
        }
    });
    Ok(Server { path, task })
}

async fn connection(mut stream: UnixStream, audio: Arc<dyn AudioCapture>) -> Result<()> {
    let mut request = [0u8; 1];
    tokio::time::timeout(OPEN, stream.read_exact(&mut request))
        .await
        .context("recorder sent no request")??;
    ensure!(request[0] == b'a', "unknown meeting microphone request");
    // EOF while the device opens cancels; the audio thread drops an orphan lease.
    let (mut read, mut write) = stream.into_split();
    let opened = tokio::select! {
        // A timeout is reported like a refusal: the recorder shows the reason.
        v = tokio::time::timeout(OPEN, audio.meeting_lease(None)) => v.unwrap_or_else(|_| {
            Err(anyhow::anyhow!("the microphone did not open in time; check that xflowd may use the microphone"))
        }),
        _ = read.read_u8() => bail!("recorder left while the microphone opened"),
    };
    let lease = match opened {
        Ok(lease) => lease,
        Err(e) => {
            let mut stream = read.reunite(write)?;
            return end(&mut stream, &format!("{e:#}")).await;
        }
    };
    send(&mut write, ACQUIRED, &lease.generation().to_le_bytes()).await?;
    // The writer owns the socket, so a recorder that stops reading never delays a pull.
    let (queue, mut backlog) = tokio::sync::mpsc::channel::<Vec<u8>>(BACKLOG);
    let mut writer = tokio::spawn(async move {
        while let Some(frame) = backlog.recv().await {
            write.write_all(&frame).await?;
        }
        Ok::<_, std::io::Error>(write)
    });
    let mut tick = tokio::time::interval(PUMP);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut byte = [0u8; 1];
    let mut sent = tokio::time::Instant::now();
    let mut pump = Pump::default();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                pump.tick();
                let pull = lease.pull();
                let quiet = pull.envelopes.is_empty() && sent.elapsed() >= ALIVE_EVERY;
                let queued = if quiet {
                    pump.alive(&queue)
                } else {
                    pull.envelopes.iter().try_for_each(|e| pump.audio(&queue, e))
                };
                if queued.is_err() {
                    let _ = lease.release().await;
                    pump.log("the recorder connection failed");
                    return match writer.await {
                        Ok(Err(e)) => Err(e.into()),
                        _ => bail!("the meeting recorder connection closed"),
                    };
                }
                if quiet || !pull.envelopes.is_empty() {
                    sent = tokio::time::Instant::now();
                }
                let gone = pump.full_since.is_some_and(|t| t.elapsed() >= GONE);
                if pull.stale || pull.fault.is_some() || gone {
                    let reason = match pull.fault {
                        Some(fault) => fault,
                        None if gone => "the meeting recorder stopped reading".into(),
                        None => "the meeting microphone lease ended".into(),
                    };
                    let _ = lease.release().await;
                    pump.log(&reason);
                    if let Some(gap) = pump.gap_frame() {
                        let _ = queue.try_send(gap);
                    }
                    drop(queue);
                    if let Ok(Ok(Ok(mut write))) = tokio::time::timeout(STALL, &mut writer).await {
                        end(&mut write, &reason).await?;
                    } else {
                        writer.abort();
                    }
                    return Ok(());
                }
            }
            got = read.read(&mut byte) => {
                let last = lease.release().await?;
                if !(matches!(got, Ok(1)) && byte[0] == RELEASE) {
                    writer.abort();
                    pump.log("the recorder left");
                    return Ok(());
                }
                // The recorder asked for the rest. One that froze instead still
                // lets the lease, and the next meeting, go.
                let flush = async {
                    for e in &last.envelopes {
                        queue.send(pump.frame(e)).await.ok()?;
                    }
                    if let Some(gap) = pump.gap_frame() {
                        queue.send(gap).await.ok()?;
                    }
                    drop(queue);
                    (&mut writer).await.ok()?.ok()
                };
                let flushed = tokio::time::timeout(RELEASE_FLUSH, flush).await;
                pump.log("released");
                let Ok(Some(mut write)) = flushed else {
                    writer.abort();
                    bail!("the meeting recorder stopped reading after asking to stop");
                };
                return end(&mut write, "released").await;
            }
        }
    }
}

/// What the pump drops for a slow recorder, and what it saw, for one log line.
#[derive(Default)]
struct Pump {
    /// Frames dropped since the last frame the recorder got; sent with the next one.
    carry: u64,
    /// Rate of the last audio, for a closing gap that has no audio of its own.
    rate: u32,
    full_since: Option<tokio::time::Instant>,
    gaps: u64,
    lost: u64,
    last_tick: Option<tokio::time::Instant>,
    longest_tick: Duration,
}

impl Pump {
    fn tick(&mut self) {
        let now = tokio::time::Instant::now();
        if let Some(last) = self.last_tick.replace(now) {
            self.longest_tick = self.longest_tick.max(now - last);
        }
    }

    /// The bridge frame for `e`, carrying every frame lost before it.
    fn frame(&mut self, e: &CaptureEnvelope) -> Vec<u8> {
        let gap = e.gap.map_or(0, |g| g.frames);
        if gap > 0 {
            self.gaps += 1;
            self.lost += gap;
        }
        let lost = gap + std::mem::take(&mut self.carry);
        self.rate = e.sample_rate;
        let channels = usize::from(e.channels.max(1));
        let mut body = Vec::with_capacity(12 + e.samples.len() / channels * 4);
        body.extend_from_slice(&e.sample_rate.to_le_bytes());
        body.extend_from_slice(&lost.to_le_bytes());
        for frame in e.samples.chunks_exact(channels) {
            let mono = frame.iter().sum::<f32>() / channels as f32;
            body.extend_from_slice(&mono.to_le_bytes());
        }
        framed(AUDIO, &body)
    }

    /// A frame with no audio that reports what was dropped since the last one.
    fn gap_frame(&mut self) -> Option<Vec<u8>> {
        if self.carry == 0 || self.rate == 0 {
            return None;
        }
        let mut body = Vec::with_capacity(12);
        body.extend_from_slice(&self.rate.to_le_bytes());
        body.extend_from_slice(&std::mem::take(&mut self.carry).to_le_bytes());
        Some(framed(AUDIO, &body))
    }

    /// Queue without waiting. A full backlog drops the frame and carries its
    /// audio, and the loss it reported, to the next frame as a gap.
    fn audio(&mut self, queue: &Sender<Vec<u8>>, e: &CaptureEnvelope) -> Result<(), ()> {
        let owed = self.carry + e.gap.map_or(0, |g| g.frames);
        let frame = self.frame(e);
        match queue.try_send(frame) {
            Ok(()) => self.full_since = None,
            Err(TrySendError::Full(_)) => {
                if owed == 0 {
                    self.gaps += 1;
                }
                self.carry = owed + u64::from(e.frame_count);
                self.lost += u64::from(e.frame_count);
                self.full_since
                    .get_or_insert_with(tokio::time::Instant::now);
            }
            Err(TrySendError::Closed(_)) => return Err(()),
        }
        Ok(())
    }

    fn alive(&mut self, queue: &Sender<Vec<u8>>) -> Result<(), ()> {
        match queue.try_send(framed(ALIVE, &[])) {
            Ok(()) => self.full_since = None,
            Err(TrySendError::Full(_)) => {
                self.full_since
                    .get_or_insert_with(tokio::time::Instant::now);
            }
            Err(TrySendError::Closed(_)) => return Err(()),
        }
        Ok(())
    }

    fn log(&self, reason: &str) {
        xflow_core::log(
            "info",
            &format!(
                "meeting microphone ended ({reason}): {} gaps, {} frames lost, longest pull interval {} ms",
                self.gaps,
                self.lost,
                self.longest_tick.as_millis()
            ),
        );
    }
}

fn framed(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + body.len());
    frame.push(kind);
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(body);
    frame
}

async fn send<W: AsyncWriteExt + Unpin>(w: &mut W, kind: u8, body: &[u8]) -> Result<()> {
    ensure!(
        body.len() <= MAX_FRAME,
        "meeting microphone frame too large"
    );
    tokio::time::timeout(STALL, w.write_all(&framed(kind, body)))
        .await
        .context("the meeting recorder stopped reading")??;
    Ok(())
}

async fn end<W: AsyncWriteExt + Unpin>(stream: &mut W, reason: &str) -> Result<()> {
    let mut reason = reason.to_owned();
    reason.truncate(512);
    send(stream, END, reason.as_bytes()).await
}

/// The recorder's side, blocking: one thread reads frames as they arrive.
pub struct Client {
    stream: std::os::unix::net::UnixStream,
}

impl Client {
    pub fn acquire() -> Result<Self> {
        Self::acquire_at(&socket_path()?)
    }
    pub fn acquire_at(path: &Path) -> Result<Self> {
        let mut stream = std::os::unix::net::UnixStream::connect(path).context(
            "the xflow daemon is not sharing its microphone; start or update it (xflow daemon)",
        )?;
        let peer = unsafe {
            let (mut uid, mut gid) = (0, 0);
            ensure!(
                libc::getpeereid(std::os::fd::AsRawFd::as_raw_fd(&stream), &mut uid, &mut gid) == 0,
                "could not check the microphone bridge owner"
            );
            uid
        };
        ensure!(
            peer == unsafe { libc::geteuid() },
            "the microphone bridge belongs to another user"
        );
        stream.set_read_timeout(Some(OPEN + Duration::from_secs(2)))?;
        stream.write_all(b"a")?;
        let mut client = Self { stream };
        match client.read_raw()? {
            (ACQUIRED, _) => {}
            (END, reason) => bail!("{}", String::from_utf8_lossy(&reason)),
            _ => bail!("unexpected microphone bridge reply"),
        }
        client
            .stream
            .set_read_timeout(Some(Duration::from_secs(5)))?;
        Ok(client)
    }
    fn read_raw(&mut self) -> Result<(u8, Vec<u8>)> {
        let mut head = [0u8; 5];
        self.stream.read_exact(&mut head)?;
        let len = u32::from_le_bytes(head[1..5].try_into().unwrap()) as usize;
        ensure!(len <= MAX_FRAME, "invalid microphone bridge frame");
        let mut body = vec![0; len];
        self.stream.read_exact(&mut body)?;
        Ok((head[0], body))
    }
    /// The next frame; a read error means the daemon went away.
    pub fn recv(&mut self) -> Result<Frame> {
        let mut frame = self.read_raw()?;
        while frame.0 == ALIVE {
            frame = self.read_raw()?;
        }
        match frame {
            (AUDIO, body) if body.len() >= 12 && (body.len() - 12) % 4 == 0 => Ok(Frame::Audio {
                rate: u32::from_le_bytes(body[0..4].try_into().unwrap()),
                lost: u64::from_le_bytes(body[4..12].try_into().unwrap()),
                samples: body[12..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect(),
            }),
            (END, reason) => Ok(Frame::End(String::from_utf8_lossy(&reason).into_owned())),
            _ => bail!("invalid microphone bridge frame"),
        }
    }
    /// Ask for the last audio; keep calling `recv` until `Frame::End`.
    pub fn release(&self) -> Result<()> {
        (&self.stream).write_all(&[RELEASE])?;
        Ok(())
    }
    pub fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use xflow_core::{AudioClip, CaptureEnvelope, CaptureGap, MeetingPull, MeetingSubscription};

    #[derive(Default)]
    struct Shared {
        leases: Mutex<u32>,
        released: Mutex<u32>,
        fault: Mutex<Option<String>>,
        quiet: Mutex<bool>,
        /// Pull a real 20 ms of 48 kHz mono each time, and count it.
        live: Mutex<bool>,
        produced: Mutex<u64>,
    }
    struct FakeMic(Arc<Shared>);
    struct FakeLease(Arc<Shared>, Mutex<u64>);

    #[async_trait::async_trait]
    impl MeetingSubscription for FakeLease {
        fn generation(&self) -> u64 {
            7
        }
        fn pull(&self) -> MeetingPull {
            let mut n = self.1.lock().unwrap();
            *n += 1;
            let fault = self.0.fault.lock().unwrap().clone();
            if *self.0.live.lock().unwrap() {
                *self.0.produced.lock().unwrap() += 960;
                return MeetingPull {
                    stale: false,
                    fault,
                    envelopes: vec![CaptureEnvelope {
                        generation: 7,
                        source_id: 1,
                        sample_rate: 48_000,
                        channels: 1,
                        epoch: 1,
                        host_us: 0,
                        frame_offset: 0,
                        frame_count: 960,
                        sequence: *n,
                        gap: None,
                        samples: vec![0.25; 960],
                    }],
                };
            }
            if *self.0.quiet.lock().unwrap() {
                return MeetingPull {
                    stale: false,
                    fault,
                    envelopes: Vec::new(),
                };
            }
            MeetingPull {
                stale: false,
                fault,
                envelopes: vec![CaptureEnvelope {
                    generation: 7,
                    source_id: 1,
                    sample_rate: 48_000,
                    channels: 2,
                    epoch: 1,
                    host_us: 0,
                    frame_offset: 0,
                    frame_count: 2,
                    sequence: *n,
                    gap: (*n == 2).then_some(CaptureGap {
                        from_sequence: 1,
                        to_sequence: 1,
                        frames: 480,
                    }),
                    samples: vec![0.5, 0.1, -0.5, -0.1],
                }],
            }
        }
        async fn release(self: Box<Self>) -> Result<MeetingPull> {
            *self.0.released.lock().unwrap() += 1;
            Ok(MeetingPull {
                stale: false,
                fault: None,
                envelopes: Vec::new(),
            })
        }
    }
    #[async_trait::async_trait]
    impl AudioCapture for FakeMic {
        async fn start(&self) -> Result<()> {
            Ok(())
        }
        async fn stop(&self) -> Result<AudioClip> {
            bail!("unused")
        }
        async fn cancel(&self) -> Result<()> {
            Ok(())
        }
        fn level(&self) -> f32 {
            0.0
        }
        async fn meeting_lease(&self, _: Option<String>) -> Result<Box<dyn MeetingSubscription>> {
            *self.0.leases.lock().unwrap() += 1;
            Ok(Box::new(FakeLease(self.0.clone(), Mutex::new(0))))
        }
    }

    fn server(shared: &Arc<Shared>) -> (tempfile::TempDir, PathBuf, Server) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.sock");
        let audio: Arc<dyn AudioCapture> = Arc::new(FakeMic(shared.clone()));
        let (_tx, rx) = watch::channel(audio);
        let server = serve_at(path.clone(), rx).unwrap();
        (dir, path, server)
    }
    async fn wait(what: impl Fn() -> bool) {
        for _ in 0..200 {
            if what() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streams_mono_with_gaps_and_releases_on_request() {
        let shared = Arc::new(Shared::default());
        let (_dir, path, _server) = server(&shared);
        let frames = tokio::task::spawn_blocking(move || {
            let mut client = Client::acquire_at(&path).unwrap();
            let first = client.recv().unwrap();
            let second = client.recv().unwrap();
            client.release().unwrap();
            let mut tail = Vec::new();
            loop {
                match client.recv().unwrap() {
                    Frame::End(reason) => break (first, second, tail, reason),
                    audio => tail.push(audio),
                }
            }
        })
        .await
        .unwrap();
        let (first, second, _tail, reason) = frames;
        assert_eq!(
            first,
            Frame::Audio {
                rate: 48_000,
                lost: 0,
                samples: vec![0.3, -0.3]
            }
        );
        assert!(matches!(second, Frame::Audio { lost: 480, .. }));
        assert_eq!(reason, "released");
        assert_eq!(*shared.released.lock().unwrap(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_vanished_recorder_releases_and_a_second_is_refused() {
        let shared = Arc::new(Shared::default());
        let (_dir, path, _server) = server(&shared);
        let p = path.clone();
        let refused = tokio::task::spawn_blocking(move || {
            let first = Client::acquire_at(&p).unwrap();
            let second = Client::acquire_at(&p).err().map(|e| e.to_string());
            drop(first);
            second
        })
        .await
        .unwrap();
        assert!(refused.unwrap().contains("already using"));
        wait(|| *shared.released.lock().unwrap() == 1).await;
        // The lease is free again.
        let s = shared.clone();
        tokio::task::spawn_blocking(move || Client::acquire_at(&path).map(drop))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*s.leases.lock().unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_quiet_microphone_sends_keepalives() {
        let shared = Arc::new(Shared::default());
        *shared.quiet.lock().unwrap() = true;
        let (_dir, path, _server) = server(&shared);
        let kinds = tokio::task::spawn_blocking(move || {
            let mut client = Client::acquire_at(&path).unwrap();
            let started = std::time::Instant::now();
            let mut kinds = Vec::new();
            while started.elapsed() < Duration::from_millis(2_500) {
                kinds.push(client.read_raw().unwrap().0);
            }
            kinds
        })
        .await
        .unwrap();
        assert!(
            kinds.len() >= 2 && kinds.iter().all(|k| *k == ALIVE),
            "{kinds:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_daemon_fault_ends_the_lease_with_its_reason() {
        let shared = Arc::new(Shared::default());
        *shared.fault.lock().unwrap() = Some("dictation switched microphone".into());
        let (_dir, path, _server) = server(&shared);
        let reason = tokio::task::spawn_blocking(move || {
            let mut client = Client::acquire_at(&path).unwrap();
            loop {
                if let Frame::End(reason) = client.recv().unwrap() {
                    break reason;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(reason, "dictation switched microphone");
        wait(|| *shared.released.lock().unwrap() == 1).await;
    }

    /// A recorder that stops reading used to lose the lease after 2 s, which
    /// stopped the meeting. Now the pump keeps pulling and reports a gap.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_recorder_that_stops_reading_gets_a_gap_not_an_end() {
        let shared = Arc::new(Shared::default());
        *shared.live.lock().unwrap() = true;
        let (_dir, path, _server) = server(&shared);
        let (lost, frames, reason) = tokio::task::spawn_blocking(move || {
            let mut client = Client::acquire_at(&path).unwrap();
            std::thread::sleep(Duration::from_secs(3));
            let (mut lost, mut frames) = (0u64, 0u64);
            let started = std::time::Instant::now();
            let mut count = |frame| match frame {
                Frame::Audio {
                    lost: l, samples, ..
                } => {
                    lost += l;
                    frames += samples.len() as u64;
                    None
                }
                Frame::End(reason) => Some(reason),
            };
            while started.elapsed() < Duration::from_millis(600) {
                if let Some(reason) = count(client.recv().unwrap()) {
                    panic!("the lease ended: {reason}");
                }
            }
            client.release().unwrap();
            let reason = loop {
                if let Some(reason) = count(client.recv().unwrap()) {
                    break reason;
                }
            };
            (lost, frames, reason)
        })
        .await
        .unwrap();
        assert_eq!(reason, "released");
        assert!(
            lost > 48_000,
            "about 3 s minus the backlog is a gap: {lost}"
        );
        assert!(frames > 0);
        assert_eq!(
            lost + frames,
            *shared.produced.lock().unwrap(),
            "every pulled frame is delivered or counted"
        );
    }
}
