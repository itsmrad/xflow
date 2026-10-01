//! Short cues. Playback work and child reaping happen off the caller's thread.
use anyhow::{bail, Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use xflow_core::config::SoundsConfig;

#[derive(Clone, Copy, Debug)]
pub enum Cue {
    Start,
    Stop,
    Error,
}

static PLAYING: AtomicUsize = AtomicUsize::new(0);
struct Playback;
impl Drop for Playback {
    fn drop(&mut self) {
        PLAYING.fetch_sub(1, Ordering::Relaxed);
    }
}

fn player_path(path: &std::ffi::OsStr) -> Option<PathBuf> {
    choose_player(|name| {
        std::env::split_paths(path)
            .map(|p| p.join(name))
            .find(|p| executable(p))
    })
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn choose_player(mut find: impl FnMut(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    ["pw-play", "paplay", "aplay"]
        .into_iter()
        .find_map(&mut find)
}

/// Fire-and-forget; silently skip disabled/muted cues and excessive overlaps.
pub fn play(cue: Cue, config: &SoundsConfig) {
    if !config.enabled || !config.volume.is_finite() || config.volume <= 0.0 {
        return;
    }
    if PLAYING
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < 4).then_some(n + 1)
        })
        .is_err()
    {
        return;
    }
    let guard = Playback;
    let config = config.clone();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let work = async move {
        let _guard = guard;
        match tokio::time::timeout(Duration::from_secs(10), playback(cue, &config, &path)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::debug!(%error, "sound playback unavailable"),
            Err(error) => tracing::debug!(%error, "sound playback timed out"),
        }
    };
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(work);
    } else {
        // Calls outside a daemon runtime remain nonblocking, including discovery.
        let _ = std::thread::Builder::new()
            .name("xflow-cue".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(work);
                }
            });
    }
}

fn builtin(cue: Cue) -> &'static [u8] {
    match cue {
        Cue::Start => include_bytes!("../assets/start.wav"),
        Cue::Stop => include_bytes!("../assets/stop.wav"),
        Cue::Error => include_bytes!("../assets/error.wav"),
    }
}

async fn playback(cue: Cue, config: &SoundsConfig, path: &std::ffi::OsStr) -> Result<()> {
    let Some(player) = player_path(path) else {
        return Ok(());
    };
    let custom = match cue {
        Cue::Start => &config.start,
        Cue::Stop => &config.stop,
        Cue::Error => &config.error,
    };
    let mut command = Command::new(&player);
    let volume = config.volume.clamp(0.0, 1.0);
    let is_alsa = player.file_name().is_some_and(|n| n == "aplay");
    if player.file_name().is_some_and(|n| n == "pw-play") {
        command.args(["--volume", &volume.to_string()]);
    } else if !is_alsa {
        command.arg(format!("--volume={}", (volume * 65536.0).round() as u32));
    }
    let mut data = if is_alsa || custom.is_none() {
        Some(if let Some(file) = custom {
            let metadata = tokio::fs::metadata(file).await?;
            if metadata.len() > 8 * 1024 * 1024 {
                bail!("custom ALSA cue exceeds 8 MiB");
            }
            {
                let mut bytes = Vec::new();
                tokio::fs::File::open(file)
                    .await?
                    .take(8 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .await?;
                if bytes.len() > 8 * 1024 * 1024 {
                    bail!("custom ALSA cue exceeds 8 MiB");
                }
                bytes
            }
        } else {
            builtin(cue).to_vec()
        })
    } else {
        None
    };
    if is_alsa {
        command.args(["-q", "-t", "wav"]);
        scale_wav(data.as_mut().context("missing cue data")?, volume)?;
    }
    if data.is_some() {
        if !is_alsa && player.file_name().is_some_and(|n| n == "pw-play") {
            command.arg("-");
        }
        command.stdin(Stdio::piped());
    } else if let Some(file) = custom {
        command.arg("--").arg(file).stdin(Stdio::null());
    }
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    if let Some(data) = data {
        let mut stdin = child.stdin.take().context("missing player stdin")?;
        stdin.write_all(&data).await?;
        stdin.shutdown().await?;
        drop(stdin);
    }
    if !child.wait().await?.success() {
        bail!("sound player failed");
    }
    Ok(())
}

// aplay has no per-stream volume flag. Attenuate PCM16 WAV data locally rather
// than changing a user's mixer. Other players accept arbitrary custom formats.
fn scale_wav(data: &mut [u8], volume: f32) -> Result<()> {
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        bail!("ALSA cues must be PCM16 WAV files");
    }
    let mut offset = 12;
    let mut pcm16 = false;
    let mut samples = None;
    while offset + 8 <= data.len() {
        let len = u32::from_le_bytes(data[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(len)
            .filter(|end| *end <= data.len())
            .context("invalid WAV chunk")?;
        if &data[offset..offset + 4] == b"fmt " && len >= 16 {
            pcm16 = data[start..start + 2] == [1, 0] && data[start + 14..start + 16] == [16, 0];
        }
        if &data[offset..offset + 4] == b"data" {
            samples = Some(start..end);
        }
        offset = end + len % 2;
    }
    if !pcm16 {
        bail!("ALSA cues must be PCM16 WAV files");
    }
    for sample in data[samples.context("missing WAV data")?]
        .as_chunks_mut::<2>()
        .0
    {
        let value = i16::from_le_bytes([sample[0], sample[1]]);
        sample.copy_from_slice(&((f32::from(value) * volume).round() as i16).to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn player_preference_and_missing_tools() {
        assert_eq!(choose_player(|n| Some(n.into())), Some("pw-play".into()));
        assert_eq!(
            choose_player(|n| (n != "pw-play").then(|| n.into())),
            Some("paplay".into())
        );
        assert_eq!(
            choose_player(|n| (n == "aplay").then(|| n.into())),
            Some("aplay".into())
        );
        assert_eq!(choose_player(|_| None), None);
    }
    #[test]
    fn builtin_wavs_are_short_and_volume_is_applied() {
        for cue in [Cue::Start, Cue::Stop, Cue::Error] {
            let mut wav = builtin(cue).to_vec();
            assert!(wav.len() < 5000);
            scale_wav(&mut wav, 0.0).unwrap();
            assert!(wav[44..].iter().all(|b| *b == 0));
        }
        assert!(scale_wav(&mut [0; 44], 0.5).is_err());
    }
}
