use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use xflow_core::{config::RecordingConfig, AudioCapture, AudioClip, MAX_UPLOAD_FRAMES};

enum Command {
    Start(oneshot::Sender<Result<()>>),
    Stop(oneshot::Sender<Result<AudioClip>>),
    Cancel(oneshot::Sender<Result<()>>),
    Limit(u64),
    Shutdown,
}

/// The native stream lives exclusively on a dedicated thread, including on
/// platforms where CPAL streams cannot be sent between threads.
pub struct CpalCapture {
    sender: mpsc::SyncSender<Command>,
    level: Arc<AtomicU32>,
    finished: Arc<AtomicBool>,
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

// Bound allocation even if a device advertises an unusually high rate or
// channel count. The duration remains an upper bound, never a memory promise.
const MAX_SAMPLES: usize = 16 * 1024 * 1024;

impl Recording {
    fn sample_limit(sample_rate: u32, channels: u16, max_seconds: u32) -> Result<usize> {
        let channels = usize::from(channels);
        if channels == 0 {
            bail!("invalid audio stream configuration");
        }
        let duration_frames = (sample_rate as usize)
            .checked_mul(max_seconds as usize)
            .context("recording size overflow")?;
        let frames = duration_frames
            .min(MAX_SAMPLES / channels)
            .min(MAX_UPLOAD_FRAMES);
        if frames == 0 {
            bail!("invalid audio stream configuration");
        }
        Ok(frames * channels)
    }

    fn new(sample_rate: u32, channels: u16, config: &RecordingConfig) -> Result<Self> {
        let limit = Self::sample_limit(sample_rate, channels, config.max_seconds)?;
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

/// Names reported by the audio host; enumeration never opens a microphone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    pub name: String,
    pub default: bool,
}

pub fn input_devices() -> Result<Vec<InputDevice>> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.name().ok());
    host.input_devices()
        .context("cannot enumerate microphones")?
        .map(|device| {
            let name = device.name().context("cannot inspect microphone name")?;
            Ok(InputDevice {
                default: default.as_deref() == Some(&name),
                name,
            })
        })
        .collect()
}

fn device_index(requested: &str, names: &[String]) -> Result<usize> {
    names
        .iter()
        .position(|name| name == requested)
        .with_context(|| {
            format!(
                "microphone {requested:?} not found; available devices: {}",
                names.join(", ")
            )
        })
}

struct CaptureBuffer {
    recording: Mutex<Option<Recording>>,
    active: AtomicBool,
    limited: Arc<AtomicBool>,
    first_callback_us: AtomicU64,
}

impl CaptureBuffer {
    fn take(&self) -> Result<Option<Recording>> {
        self.active.store(false, Ordering::Release);
        Ok(self
            .recording
            .lock()
            .map_err(|_| anyhow!("audio buffer lock poisoned"))?
            .take())
    }

    fn begin(&self, recording: Recording) -> Result<()> {
        *self
            .recording
            .lock()
            .map_err(|_| anyhow!("audio buffer lock poisoned"))? = Some(recording);
        self.limited.store(false, Ordering::Relaxed);
        self.active.store(true, Ordering::Release);
        Ok(())
    }

    fn fail(&self, error: String) {
        self.active.store(false, Ordering::Release);
        if let Ok(mut guard) = self.recording.lock() {
            if let Some(recording) = guard.as_mut() {
                recording.error = Some(error);
            }
        }
        self.limited.store(true, Ordering::Release);
    }
}

struct OpenStream {
    // Keep the stream on its owner thread. Dropping closes capture immediately.
    _stream: cpal::Stream,
    buffer: Arc<CaptureBuffer>,
    sample_rate: u32,
    channels: u16,
    generation: u64,
}

fn warm_deadline(config: &RecordingConfig, has_stream: bool, now: Instant) -> Option<Instant> {
    (has_stream && config.keep_warm_secs > 0)
        .then(|| now + Duration::from_secs(u64::from(config.keep_warm_secs)))
}

impl CpalCapture {
    pub fn new(config: &RecordingConfig) -> Result<Self> {
        if !(1..=600).contains(&config.max_seconds)
            || !config.silence_threshold.is_finite()
            || !(0.0..=1.0).contains(&config.silence_threshold)
            || config.keep_warm_secs > 600
        {
            bail!("invalid recording configuration");
        }
        let (sender, receiver) = mpsc::sync_channel(16);
        let callback_sender = sender.clone();
        let level = Arc::new(AtomicU32::new(0_f32.to_bits()));
        let thread_level = level.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let thread_finished = finished.clone();
        let config = config.clone();
        std::thread::Builder::new()
            .name("xflow-audio".into())
            .spawn(move || {
                let mut stream: Option<OpenStream> = None;
                let mut completed: Option<Recording> = None;
                let mut generation = 0_u64;
                let mut deadline: Option<Instant> = None;
                loop {
                    let command = match deadline {
                        Some(until) => match receiver
                            .recv_timeout(until.saturating_duration_since(Instant::now()))
                        {
                            Ok(command) => command,
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                stream = None;
                                deadline = None;
                                continue;
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        },
                        None => match receiver.recv() {
                            Ok(command) => command,
                            Err(_) => break,
                        },
                    };
                    match command {
                        Command::Start(reply) => {
                            let result = (|| {
                                if completed.is_some()
                                    || stream.as_ref().is_some_and(|s| {
                                        s.buffer.active.load(Ordering::Acquire)
                                            || s.buffer.limited.load(Ordering::Relaxed)
                                    })
                                {
                                    bail!("already recording");
                                }
                                thread_finished.store(false, Ordering::Release);
                                if let Some(open) = &stream {
                                    let started = Instant::now();
                                    open.buffer.begin(Recording::new(
                                        open.sample_rate,
                                        open.channels,
                                        &config,
                                    )?)?;
                                    tracing::debug!(
                                        warm_start_us = started.elapsed().as_micros() as u64,
                                        "microphone warm stream activated"
                                    );
                                } else {
                                    generation = generation.wrapping_add(1);
                                    stream = Some(open_stream(
                                        &config,
                                        callback_sender.clone(),
                                        thread_level.clone(),
                                        generation,
                                        thread_finished.clone(),
                                    )?);
                                }
                                deadline = None;
                                Ok(())
                            })();
                            let started = result.is_ok();
                            if reply.send(result).is_err() && started {
                                stream = None;
                                completed = None;
                                thread_level.store(0, Ordering::Relaxed);
                            }
                        }
                        Command::Stop(reply) => {
                            let buffer = if let Some(open) = &stream {
                                let us = open.buffer.first_callback_us.load(Ordering::Relaxed);
                                tracing::debug!(
                                    open_to_first_callback_us = us,
                                    "microphone startup latency"
                                );
                                let result = open.buffer.take();
                                open.buffer.limited.store(false, Ordering::Release);
                                result
                            } else {
                                Ok(None)
                            };
                            let result = buffer
                                .and_then(|buffer| {
                                    buffer.or_else(|| completed.take()).context("not recording")
                                })
                                .and_then(Recording::finish);
                            deadline = if result.is_ok() {
                                warm_deadline(&config, stream.is_some(), Instant::now())
                            } else {
                                None
                            };
                            if deadline.is_none() {
                                stream = None;
                            }
                            thread_level.store(0, Ordering::Relaxed);
                            thread_finished.store(false, Ordering::Release);
                            let _ = reply.send(result);
                        }
                        Command::Cancel(reply) => {
                            stream = None;
                            completed = None;
                            deadline = None;
                            thread_level.store(0, Ordering::Relaxed);
                            thread_finished.store(false, Ordering::Release);
                            let _ = reply.send(Ok(()));
                        }
                        Command::Limit(source) => {
                            if stream.as_ref().is_some_and(|s| {
                                s.generation == source && s.buffer.limited.load(Ordering::Acquire)
                            }) {
                                if let Some(open) = stream.take() {
                                    completed = open.buffer.take().ok().flatten();
                                }
                                deadline = None;
                                thread_level.store(0, Ordering::Relaxed);
                            }
                        }
                        Command::Shutdown => break,
                    }
                    // If the bounded wake queue was full, any queued command also
                    // observes the limit/failure flag and closes the stream.
                    if stream
                        .as_ref()
                        .is_some_and(|s| s.buffer.limited.load(Ordering::Acquire))
                    {
                        // Stop already owns the buffer if it was just requested.
                        if let Some(open) = stream.take() {
                            completed = open.buffer.take().ok().flatten();
                        }
                        deadline = None;
                        thread_level.store(0, Ordering::Relaxed);
                    }
                }
            })
            .context("cannot start audio thread")?;
        Ok(Self {
            sender,
            level,
            finished,
        })
    }
}

fn open_stream(
    config: &RecordingConfig,
    sender: mpsc::SyncSender<Command>,
    level: Arc<AtomicU32>,
    generation: u64,
    finished: Arc<AtomicBool>,
) -> Result<OpenStream> {
    let opened = Instant::now();
    let host = cpal::default_host();
    let device = if let Some(requested) = &config.device {
        let devices: Vec<_> = host
            .input_devices()
            .context("cannot enumerate microphones")?
            .collect();
        let names = devices
            .iter()
            .map(|d| d.name())
            .collect::<std::result::Result<Vec<_>, _>>()?;
        devices
            .into_iter()
            .nth(device_index(requested, &names)?)
            .context("microphone disappeared")?
    } else {
        host.default_input_device()
            .context("no default microphone; select recording.device from xflow devices")?
    };
    let supported = device
        .default_input_config()
        .context("cannot inspect microphone")?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let buffer = Arc::new(CaptureBuffer {
        recording: Mutex::new(Some(Recording::new(sample_rate, channels, config)?)),
        active: AtomicBool::new(true),
        limited: finished,
        first_callback_us: AtomicU64::new(0),
    });
    let stream_config = supported.config();
    macro_rules! build {
        ($sample:ty) => {
            build_stream::<$sample>(
                &device,
                &stream_config,
                buffer.clone(),
                sender,
                level,
                generation,
                opened,
            )
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
    Ok(OpenStream {
        _stream: stream,
        buffer,
        sample_rate,
        channels,
        generation,
    })
}

fn smooth_level(previous: f32, rms: f32, seconds: f32) -> f32 {
    let tau = if rms > previous { 0.025 } else { 0.12 };
    previous + (rms - previous) * (1.0 - (-seconds / tau).exp())
}

fn build_stream<T: SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: Arc<CaptureBuffer>,
    sender: mpsc::SyncSender<Command>,
    level: Arc<AtomicU32>,
    generation: u64,
    opened: Instant,
) -> Result<cpal::Stream>
where
    f32: FromSample<T>,
{
    let samples_per_second = config.sample_rate.0 as f32 * f32::from(config.channels);
    let limit_sender = sender.clone();
    let error_buffer = buffer.clone();
    let mut first = true;
    let mut smoothed = 0.0;
    device
        .build_input_stream(
            config,
            move |input: &[T], _| {
                if first {
                    buffer.first_callback_us.store(
                        opened.elapsed().as_micros().max(1) as u64,
                        Ordering::Relaxed,
                    );
                    first = false;
                }
                if !buffer.active.load(Ordering::Acquire) {
                    smoothed = 0.0;
                    return;
                }
                // Worker only takes this lock to swap buffers. The bounded limit queue never allocates.
                if let Ok(mut guard) = buffer.recording.lock() {
                    if let Some(recording) = guard.as_mut() {
                        let (rms, limit) = recording.push(input);
                        smoothed =
                            smooth_level(smoothed, rms, input.len() as f32 / samples_per_second);
                        level.store(smoothed.to_bits(), Ordering::Relaxed);
                        if limit {
                            buffer.active.store(false, Ordering::Release);
                            buffer.limited.store(true, Ordering::Release);
                            let _ = limit_sender.try_send(Command::Limit(generation));
                        }
                    }
                }
            },
            move |error| {
                error_buffer.fail(error.to_string());
                // Never block a CPAL callback on the worker that drops its stream.
                let _ = sender.try_send(Command::Limit(generation));
            },
            Some(Duration::from_secs(3)),
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
    fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
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
    fn device_matching_is_exact_and_lists_available_names() {
        let names = vec!["Default".into(), "USB Microphone".into()];
        assert_eq!(device_index("USB Microphone", &names).unwrap(), 1);
        let error = device_index("usb", &names).unwrap_err().to_string();
        assert!(error.contains("USB Microphone") && error.contains("Default"));
        assert!(device_index("missing", &[]).is_err());
    }
    #[test]
    fn warm_buffers_discard_idle_samples_and_restart_cleanly() {
        let buffer = CaptureBuffer {
            recording: Mutex::new(None),
            active: AtomicBool::new(false),
            limited: Arc::new(AtomicBool::new(false)),
            first_callback_us: AtomicU64::new(0),
        };
        assert!(buffer.take().unwrap().is_none());
        buffer
            .begin(Recording::new(4, 1, &RecordingConfig::default()).unwrap())
            .unwrap();
        buffer
            .recording
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .push(&[0.5_f32]);
        assert_eq!(
            buffer.take().unwrap().unwrap().finish().unwrap().samples,
            vec![0.5]
        );
        assert!(!buffer.active.load(Ordering::Acquire));
        assert!(buffer.recording.lock().unwrap().is_none());
        buffer
            .begin(Recording::new(4, 1, &RecordingConfig::default()).unwrap())
            .unwrap();
        assert!(buffer
            .recording
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .samples
            .is_empty());
        let now = Instant::now();
        assert_eq!(warm_deadline(&RecordingConfig::default(), true, now), None);
        let config = RecordingConfig {
            keep_warm_secs: 2,
            ..Default::default()
        };
        assert_eq!(
            warm_deadline(&config, true, now),
            Some(now + Duration::from_secs(2))
        );
        assert_eq!(warm_deadline(&config, false, now), None);
    }
    #[test]
    fn levels_attack_quickly_and_decay_smoothly() {
        let attack = smooth_level(0.0, 0.5, 1.0 / 30.0);
        assert!(attack > 0.3 && attack < 0.5);
        let release = smooth_level(attack, 0.0, 1.0 / 30.0);
        assert!(release > 0.0 && release < attack);
    }
    #[test]
    fn stream_failure_survives_a_full_wake_queue() {
        let buffer = CaptureBuffer {
            recording: Mutex::new(Some(
                Recording::new(4, 1, &RecordingConfig::default()).unwrap(),
            )),
            active: AtomicBool::new(true),
            limited: Arc::new(AtomicBool::new(false)),
            first_callback_us: AtomicU64::new(0),
        };
        buffer
            .recording
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .push(&[0.5_f32]);
        let (sender, _receiver) = mpsc::sync_channel(1);
        sender.try_send(Command::Limit(1)).unwrap();
        buffer.fail("device disconnected".into());
        assert!(sender.try_send(Command::Limit(1)).is_err());
        assert!(buffer.limited.load(Ordering::Acquire));
        assert!(!buffer.active.load(Ordering::Acquire));
        assert!(buffer
            .take()
            .unwrap()
            .unwrap()
            .finish()
            .unwrap_err()
            .to_string()
            .contains("device disconnected"));
    }
    #[test]
    fn recording_is_bounded_and_normalizes_nonfinite_samples() {
        let mut recording = Recording::new(
            4,
            1,
            &RecordingConfig {
                max_seconds: 1,
                silence_threshold: 0.1,
                ..Default::default()
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
            ..Default::default()
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

    #[test]
    fn high_rate_capture_stays_within_upload_memory_and_whole_frame_limits() {
        assert_eq!(
            Recording::sample_limit(192_000, 1, 120).unwrap(),
            MAX_UPLOAD_FRAMES
        );
        assert_eq!(
            Recording::sample_limit(48_000, 1, 600).unwrap(),
            MAX_UPLOAD_FRAMES
        );
        assert_eq!(
            Recording::sample_limit(48_000, 2, 120).unwrap(),
            48_000 * 2 * 120
        );
        assert_eq!(
            Recording::sample_limit(192_000, 32, 600).unwrap(),
            MAX_SAMPLES
        );
        let three_channel_limit = Recording::sample_limit(192_000, 3, 600).unwrap();
        assert_eq!(three_channel_limit, (MAX_SAMPLES / 3) * 3);
        assert_eq!(three_channel_limit % 3, 0);
        assert!(Recording::sample_limit(48_000, 0, 120).is_err());
    }
    #[test]
    fn completion_flag_keeps_buffer_and_device_error_until_retrieval() {
        let finished = Arc::new(AtomicBool::new(false));
        let buffer = CaptureBuffer {
            recording: Mutex::new(None),
            active: AtomicBool::new(false),
            limited: finished.clone(),
            first_callback_us: AtomicU64::new(0),
        };
        let (sender, _receiver) = mpsc::sync_channel(16);
        let capture = CpalCapture {
            sender,
            level: Arc::new(AtomicU32::new(0)),
            finished,
        };
        buffer
            .begin(Recording::new(2, 1, &RecordingConfig::default()).unwrap())
            .unwrap();
        assert!(!capture.finished());
        buffer.fail("fixture CPAL device error".into());
        assert!(capture.finished());
        assert!(buffer
            .take()
            .unwrap()
            .unwrap()
            .finish()
            .unwrap_err()
            .to_string()
            .contains("fixture CPAL device error"));
        buffer
            .begin(Recording::new(2, 1, &RecordingConfig::default()).unwrap())
            .unwrap();
        assert!(!capture.finished());
        let mut lock = buffer.recording.lock().unwrap();
        let recording = lock.as_mut().unwrap();
        recording.push(&[0.5_f32; 2]);
        drop(lock);
        buffer.limited.store(true, Ordering::Release);
        assert!(capture.finished());
        assert_eq!(
            buffer
                .take()
                .unwrap()
                .unwrap()
                .finish()
                .unwrap()
                .samples
                .len(),
            2
        );
    }
}
