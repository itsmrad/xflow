//! Opt-in adapters for the real daemon binary's isolated integration tests.
//! No microphone, desktop commands, or credential APIs belong in this module.
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use xflow_core::{
    config::RecordingConfig, AppContext, AudioCapture, AudioClip, Desktop, InjectionOutcome,
};

type Adapters = (Arc<dyn AudioCapture>, Arc<dyn Desktop>);

pub(crate) fn adapters(config: &RecordingConfig) -> Result<Option<Adapters>> {
    let audio = std::env::var_os("XFLOW_TEST_AUDIO");
    let sink = std::env::var_os("XFLOW_TEST_SINK");
    if audio.is_none() && sink.is_none() {
        return Ok(None);
    }
    let audio = audio.context("XFLOW_TEST_AUDIO and XFLOW_TEST_SINK must be set together")?;
    let sink =
        PathBuf::from(sink.context("XFLOW_TEST_AUDIO and XFLOW_TEST_SINK must be set together")?);
    let voiced = match audio.to_str() {
        Some("voice") => true,
        Some("silence") => false,
        _ => bail!("XFLOW_TEST_AUDIO must be voice or silence"),
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
    Ok(Some((
        Arc::new(SyntheticAudio {
            active: AtomicBool::new(false),
            voiced,
            threshold: config.silence_threshold,
        }),
        Arc::new(FileDesktop(sink)),
    )))
}

struct SyntheticAudio {
    active: AtomicBool,
    voiced: bool,
    threshold: f32,
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
        if !self.voiced || 0.2 <= self.threshold {
            bail!("no speech detected");
        }
        // ponytail: fixed one-second mono PCM, not a timing/device simulator.
        // Add WAV fixtures here when codec/device coverage needs real samples.
        Ok(AudioClip {
            samples: vec![0.2; 16_000],
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
