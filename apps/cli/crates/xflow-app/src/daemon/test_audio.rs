//! Debug-only capture: native desktop and providers stay on the normal path.
use super::*;
use async_trait::async_trait;
use tokio::{io::AsyncReadExt, sync::Mutex};

pub(super) struct WavCapture {
    path: PathBuf,
    clip: Mutex<Option<AudioClip>>,
}

impl WavCapture {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            clip: Mutex::new(None),
        }
    }
}

#[async_trait]
impl AudioCapture for WavCapture {
    async fn start(&self) -> Result<()> {
        let mut current = self.clip.lock().await;
        if current.is_some() {
            bail!("test audio is already recording");
        }
        let clip = async {
            if !tokio::fs::metadata(&self.path).await?.is_file() {
                bail!("WAV must be a regular file");
            }
            let file = tokio::fs::File::open(&self.path).await?;
            let mut bytes = Vec::new();
            file.take(xflow_core::MAX_UPLOAD_WAV_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.is_empty() || bytes.len() > xflow_core::MAX_UPLOAD_WAV_BYTES {
                bail!("WAV is empty or exceeds 25 MB");
            }
            tokio::task::spawn_blocking(move || -> Result<AudioClip> {
                let clip = xflow_providers::decode_wav(&bytes)?;
                if clip.samples.is_empty()
                    || !(8_000..=384_000).contains(&clip.sample_rate)
                    || !(1..=32).contains(&clip.channels)
                    || !clip
                        .samples
                        .len()
                        .is_multiple_of(usize::from(clip.channels))
                    || clip.samples.iter().any(|s| !s.is_finite())
                {
                    bail!("invalid or empty PCM audio");
                }
                Ok(clip)
            })
            .await?
        }
        .await
        .with_context(|| format!("XFLOW_TEST_AUDIO: cannot load {}", self.path.display()))?;
        *current = Some(clip);
        Ok(())
    }

    async fn stop(&self) -> Result<AudioClip> {
        self.clip
            .lock()
            .await
            .take()
            .context("no test audio recording is active")
    }

    async fn cancel(&self) -> Result<()> {
        self.clip.lock().await.take();
        Ok(())
    }

    fn level(&self) -> f32 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wav_capture_reloads_preserves_pcm_and_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio.wav");
        let clip = AudioClip {
            samples: vec![0.25; 16_000],
            sample_rate: 16_000,
            channels: 1,
        };
        let bytes = xflow_providers::encode_wav(&clip).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let capture = WavCapture::new(path.clone());
        assert!(capture.stop().await.is_err());
        capture.start().await.unwrap();
        assert!(capture.start().await.is_err());
        let expected = xflow_providers::decode_wav(&bytes).unwrap();
        let actual = capture.stop().await.unwrap();
        assert_eq!(actual.samples, expected.samples);
        assert_eq!(actual.sample_rate, expected.sample_rate);
        assert_eq!(actual.channels, expected.channels);
        capture.start().await.unwrap();
        capture.cancel().await.unwrap();
        assert!(capture.stop().await.is_err());
        std::fs::write(&path, b"broken WAV").unwrap();
        let error = capture.start().await.unwrap_err();
        assert!(format!("{error:#}").contains("XFLOW_TEST_AUDIO"));
        assert!(capture.stop().await.is_err());
        std::fs::write(&path, bytes).unwrap();
        capture.start().await.unwrap();
        capture.stop().await.unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(capture.start().await.is_err());
        std::fs::write(&path, []).unwrap();
        assert!(capture.start().await.is_err());
        let mut invalid = xflow_providers::encode_wav(&clip).unwrap();
        invalid[24..28].copy_from_slice(&0_u32.to_le_bytes());
        std::fs::write(&path, invalid).unwrap();
        assert!(capture.start().await.is_err());
        let mut nonfinite = xflow_providers::encode_wav(&clip).unwrap();
        nonfinite[20..22].copy_from_slice(&3_u16.to_le_bytes());
        nonfinite[28..32].copy_from_slice(&64_000_u32.to_le_bytes());
        nonfinite[32..34].copy_from_slice(&4_u16.to_le_bytes());
        nonfinite[34..36].copy_from_slice(&32_u16.to_le_bytes());
        nonfinite[44..48].copy_from_slice(&f32::NAN.to_le_bytes());
        std::fs::write(&path, nonfinite).unwrap();
        assert!(capture.start().await.is_err());
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(xflow_core::MAX_UPLOAD_WAV_BYTES as u64 + 1)
            .unwrap();
        assert!(capture.start().await.is_err());
        assert!(WavCapture::new(dir.path().into()).start().await.is_err());
    }

    #[tokio::test]
    #[ignore = "local measurement: set XFLOW_TEST_BENCH_WAV to a generated speech WAV"]
    async fn wav_capture_latency_and_fidelity() {
        let path = PathBuf::from(std::env::var_os("XFLOW_TEST_BENCH_WAV").unwrap());
        let expected = xflow_providers::decode_wav(&std::fs::read(&path).unwrap()).unwrap();
        let capture = WavCapture::new(path);
        let mut times = Vec::new();
        for _ in 0..100 {
            let start = Instant::now();
            capture.start().await.unwrap();
            let actual = capture.stop().await.unwrap();
            times.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(actual.samples, expected.samples);
            assert_eq!(actual.sample_rate, expected.sample_rate);
            assert_eq!(actual.channels, expected.channels);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "WAV capture: {} ms audio, n=100, p50={:.3} ms, p95={:.3} ms, PCM differences=0",
            super::super::audio_duration(&expected).unwrap(),
            times[49],
            times[94]
        );
    }
}
