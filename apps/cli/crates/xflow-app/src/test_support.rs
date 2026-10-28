//! Opt-in adapters for the real daemon binary's isolated integration tests.
//! No microphone, desktop commands, or credential APIs belong in this module.
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use xflow_core::{AppContext, AudioCapture, AudioClip, Desktop, InjectionOutcome};

type Adapters = (Arc<dyn AudioCapture>, Arc<dyn Desktop>);

pub(crate) fn adapters() -> Result<Option<Adapters>> {
    let audio = std::env::var_os("XFLOW_TEST_AUDIO");
    let sink = std::env::var_os("XFLOW_TEST_SINK");
    if audio.is_none() && sink.is_none() {
        return Ok(None);
    }
    // A debug WAV source uses the native desktop, outside the isolated harness.
    #[cfg(debug_assertions)]
    if sink.is_none()
        && std::env::var_os("XFLOW_TEST_ROOT").is_none()
        && audio
            .as_ref()
            .is_some_and(|a| a != "voice" && a != "silence")
    {
        return Ok(None);
    }
    let audio = audio.context("XFLOW_TEST_AUDIO and XFLOW_TEST_SINK must be set together")?;
    let sink =
        PathBuf::from(sink.context("XFLOW_TEST_AUDIO and XFLOW_TEST_SINK must be set together")?);
    let voiced = match audio.to_str() {
        Some("voice") => Some(true),
        Some("silence") => Some(false),
        Some("wav") => None,
        _ => bail!("XFLOW_TEST_AUDIO must be voice, silence or wav"),
    };
    let root =
        PathBuf::from(std::env::var_os("XFLOW_TEST_ROOT").context("XFLOW_TEST_ROOT is required")?);
    let metadata = std::fs::symlink_metadata(&root)?;
    if !root.is_absolute()
        || root.canonicalize()? != root
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        bail!("test root must be an owned, private, canonical directory");
    }
    // Require one fresh sandbox root; never fall back to the user's XDG paths.
    for (name, subdir) in [
        ("XDG_RUNTIME_DIR", "run"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
    ] {
        let path =
            PathBuf::from(std::env::var_os(name).with_context(|| format!("{name} is required"))?);
        if path != root.join(subdir) || path.canonicalize()? != path {
            bail!("test XDG directories must be inside XFLOW_TEST_ROOT");
        }
    }
    if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        bail!("test adapters require DISPLAY and WAYLAND_DISPLAY to be unset");
    }
    // Only this fixed file may be created; no symlink or arbitrary output path.
    if sink != root.join("delivered.txt") {
        bail!("test sink must be XFLOW_TEST_ROOT/delivered.txt");
    }
    let audio: Arc<dyn AudioCapture> = match voiced {
        Some(voiced) => Arc::new(SyntheticAudio {
            active: AtomicBool::new(false),
            voiced,
        }),
        None => Arc::new(WavAudio::new(&root)?),
    };
    Ok(Some((audio, Arc::new(FileDesktop(sink)))))
}

/// `XFLOW_TEST_AUDIO=wav` plays the fixed `XFLOW_TEST_ROOT/input.wav` as a live
/// microphone: `drain` returns what has "arrived" since the last call, at
/// `XFLOW_TEST_AUDIO_SPEED` times real time (default 1), and the capture finishes
/// at the end of the file. Samples are streamed from disk, so daemon RSS stays
/// representative of a real long dictation.
struct WavAudio {
    path: PathBuf,
    speed: f64,
    playback: Mutex<Option<Playback>>,
    finished: AtomicBool,
}

struct Playback {
    reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
    started: Instant,
    sent: u32,
}

impl WavAudio {
    fn new(root: &Path) -> Result<Self> {
        let path = root.join("input.wav");
        let metadata = std::fs::symlink_metadata(&path).context("XFLOW_TEST_ROOT/input.wav")?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            bail!("input.wav must be an owned regular file");
        }
        let speed = match std::env::var("XFLOW_TEST_AUDIO_SPEED") {
            Ok(value) => value.parse().context("XFLOW_TEST_AUDIO_SPEED")?,
            Err(_) => 1.0,
        };
        if !(1.0..=1000.0).contains(&speed) {
            bail!("XFLOW_TEST_AUDIO_SPEED must be 1..1000");
        }
        Ok(Self {
            path,
            speed,
            playback: Mutex::new(None),
            finished: AtomicBool::new(false),
        })
    }

    /// Everything that has arrived since the last call.
    fn take(&self) -> Result<Option<AudioClip>> {
        let mut guard = self.playback.lock().unwrap();
        let Some(playback) = guard.as_mut() else {
            return Ok(None);
        };
        let spec = playback.reader.spec();
        let total = playback.reader.duration();
        let elapsed = playback.started.elapsed().as_secs_f64() * self.speed;
        let due = ((elapsed * f64::from(spec.sample_rate)) as u32).min(total);
        let count = (due - playback.sent) as usize * usize::from(spec.channels);
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => playback
                .reader
                .samples::<f32>()
                .take(count)
                .collect::<std::result::Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
                playback
                    .reader
                    .samples::<i32>()
                    .take(count)
                    .map(|s| s.map(|s| s as f32 / scale))
                    .collect::<std::result::Result<_, _>>()?
            }
        };
        playback.sent = due;
        if due == total {
            self.finished.store(true, Ordering::SeqCst);
        }
        Ok(Some(AudioClip {
            samples,
            sample_rate: spec.sample_rate,
            channels: spec.channels,
        }))
    }
}

#[async_trait]
impl AudioCapture for WavAudio {
    async fn start(&self) -> Result<()> {
        let mut playback = self.playback.lock().unwrap();
        if playback.is_some() {
            bail!("already recording");
        }
        *playback = Some(Playback {
            reader: hound::WavReader::open(&self.path).context("invalid input.wav")?,
            started: Instant::now(),
            sent: 0,
        });
        self.finished.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&self) -> Result<AudioClip> {
        let clip = self.take()?.context("no recording is active")?;
        *self.playback.lock().unwrap() = None;
        self.finished.store(false, Ordering::SeqCst);
        Ok(clip)
    }
    async fn cancel(&self) -> Result<()> {
        *self.playback.lock().unwrap() = None;
        self.finished.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn drain(&self) -> Result<Option<AudioClip>> {
        self.take()
    }
    fn level(&self) -> f32 {
        if self.playback.lock().unwrap().is_some() {
            0.2
        } else {
            0.0
        }
    }
    fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}

struct SyntheticAudio {
    active: AtomicBool,
    voiced: bool,
}

#[async_trait]
impl AudioCapture for SyntheticAudio {
    async fn start(&self) -> Result<()> {
        if self.active.swap(true, Ordering::SeqCst) {
            bail!("already recording");
        }
        Ok(())
    }
    async fn stop(&self) -> Result<AudioClip> {
        if !self.active.swap(false, Ordering::SeqCst) {
            bail!("no recording is active");
        }
        // ponytail: fixed one-second mono PCM, not a timing/device simulator.
        // Add WAV fixtures here when codec/device coverage needs real samples.
        Ok(AudioClip {
            samples: vec![if self.voiced { 0.2 } else { 0.0 }; 16_000],
            sample_rate: 16_000,
            channels: 1,
        })
    }
    async fn cancel(&self) -> Result<()> {
        self.active.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn level(&self) -> f32 {
        if self.active.load(Ordering::SeqCst) && self.voiced {
            0.2
        } else {
            0.0
        }
    }
}

struct FileDesktop(PathBuf);

#[async_trait]
impl Desktop for FileDesktop {
    async fn context(&self) -> Result<AppContext> {
        Ok(AppContext::default())
    }
    async fn inject(&self, text: &str, _: &AppContext) -> Result<InjectionOutcome> {
        self.copy(text).await?;
        Ok(InjectionOutcome::Pasted)
    }
    async fn copy(&self, text: &str) -> Result<()> {
        use std::io::Write;
        // The tiny sink is synchronous; tests never call a desktop subprocess.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.0)?;
        file.write_all(text.as_bytes())?;
        Ok(())
    }
}
