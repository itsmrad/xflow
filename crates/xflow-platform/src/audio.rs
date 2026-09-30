use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    mpsc, Arc, Mutex,
};
use tokio::sync::oneshot;
use xflow_core::{config::RecordingConfig, AudioCapture, AudioClip};

enum Command {
    Start(oneshot::Sender<Result<()>>),
    Stop(oneshot::Sender<Result<AudioClip>>),
    Cancel(oneshot::Sender<Result<()>>),
    Limit,
    Failed(String),
    Shutdown,
}

/// The native stream lives exclusively on a dedicated thread, including on
/// platforms where CPAL streams cannot be sent between threads.
pub struct CpalCapture {
    sender: mpsc::Sender<Command>,
    level: Arc<AtomicU32>,
}

struct Recording {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
    limit: usize,
    threshold: f32,
    voiced: bool,
    complete: bool,
    error: Option<String>,
}

impl Recording {
    fn new(sample_rate: u32, channels: u16, config: &RecordingConfig) -> Result<Self> {
        let limit = (sample_rate as usize)
            .checked_mul(channels as usize)
            .and_then(|n| n.checked_mul(config.max_seconds as usize))
            .context("recording size overflow")?;
        if limit == 0 {
            bail!("invalid audio stream configuration");
        }
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(limit)
            .context("cannot allocate recording buffer")?;
        Ok(Self {
            samples,
            sample_rate,
            channels,
            limit,
            threshold: config.silence_threshold,
            voiced: false,
            complete: false,
            error: None,
        })
    }

    fn push<T: Sample>(&mut self, input: &[T]) -> (f32, bool)
    where
        f32: FromSample<T>,
    {
        if self.complete {
            return (0.0, false);
        }
        let available = self.limit - self.samples.len();
        let input = &input[..input.len().min(available)];
        let mut energy = 0.0_f64;
        for sample in input {
            let value: f32 = sample.to_sample();
            let value = if value.is_finite() {
                value.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            energy += f64::from(value).powi(2);
            self.samples.push(value);
        }
        let rms = if input.is_empty() {
            0.0
        } else {
            (energy / input.len() as f64).sqrt() as f32
        };
        self.voiced |= rms > self.threshold;
        self.complete = self.samples.len() == self.limit;
        (rms, self.complete)
    }

    fn finish(self) -> Result<AudioClip> {
        if let Some(error) = self.error {
            bail!("microphone stream failed: {error}");
        }
        if !self.voiced {
            bail!("no speech detected; check microphone or silence threshold");
        }
        Ok(AudioClip {
            samples: self.samples,
            sample_rate: self.sample_rate,
            channels: self.channels,
        })
    }
}

impl CpalCapture {
    pub fn new(config: &RecordingConfig) -> Result<Self> {
        if !(1..=600).contains(&config.max_seconds)
            || !config.silence_threshold.is_finite()
            || !(0.0..=1.0).contains(&config.silence_threshold)
        {
            bail!("invalid recording configuration");
        }
        let (sender, receiver) = mpsc::channel();
        let callback_sender = sender.clone();
        let level = Arc::new(AtomicU32::new(0_f32.to_bits()));
        let thread_level = level.clone();
        let config = config.clone();
        std::thread::Builder::new()
            .name("xflow-audio".into())
            .spawn(move || {
                let mut stream = None;
                let mut recording: Option<Arc<Mutex<Recording>>> = None;
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Start(reply) => {
                            let result = if recording.is_some() {
                                Err(anyhow!("already recording"))
                            } else {
                                open_stream(&config, callback_sender.clone(), thread_level.clone())
                                    .map(|(new_stream, buffer)| {
                                        stream = Some(new_stream);
                                        recording = Some(buffer);
                                    })
                            };
                            // A cancelled start request must not leave the microphone open.
                            if reply.send(result).is_err() {
                                stream = None;
                                recording = None;
                            }
                        }
                        Command::Stop(reply) => {
                            stream = None;
                            thread_level.store(0_f32.to_bits(), Ordering::Relaxed);
                            let result =
                                recording
                                    .take()
                                    .context("not recording")
                                    .and_then(|buffer| {
                                        Arc::try_unwrap(buffer)
                                            .map_err(|_| anyhow!("audio callback did not stop"))?
                                            .into_inner()
                                            .map_err(|_| anyhow!("audio buffer lock poisoned"))?
                                            .finish()
                                    });
                            let _ = reply.send(result);
                        }
                        Command::Cancel(reply) => {
                            stream = None;
                            recording = None;
                            thread_level.store(0_f32.to_bits(), Ordering::Relaxed);
                            let _ = reply.send(Ok(()));
                        }
                        Command::Limit => {
                            stream = None;
                            thread_level.store(0_f32.to_bits(), Ordering::Relaxed);
                        }
                        Command::Failed(error) => {
                            stream = None;
                            if let Some(buffer) = &recording {
                                if let Ok(mut buffer) = buffer.lock() {
                                    buffer.error = Some(error);
                                }
                            }
                            thread_level.store(0_f32.to_bits(), Ordering::Relaxed);
                        }
                        Command::Shutdown => break,
                    }
                }
            })
            .context("cannot start audio thread")?;
        Ok(Self { sender, level })
    }
}

fn open_stream(
    config: &RecordingConfig,
    sender: mpsc::Sender<Command>,
    level: Arc<AtomicU32>,
) -> Result<(cpal::Stream, Arc<Mutex<Recording>>)> {
    let device = cpal::default_host()
        .default_input_device()
        .context("no default microphone")?;
    let supported = device
        .default_input_config()
        .context("cannot inspect microphone")?;
    let buffer = Arc::new(Mutex::new(Recording::new(
        supported.sample_rate().0,
        supported.channels(),
        config,
    )?));
    let stream_config = supported.config();
    macro_rules! build {
        ($sample:ty) => {
            build_stream::<$sample>(&device, &stream_config, buffer.clone(), sender, level)
        };
    }
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::F64 => build!(f64),
        cpal::SampleFormat::I8 => build!(i8),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::I32 => build!(i32),
        cpal::SampleFormat::I64 => build!(i64),
        cpal::SampleFormat::U8 => build!(u8),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::U32 => build!(u32),
        cpal::SampleFormat::U64 => build!(u64),
        other => bail!("unsupported microphone sample format {other}"),
    }?;
    stream.play().context("cannot start microphone")?;
    Ok((stream, buffer))
}

fn build_stream<T: SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: Arc<Mutex<Recording>>,
    sender: mpsc::Sender<Command>,
    level: Arc<AtomicU32>,
) -> Result<cpal::Stream>
where
    f32: FromSample<T>,
{
    let error_sender = sender.clone();
    device
        .build_input_stream(
            config,
            move |input: &[T], _| {
                if let Ok(mut buffer) = buffer.lock() {
                    let (rms, reached_limit) = buffer.push(input);
                    level.store(rms.to_bits(), Ordering::Relaxed);
                    if reached_limit {
                        let _ = sender.send(Command::Limit);
                    }
                }
            },
            move |error| {
                let _ = error_sender.send(Command::Failed(error.to_string()));
            },
            Some(std::time::Duration::from_secs(3)),
        )
        .context("cannot open microphone stream")
}

#[async_trait]
impl AudioCapture for CpalCapture {
    async fn start(&self) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::Start(reply))
            .context("audio thread stopped")?;
        receive.await.context("audio thread stopped")?
    }
    async fn stop(&self) -> Result<AudioClip> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::Stop(reply))
            .context("audio thread stopped")?;
        receive.await.context("audio thread stopped")?
    }
    async fn cancel(&self) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::Cancel(reply))
            .context("audio thread stopped")?;
        receive.await.context("audio thread stopped")?
    }
    fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}

impl Drop for CpalCapture {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_is_bounded_and_normalizes_nonfinite_samples() {
        let mut recording = Recording::new(
            4,
            1,
            &RecordingConfig {
                max_seconds: 1,
                silence_threshold: 0.1,
            },
        )
        .unwrap();
        let (rms, limit) = recording.push(&[f32::NAN, f32::INFINITY, 2.0, -2.0, 0.1]);
        assert!(rms.is_finite());
        assert!(limit);
        assert!(!recording.push(&[1.0]).1);
        assert_eq!(
            recording.finish().unwrap().samples,
            vec![0.0, 0.0, 1.0, -1.0]
        );
    }
    #[test]
    fn vad_rejects_silence_without_removing_internal_pauses() {
        let config = RecordingConfig {
            max_seconds: 1,
            silence_threshold: 0.1,
        };
        let mut silent = Recording::new(4, 1, &config).unwrap();
        silent.push(&[0.01_f32; 4]);
        assert!(silent.finish().is_err());
        let mut voiced = Recording::new(4, 1, &config).unwrap();
        voiced.push(&[0.5_f32, 0.0, 0.0, 0.5]);
        assert_eq!(voiced.finish().unwrap().samples.len(), 4);
    }
    #[test]
    fn integer_pcm_is_scaled() {
        let mut recording = Recording::new(2, 1, &RecordingConfig::default()).unwrap();
        recording.push(&[i16::MIN, 0, i16::MAX]);
        let clip = recording.finish().unwrap();
        assert_eq!(clip.samples[0], -1.0);
        assert_eq!(clip.samples[1], 0.0);
        assert!(clip.samples[2] > 0.99);
    }
}
