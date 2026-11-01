//! A failed dictation kept for `xflow retry` (`privacy.keep_failed_audio`, off by
//! default): at most one, 0600 files in 0700 directories, deleted after a
//! successful retry, a discard, clear-history or 24 hours.
//!
//! A save is staged in `staging-<session>/`, then committed by renaming it to
//! `current/` under a lock, only while its session is still live: a cancel or
//! clear that wins the lock first leaves nothing behind.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};
use xflow_core::{ipc::FailedAudio, AudioClip};

pub const TTL_SECS: i64 = 24 * 60 * 60;
/// 20 minutes (the recording cap) of 16 kHz PCM16 is 38.4 MB.
pub const MAX_BYTES: u64 = 40 << 20;
const META: &str = "kept.json";
const CURRENT: &str = "current";

/// One segment of the kept dictation, in recording order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    /// Transcribed by `provider`; `delivered` once pasted, copied or printed.
    Text {
        text: String,
        delivered: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
    /// Still to transcribe, in `<index>.wav` (16 kHz mono PCM16).
    Audio { low_energy: bool },
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Kept {
    pub created_at: i64,
    pub audio_ms: u64,
    /// The daemon session that kept it (see `session`).
    pub session: String,
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
/// Names a daemon session across restarts: generations restart at 0.
pub fn session(generation: u64) -> String {
    format!("{}-{generation}", std::process::id())
}
/// Commits, cancels and clears never interleave.
fn lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
/// A save written but not yet committed; dropping it deletes it.
pub struct Staged {
    path: PathBuf,
}
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
/// Writes a dictation to keep; `audio` holds one clip per `Part::Audio`, in order.
pub fn stage(
    dir: &Path,
    session: String,
    created_at: i64,
    parts: Vec<Part>,
    audio: Vec<AudioClip>,
) -> Result<Staged> {
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
    crate::paths::private_dir(dir)?;
    let path = dir.join(format!("staging-{session}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::DirBuilder::new().mode(0o700).create(&path)?;
    let staged = Staged { path };
    let indexes = parts
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Part::Audio { .. }));
    for ((index, _), wav) in indexes.zip(&wavs) {
        write_private(&staged.path.join(format!("{index}.wav")), wav)?;
    }
    let kept = Kept {
        created_at,
        audio_ms,
        session,
        parts,
    };
    write_private(&staged.path.join(META), &serde_json::to_vec(&kept)?)?;
    Ok(staged)
}
/// Replaces the kept dictation with `staged` while `live()`; otherwise deletes it.
pub fn commit(staged: Staged, live: impl FnOnce() -> bool) -> Result<()> {
    let _lock = lock();
    if !live() {
        bail!("session ended");
    }
    let current = staged.path.with_file_name(CURRENT);
    remove(&current)?;
    std::fs::rename(&staged.path, &current)?;
    Ok(())
}
/// The kept dictation, or None once it expired (then deleted).
pub fn load(dir: &Path) -> Result<Option<Kept>> {
    let current = dir.join(CURRENT);
    let bytes = match read_private(&current.join(META)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    for path in [dir, &current] {
        if !std::fs::symlink_metadata(path)?.is_dir() {
            bail!("unsafe kept dictation directory");
        }
    }
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
    let path = dir.join(CURRENT).join(format!("{index}.wav"));
    xflow_providers::decode_wav(&read_private(&path)?)
}
pub fn clear(dir: &Path) -> Result<()> {
    let _lock = lock();
    remove(dir)
}
/// Deletes what `session` kept, if it is still the kept dictation.
pub fn discard(dir: &Path, session: &str) -> Result<()> {
    let _lock = lock();
    let current = dir.join(CURRENT);
    match read_private(&current.join(META)) {
        Ok(bytes) => match serde_json::from_slice::<Kept>(&bytes) {
            Ok(kept) if kept.session != session => Ok(()),
            _ => remove(&current),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
/// Deletes everything but the kept dictation: saves that were never committed.
pub fn prune(dir: &Path) -> Result<()> {
    let _lock = lock();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() != CURRENT {
            remove(&entry.path())?;
        }
    }
    Ok(())
}
#[cfg(test)]
pub fn save(dir: &Path, created_at: i64, parts: Vec<Part>, audio: Vec<AudioClip>) -> Result<()> {
    // A generation no daemon test reaches.
    commit(
        stage(dir, session(u64::MAX), created_at, parts, audio)?,
        || true,
    )
}
/// Removes a symlink itself, never its target.
fn remove(path: &Path) -> Result<()> {
    let removed = match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) => Err(e),
    };
    match removed {
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
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn kept_audio_is_private_ordered_and_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let parts = vec![
            Part::Text {
                text: "first".into(),
                delivered: true,
                provider: Some("groq".into()),
            },
            Part::Audio { low_energy: false },
        ];
        save(&dir, now(), parts.clone(), vec![clip(2)]).unwrap();
        let kept = load(&dir).unwrap().unwrap();
        assert_eq!((kept.audio_ms, kept.parts), (2000, parts));
        assert_eq!(names(&dir), ["current"]);
        for path in [&dir, &dir.join(CURRENT)] {
            assert_eq!(mode(path), 0o700);
        }
        for file in ["kept.json", "1.wav"] {
            assert_eq!(mode(&dir.join(CURRENT).join(file)), 0o600, "{file}");
        }
        let audio = audio(&dir, 1).unwrap();
        assert_eq!((audio.samples.len(), audio.sample_rate), (32_000, 16_000));
        assert!((audio.samples[0] - 0.25).abs() < 1e-4);
        clear(&dir).unwrap();
        assert!(!dir.exists());
    }
    #[test]
    fn a_save_whose_session_ended_leaves_nothing_and_discard_takes_only_its_own() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let audio = vec![Part::Audio { low_energy: false }];
        save(&dir, now(), audio.clone(), vec![clip(1)]).unwrap();
        let staged = stage(&dir, session(2), now(), audio.clone(), vec![clip(2)]).unwrap();
        assert_eq!(names(&dir), ["current", &format!("staging-{}", session(2))]);
        assert!(commit(staged, || false).is_err());
        assert_eq!(names(&dir), ["current"], "the earlier one stays");
        assert_eq!(load(&dir).unwrap().unwrap().audio_ms, 1000);
        // A save left behind (e.g. by a crash) is pruned; the kept one is not.
        std::mem::forget(stage(&dir, session(3), now(), audio, vec![clip(1)]).unwrap());
        prune(&dir).unwrap();
        assert_eq!(names(&dir), ["current"]);
        discard(&dir, &session(2)).unwrap();
        assert_eq!(names(&dir), ["current"], "another session's");
        discard(&dir, &session(u64::MAX)).unwrap();
        assert!(names(&dir).is_empty());
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
        // A planted symlink is never followed or written through, and clear
        // removes the link itself.
        clear(&dir).unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        assert!(save(&dir, now(), audio.clone(), vec![clip(1)]).is_err());
        assert!(load(&dir).unwrap().is_none());
        clear(&dir).unwrap();
        assert!(std::fs::symlink_metadata(&dir).is_err() && elsewhere.exists());
        save(&dir, now(), audio, vec![clip(1)]).unwrap();
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        let meta = dir.join(CURRENT).join(META);
        std::fs::remove_file(&meta).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("x"), &meta).unwrap();
        assert!(load(&dir).is_err(), "kept.json is never followed");
    }
}
