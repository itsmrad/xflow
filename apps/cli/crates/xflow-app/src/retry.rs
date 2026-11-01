//! A failed dictation kept for `xflow retry` (`privacy.keep_failed_audio`, off by
//! default): at most one, 0600 files in a 0700 directory, deleted after a
//! successful retry, a discard, clear-history or 24 hours.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
use xflow_core::{ipc::FailedAudio, AudioClip};

pub const TTL_SECS: i64 = 24 * 60 * 60;
/// 20 minutes (the recording cap) of 16 kHz PCM16 is 38.4 MB.
pub const MAX_BYTES: u64 = 40 << 20;
const META: &str = "kept.json";

/// One segment of the kept dictation, in recording order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    /// Transcribed; `delivered` once its text was pasted, copied or printed.
    Text { text: String, delivered: bool },
    /// Still to transcribe, in `<index>.wav` (16 kHz mono PCM16).
    Audio { low_energy: bool },
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Kept {
    pub created_at: i64,
    pub audio_ms: u64,
    pub parts: Vec<Part>,
}
impl Kept {
    pub fn summary(&self) -> FailedAudio {
        FailedAudio {
            created_at: self.created_at,
            expires_at: self.created_at + TTL_SECS,
            audio_ms: self.audio_ms,
            partial: self.parts.iter().any(|p| {
                matches!(
                    p,
                    Part::Text {
                        delivered: true,
                        ..
                    }
                )
            }),
        }
    }
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
/// Replaces the kept dictation; `audio` holds one clip per `Part::Audio`, in order.
pub fn save(dir: &Path, created_at: i64, parts: Vec<Part>, audio: Vec<AudioClip>) -> Result<Kept> {
    // Each clip is stored as 16 kHz mono PCM16: 32 bytes per millisecond.
    let audio_ms: u64 = audio
        .iter()
        .map(|a| {
            (a.samples.len() / usize::from(a.channels.max(1))) as u64 * 1000
                / u64::from(a.sample_rate.max(1))
        })
        .sum();
    if audio_ms * 32 > MAX_BYTES {
        bail!("failed audio is over the {} MB cap", MAX_BYTES >> 20);
    }
    let wavs = audio
        .iter()
        .map(xflow_providers::encode_wav)
        .collect::<Result<Vec<_>>>()?;
    clear(dir)?;
    crate::paths::private_dir(dir)?;
    let indexes = parts
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Part::Audio { .. }));
    for ((index, _), wav) in indexes.zip(&wavs) {
        write_private(&dir.join(format!("{index}.wav")), wav)?;
    }
    let kept = Kept {
        created_at,
        audio_ms,
        parts,
    };
    // Written last and renamed in: a crash mid-save leaves no half-kept dictation.
    write_private(&dir.join("kept.tmp"), &serde_json::to_vec(&kept)?)?;
    std::fs::rename(dir.join("kept.tmp"), dir.join(META))?;
    Ok(kept)
}
/// The kept dictation, or None once it expired (then deleted).
pub fn load(dir: &Path) -> Result<Option<Kept>> {
    let bytes = match read_private(&dir.join(META)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let kept: Kept = serde_json::from_slice(&bytes).context("invalid kept dictation")?;
    let now = now();
    // A future timestamp would stretch the 24 hours.
    if now >= kept.created_at + TTL_SECS || kept.created_at > now + 60 {
        clear(dir)?;
        return Ok(None);
    }
    Ok(Some(kept))
}
pub fn audio(dir: &Path, index: usize) -> Result<AudioClip> {
    xflow_providers::decode_wav(&read_private(&dir.join(format!("{index}.wav")))?)
}
pub fn clear(dir: &Path) -> Result<()> {
    // Removes a symlink itself, never its target.
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}
fn read_private(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn clip(seconds: usize) -> AudioClip {
        AudioClip {
            samples: vec![0.25; seconds * 16_000],
            sample_rate: 16_000,
            channels: 1,
        }
    }
    /// Half the memory of a 16 kHz clip of the same length.
    fn long(seconds: usize) -> AudioClip {
        AudioClip {
            samples: vec![0.25; seconds * 8_000],
            sample_rate: 8_000,
            channels: 1,
        }
    }
    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn kept_audio_is_private_ordered_and_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let parts = vec![
            Part::Text {
                text: "first".into(),
                delivered: true,
            },
            Part::Audio { low_energy: false },
        ];
        let kept = save(&dir, now(), parts.clone(), vec![clip(2)]).unwrap();
        assert_eq!(kept.audio_ms, 2000);
        assert!(kept.summary().partial);
        assert_eq!(mode(&dir), 0o700);
        for file in ["kept.json", "1.wav"] {
            assert_eq!(mode(&dir.join(file)), 0o600, "{file}");
        }
        assert_eq!(load(&dir).unwrap().unwrap().parts, parts);
        let audio = audio(&dir, 1).unwrap();
        assert_eq!((audio.samples.len(), audio.sample_rate), (32_000, 16_000));
        assert!((audio.samples[0] - 0.25).abs() < 1e-4);
        clear(&dir).unwrap();
        assert!(!dir.exists());
    }
    #[test]
    fn expired_oversize_and_symlinked_kept_audio_never_survives() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let audio = vec![Part::Audio { low_energy: false }];
        save(&dir, now() - TTL_SECS, audio.clone(), vec![clip(1)]).unwrap();
        assert!(load(&dir).unwrap().is_none());
        assert!(!dir.exists(), "expiry deletes the audio");
        save(&dir, now() + 3600, audio.clone(), vec![clip(1)]).unwrap();
        assert!(
            load(&dir).unwrap().is_none(),
            "a future timestamp expires too"
        );
        // 22 minutes is past the byte cap: nothing is written, the old one stays.
        save(&dir, now(), audio.clone(), vec![clip(1)]).unwrap();
        assert!(save(&dir, now(), audio.clone(), vec![long(22 * 60)]).is_err());
        assert_eq!(load(&dir).unwrap().unwrap().audio_ms, 1000);
        // A planted symlink is replaced, never followed or written through.
        clear(&dir).unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        save(&dir, now(), audio, vec![clip(1)]).unwrap();
        assert!(!std::fs::symlink_metadata(&dir).unwrap().is_symlink());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        std::fs::remove_file(dir.join(META)).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("x"), dir.join(META)).unwrap();
        assert!(load(&dir).is_err(), "kept.json is never followed");
    }
}
