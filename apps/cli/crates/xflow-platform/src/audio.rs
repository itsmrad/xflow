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
use xflow_core::{
    config::{RecordingConfig, MAX_RECORDING_SECS},
    AudioCapture, AudioClip,
};

enum Command {
    Start(oneshot::Sender<Result<()>>),
    Stop(oneshot::Sender<Result<AudioClip>>),
    Cancel(oneshot::Sender<Result<()>>),
    Drain(oneshot::Sender<Result<Option<AudioClip>>>),
    Limit(u64),
    Shutdown,
}

/// The native stream lives exclusively on a dedicated thread, including on
/// platforms where CPAL streams cannot be sent between threads.
pub struct CpalCapture {
    sender: mpsc::SyncSender<Command>,
    level: Arc<AtomicU32>,
    finished: Arc<AtomicBool>,
    microphone: Arc<Mutex<Option<String>>>,
}

struct Recording {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
    limit: usize,
    reserve: usize,
    complete: bool,
    error: Option<String>,
}

// Bound allocation even if a device advertises an unusually high rate or
// channel count. The duration remains an upper bound, never a memory promise.
// The daemon drains the buffer every second, so this only stops a capture
// nobody drains; the dictation length is the daemon's `max_seconds` deadline.
const MAX_SAMPLES: usize = 16 * 1024 * 1024;
// Preallocated per buffer so the audio callback rarely reallocates between drains.
const RESERVE_SECONDS: usize = 10;

impl Recording {
    fn sample_limit(sample_rate: u32, channels: u16, max_seconds: u32) -> Result<usize> {
        let channels = usize::from(channels);
        if channels == 0 {
            bail!("invalid audio stream configuration");
        }
        let duration_frames = (sample_rate as usize)
            .checked_mul(max_seconds as usize)
            .context("recording size overflow")?;
        let frames = duration_frames.min(MAX_SAMPLES / channels);
        if frames == 0 {
            bail!("invalid audio stream configuration");
        }
        Ok(frames * channels)
    }

    fn new(sample_rate: u32, channels: u16, config: &RecordingConfig) -> Result<Self> {
        let limit = Self::sample_limit(sample_rate, channels, config.max_seconds)?;
        let reserve = limit.min(sample_rate as usize * usize::from(channels) * RESERVE_SECONDS);
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(reserve)
            .context("cannot allocate recording buffer")?;
        Ok(Self {
            samples,
            sample_rate,
            channels,
            limit,
            reserve,
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
        self.complete = self.samples.len() == self.limit;
        (rms, self.complete)
    }

    /// Hands over everything captured so far and keeps recording into `fresh`.
    fn drain(&mut self, fresh: Vec<f32>) -> AudioClip {
        AudioClip {
            samples: std::mem::replace(&mut self.samples, fresh),
            sample_rate: self.sample_rate,
            channels: self.channels,
        }
    }

    fn finish(self) -> Result<AudioClip> {
        if let Some(error) = self.error {
            bail!("microphone stream failed: {error}");
        }
        Ok(AudioClip {
            samples: self.samples,
            sample_rate: self.sample_rate,
            channels: self.channels,
        })
    }
}

/// Input names; macOS enumeration uses properties only, while ALSA may probe devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    pub name: String,
    pub default: bool,
}

pub fn input_devices() -> Result<Vec<InputDevice>> {
    #[cfg(target_os = "macos")]
    {
        Ok(mac_inputs()?
            .into_iter()
            .map(|input| InputDevice {
                name: input.candidate.name,
                default: input.candidate.is_default,
            })
            .collect())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let host = cpal::default_host();
        let default = host.default_input_device().and_then(|d| d.name().ok());
        host.input_devices()
            .context("cannot enumerate microphones")?
            .map(|device| {
                let name = device.name().context("cannot inspect microphone name")?;
                Ok(InputDevice {
                    default: default.as_ref() == Some(&name),
                    name,
                })
            })
            .collect()
    }
}

struct DeviceChoice {
    // None selects the inventory's default device by ID.
    index: Option<usize>,
    device_id: Option<u32>,
    name: String,
    is_default: bool,
    notice: Option<String>,
}

impl DeviceChoice {
    #[cfg(any(target_os = "macos", test))]
    fn echo_unavailable(&mut self) {
        let notice = "Echo cancellation is off; voice processing unavailable";
        self.notice = Some(match self.notice.take() {
            Some(existing) => format!("{existing}; {notice}"),
            None => notice.into(),
        });
    }

    fn description(&self) -> String {
        match &self.notice {
            Some(notice) => format!("{} ({notice})", self.name),
            None => self.name.clone(),
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    BuiltIn,
    Bluetooth,
    BluetoothLe,
    Virtual,
    Aggregate,
    Other,
}

#[cfg(any(target_os = "macos", test))]
struct Candidate {
    name: String,
    transport: Transport,
    is_default: bool,
}

#[cfg(any(target_os = "macos", test))]
fn resolve_device(devices: &[Candidate], config: &RecordingConfig) -> Result<DeviceChoice> {
    let default = devices
        .iter()
        .find(|d| d.is_default)
        .or_else(|| devices.iter().find(|d| d.transport == Transport::BuiltIn))
        .or_else(|| devices.iter().find(|d| d.transport == Transport::Other))
        .or_else(|| devices.first())
        .context("no usable microphone; connect an input or retry after devices settle")?;
    let named = config
        .device
        .as_ref()
        .and_then(|name| devices.iter().position(|d| &d.name == name));
    let duplicate = |name: &str| devices.iter().filter(|d| d.name == name).count() > 1;
    let mut ambiguous = named.is_some_and(|index| duplicate(&devices[index].name));
    let named = named.filter(|_| !ambiguous);
    let (index, selected) = if let Some(index) = named {
        (Some(index), &devices[index])
    } else {
        let avoid = matches!(default.transport, Transport::Virtual | Transport::Aggregate)
            || (config.avoid_bluetooth
                && matches!(
                    default.transport,
                    Transport::Bluetooth | Transport::BluetoothLe
                ));
        let builtin = devices
            .iter()
            .enumerate()
            .find(|(_, d)| avoid && d.transport == Transport::BuiltIn);
        if let Some((index, device)) = builtin {
            (Some(index), device)
        } else {
            ambiguous |= duplicate(&default.name);
            (None, default)
        }
    };
    let mut notices = Vec::new();
    if ambiguous {
        notices.push(format!(
            "duplicate microphone name; using {}",
            selected.name
        ));
    }
    if let Some(name) = &config.device {
        if named.is_none() && !ambiguous {
            notices.push(format!(
                "recording.device {name:?} not found — using {}",
                selected.name
            ));
        }
    }
    if !selected.is_default && named.is_none() && default.is_default {
        if matches!(
            default.transport,
            Transport::Bluetooth | Transport::BluetoothLe
        ) {
            notices.push("Bluetooth headphones kept in high quality".into());
        } else {
            notices.push("virtual/aggregate default bypassed".into());
        }
    }
    if !default.is_default && named.is_none() {
        notices.push(format!(
            "system default microphone unavailable — using {}",
            selected.name
        ));
    }
    if config.ignore_speakers && (!selected.is_default || config.device.is_some()) {
        notices.push("Echo cancellation is off while using a selected microphone".into());
    }
    Ok(DeviceChoice {
        index: index.or_else(|| {
            (!selected.is_default).then(|| {
                devices
                    .iter()
                    .position(|d| std::ptr::eq(d, selected))
                    .unwrap()
            })
        }),
        device_id: None,
        name: selected.name.clone(),
        is_default: selected.is_default,
        notice: (!notices.is_empty()).then(|| notices.join("; ")),
    })
}

#[cfg(any(target_os = "macos", test))]
struct NativeInput<D> {
    id: u32,
    device: D,
    candidate: Candidate,
}

#[cfg(any(target_os = "macos", test))]
fn usable_inputs<D>(
    devices: impl IntoIterator<Item = (u32, D)>,
    mut inspect: impl FnMut(u32) -> Result<Option<Candidate>>,
) -> Result<Vec<NativeInput<D>>> {
    let inputs: Vec<_> = devices
        .into_iter()
        .filter_map(|(id, device)| match inspect(id) {
            Ok(Some(candidate)) => Some(NativeInput {
                id,
                device,
                candidate,
            }),
            Ok(None) => None,
            Err(error) => {
                tracing::debug!(device_id = id, %error, "skipping unavailable microphone");
                None
            }
        })
        .collect();
    if inputs.is_empty() {
        bail!("no usable microphone; connect an input or retry after devices settle");
    }
    Ok(inputs)
}

#[cfg(target_os = "macos")]
fn inspect_input(id: u32, default_id: Option<u32>) -> Result<Option<Candidate>> {
    use coreaudio::{audio_unit::macos_helpers, sys};
    // CPAL input_devices() constructs AudioUnits; stream-ID properties do not.
    let address = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioDevicePropertyStreams,
        mScope: sys::kAudioObjectPropertyScopeInput,
        mElement: sys::kAudioObjectPropertyElementMaster,
    };
    let mut size = 0;
    let status = unsafe {
        sys::AudioObjectGetPropertyDataSize(id, &address, 0, std::ptr::null(), &mut size)
    };
    if status != 0 {
        bail!("cannot inspect input streams: CoreAudio {status}");
    }
    if size == 0 {
        return Ok(None);
    }
    // shortcut: coreaudio-rs retains each name CFString; replace its reader when ownership is fixed.
    let name = macos_helpers::get_device_name(id)?;
    let address = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioDevicePropertyTransportType,
        mScope: sys::kAudioObjectPropertyScopeGlobal,
        mElement: sys::kAudioObjectPropertyElementMaster,
    };
    let mut value = 0_u32;
    let mut size = std::mem::size_of_val(&value) as u32;
    let status = unsafe {
        sys::AudioObjectGetPropertyData(
            id,
            &address,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut u32).cast(),
        )
    };
    if status != 0 {
        bail!("cannot inspect input transport: CoreAudio {status}");
    }
    let transport = match value {
        sys::kAudioDeviceTransportTypeBuiltIn => Transport::BuiltIn,
        sys::kAudioDeviceTransportTypeBluetooth => Transport::Bluetooth,
        sys::kAudioDeviceTransportTypeBluetoothLE => Transport::BluetoothLe,
        sys::kAudioDeviceTransportTypeVirtual => Transport::Virtual,
        sys::kAudioDeviceTransportTypeAggregate => Transport::Aggregate,
        _ => Transport::Other,
    };
    Ok(Some(Candidate {
        name,
        transport,
        is_default: default_id == Some(id),
    }))
}

#[cfg(any(target_os = "macos", test))]
fn stable_inputs<D>(
    mut read: impl FnMut() -> Result<Option<Vec<NativeInput<D>>>>,
    default: impl FnOnce() -> Result<NativeInput<D>>,
) -> Result<Vec<NativeInput<D>>> {
    for _ in 0..2 {
        match read() {
            Ok(Some(inputs)) => return Ok(inputs),
            Ok(None) => {}
            Err(error) => tracing::debug!(%error, "microphone inventory unavailable"),
        }
    }
    tracing::debug!("audio device inventory changed; using the system default");
    Ok(vec![default()?])
}

#[cfg(target_os = "macos")]
fn current_default(host: &cpal::Host) -> Result<(u32, cpal::Device)> {
    use coreaudio::audio_unit::macos_helpers;
    let id = macos_helpers::get_default_device_id(true).context("no default microphone")?;
    let device = host
        .default_input_device()
        .context("no default microphone")?;
    if macos_helpers::get_default_device_id(true) != Some(id) {
        bail!("default microphone changed during enumeration");
    }
    Ok((id, device))
}

#[cfg(target_os = "macos")]
fn mac_inputs() -> Result<Vec<NativeInput<cpal::Device>>> {
    use coreaudio::audio_unit::macos_helpers;
    let host = cpal::default_host();
    // CPAL 0.15.3 devices() preserves kAudioHardwarePropertyDevices order, but hides IDs.
    // Verify the surrounding snapshots so a topology change cannot pair the wrong devices.
    stable_inputs(
        || {
            let ids = macos_helpers::get_audio_device_ids()?;
            let devices: Vec<_> = host
                .devices()
                .context("cannot enumerate audio devices")?
                .collect();
            if ids != macos_helpers::get_audio_device_ids()? || ids.len() != devices.len() {
                return Ok(None);
            }
            let default_id = macos_helpers::get_default_device_id(true);
            Ok(Some(usable_inputs(ids.into_iter().zip(devices), |id| {
                inspect_input(id, default_id)
            })?))
        },
        || {
            let (id, device) = current_default(&host)?;
            let candidate =
                inspect_input(id, Some(id))?.context("default microphone has no input streams")?;
            Ok(NativeInput {
                id,
                device,
                candidate,
            })
        },
    )
}

#[cfg(any(target_os = "macos", test))]
fn prefer_default_handle<D>(
    (mut device, mut choice): (D, DeviceChoice),
    default: Option<(u32, D)>,
    config: &RecordingConfig,
) -> (D, DeviceChoice) {
    match default {
        Some((id, default)) if choice.device_id == Some(id) => device = default,
        _ => {
            if choice.is_default && config.ignore_speakers && config.device.is_none() {
                choice.echo_unavailable();
            }
            choice.is_default = false;
        }
    }
    (device, choice)
}

#[cfg(any(target_os = "macos", test))]
fn choose_input<D>(
    inputs: Vec<NativeInput<D>>,
    config: &RecordingConfig,
) -> Result<(D, DeviceChoice)> {
    let ids: Vec<_> = inputs.iter().map(|input| input.id).collect();
    let (devices, candidates): (Vec<_>, Vec<_>) = inputs
        .into_iter()
        .map(|input| (input.device, input.candidate))
        .unzip();
    let mut choice = resolve_device(&candidates, config)?;
    let index = choice
        .index
        .or_else(|| candidates.iter().position(|d| d.is_default))
        .context("no default microphone")?;
    choice.device_id = Some(ids[index]);
    Ok((
        devices
            .into_iter()
            .nth(index)
            .context("microphone disappeared")?,
        choice,
    ))
}

#[cfg(target_os = "macos")]
fn select_device(config: &RecordingConfig) -> Result<(cpal::Device, DeviceChoice)> {
    let started = Instant::now();
    let selected = choose_input(mac_inputs()?, config)?;
    // CPAL's default handle avoids its disconnect-listener reference cycle.
    let selected = prefer_default_handle(
        selected,
        current_default(&cpal::default_host()).ok(),
        config,
    );
    tracing::debug!(resolution_us = started.elapsed().as_micros() as u64, device_id = ?selected.1.device_id, "microphone resolved");
    Ok(selected)
}

#[cfg(not(target_os = "macos"))]
fn select_device(config: &RecordingConfig) -> Result<(cpal::Device, DeviceChoice)> {
    let host = cpal::default_host();
    let device = if let Some(name) = &config.device {
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
            .nth(device_index(name, &names)?)
            .context("microphone disappeared")?
    } else {
        host.default_input_device()
            .context("no default microphone; select recording.device from xflow devices")?
    };
    let choice = DeviceChoice {
        index: None,
        device_id: None,
        name: device.name().unwrap_or_else(|_| {
            config
                .device
                .clone()
                .unwrap_or_else(|| "System default microphone (name unavailable)".into())
        }),
        is_default: config.device.is_none(),
        notice: None,
    };
    Ok((device, choice))
}

fn resolve_on_start(has_stream: bool) -> bool {
    cfg!(target_os = "macos") || !has_stream
}

/// Preview of the next capture choice. macOS uses read-only properties;
/// Linux/ALSA may briefly open devices while enumerating their formats.
pub fn selected_microphone(config: &RecordingConfig) -> Result<String> {
    Ok(select_device(config)?.1.description())
}

#[cfg(any(not(target_os = "macos"), test))]
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

    fn open(
        sample_rate: u32,
        channels: u16,
        config: &RecordingConfig,
        limited: Arc<AtomicBool>,
    ) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            recording: Mutex::new(Some(Recording::new(sample_rate, channels, config)?)),
            active: AtomicBool::new(true),
            limited,
            first_callback_us: AtomicU64::new(0),
        }))
    }

    fn drain(&self) -> Result<Option<AudioClip>> {
        let reserve = match self
            .recording
            .lock()
            .map_err(|_| anyhow!("audio buffer lock poisoned"))?
            .as_ref()
        {
            Some(recording) if !recording.samples.is_empty() => recording.reserve,
            _ => return Ok(None),
        };
        // Allocate outside the lock the audio callback also takes.
        let mut fresh = Vec::new();
        fresh
            .try_reserve_exact(reserve)
            .context("cannot allocate recording buffer")?;
        let mut guard = self
            .recording
            .lock()
            .map_err(|_| anyhow!("audio buffer lock poisoned"))?;
        Ok(guard.as_mut().map(|recording| recording.drain(fresh)))
    }

    fn fail(&self, error: String) {
        self.active.store(false, Ordering::Release);
        let mut guard = self
            .recording
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(recording) = guard.as_mut() {
            recording.error = Some(error);
            self.limited.store(true, Ordering::Release);
        }
    }
}

#[cfg(target_os = "macos")]
struct PauseOnDrop<S: StreamTrait>(S);

#[cfg(target_os = "macos")]
impl<S: StreamTrait> Drop for PauseOnDrop<S> {
    fn drop(&mut self) {
        // Only default CPAL handles reach this guard; non-default inputs use an owned HAL unit.
        if let Err(error) = self.0.pause() {
            tracing::warn!(%error, "cannot stop microphone stream");
        }
    }
}

#[cfg(target_os = "macos")]
static LIVE_HAL_UNITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(target_os = "macos")]
struct OwnedHalUnit<U> {
    unit: Option<U>,
}

#[cfg(target_os = "macos")]
impl<U> OwnedHalUnit<U> {
    fn new(unit: U) -> Self {
        let live = LIVE_HAL_UNITS.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(live_hal_units = live, "HAL input allocated");
        Self { unit: Some(unit) }
    }
}

#[cfg(target_os = "macos")]
impl<U> Drop for OwnedHalUnit<U> {
    fn drop(&mut self) {
        // AudioUnit::drop stops, uninitializes, frees callbacks and disposes the instance.
        drop(self.unit.take());
        let previous = LIVE_HAL_UNITS.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0);
        tracing::debug!(live_hal_units = previous - 1, "HAL input disposed");
    }
}

#[cfg(any(target_os = "macos", test))]
fn warm_input_present(device_id: Option<u32>, inventory: &[u32]) -> bool {
    device_id.is_some_and(|id| inventory.contains(&id))
}

#[cfg(target_os = "macos")]
fn input_alive(id: u32) -> Result<bool> {
    use coreaudio::sys;
    let address = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioDevicePropertyDeviceIsAlive,
        mScope: sys::kAudioObjectPropertyScopeGlobal,
        mElement: sys::kAudioObjectPropertyElementMaster,
    };
    let mut alive = 0_u32;
    let mut size = std::mem::size_of_val(&alive) as u32;
    let status = unsafe {
        sys::AudioObjectGetPropertyData(
            id,
            &address,
            0,
            std::ptr::null(),
            &mut size,
            (&mut alive as *mut u32).cast(),
        )
    };
    if status != 0 {
        bail!("cannot inspect microphone availability: CoreAudio {status}");
    }
    Ok(alive != 0)
}

struct OpenStream {
    // Keep the stream (cpal or the macOS voice unit) on its owner thread.
    // Non-default macOS inputs own their HAL unit directly, including its callback.
    _stream: Box<dyn std::any::Any>,
    buffer: Arc<CaptureBuffer>,
    sample_rate: u32,
    channels: u16,
    generation: u64,
    microphone: String,
    device_id: Option<u32>,
}

#[cfg(target_os = "macos")]
impl OpenStream {
    fn check_input(&self) {
        let result = (|| -> Result<()> {
            let id = self.device_id.context("microphone ID unavailable")?;
            if !input_alive(id)? {
                bail!("microphone disconnected");
            }
            if let Some(owned) = self
                ._stream
                .downcast_ref::<OwnedHalUnit<coreaudio::audio_unit::AudioUnit>>()
            {
                use coreaudio::{
                    audio_unit::{Element, Scope},
                    sys,
                };
                let unit = owned.unit.as_ref().unwrap();
                let status: sys::OSStatus = unit.get_property(
                    sys::kAudioUnitProperty_LastRenderError,
                    Scope::Global,
                    Element::Output,
                )?;
                if status != 0 {
                    bail!("microphone render failed: CoreAudio {status}");
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.buffer.fail(error.to_string());
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for OpenStream {
    fn drop(&mut self) {
        // Quiesce the buffer before stopping and disposing the native unit.
        let _ = self.buffer.take();
    }
}

fn warm_deadline(config: &RecordingConfig, has_stream: bool, now: Instant) -> Option<Instant> {
    // shortcut: VPIO follows default input; close at stop until input pinning is hardware-validated.
    #[cfg(target_os = "macos")]
    if voice_processing(config) {
        return None;
    }
    (has_stream && config.keep_warm_secs > 0)
        .then(|| now + Duration::from_secs(u64::from(config.keep_warm_secs)))
}

impl CpalCapture {
    pub fn new(config: &RecordingConfig) -> Result<Self> {
        if !(1..=MAX_RECORDING_SECS).contains(&config.max_seconds)
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
        let microphone = Arc::new(Mutex::new(None));
        let thread_microphone = microphone.clone();
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
                                let opened = Instant::now();
                                #[cfg(target_os = "macos")]
                                let selected = if resolve_on_start(stream.is_some()) {
                                    match select_device(&config) {
                                        Ok(selected) => Some(selected),
                                        Err(error) if stream.is_some() => {
                                            let id = stream.as_ref().and_then(|open| open.device_id);
                                            let present = coreaudio::audio_unit::macos_helpers::get_audio_device_ids()
                                                .is_ok_and(|ids| warm_input_present(id, &ids))
                                                && id.is_some_and(|id| input_alive(id).unwrap_or(false));
                                            if !present {
                                                stream = None;
                                                return Err(error.context("warm microphone is unavailable"));
                                            }
                                            tracing::debug!(%error, "microphone selection unavailable; reusing present warm stream");
                                            None
                                        }
                                        Err(error) => return Err(error),
                                    }
                                } else { None };
                                #[cfg(not(target_os = "macos"))]
                                let selected = resolve_on_start(stream.is_some()).then(|| select_device(&config)).transpose()?;
                                if stream.as_ref().zip(selected.as_ref()).is_some_and(
                                    |(open, (_, choice))| open.device_id != choice.device_id,
                                ) {
                                    stream = None;
                                }
                                if let Some(open) = &mut stream {
                                    #[cfg(target_os = "macos")]
                                    if !input_alive(open.device_id.context("microphone ID unavailable")?)? { bail!("warm microphone disconnected"); }
                                    if let Some((_, choice)) = &selected {
                                        open.microphone = choice.description();
                                    }
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
                                        selected.context("microphone was not resolved")?,
                                        callback_sender.clone(),
                                        thread_level.clone(),
                                        generation,
                                        thread_finished.clone(),
                                        opened,
                                    )?);
                                }
                                if let Some(open) = &stream {
                                    *thread_microphone.lock().map_err(|_| {
                                        anyhow!("microphone status lock poisoned")
                                    })? = Some(open.microphone.clone());
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
                                #[cfg(target_os = "macos")]
                                open.check_input();
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
                        Command::Drain(reply) => {
                            // Also after a failure or limit, so the audio captured before
                            // it survives; stop() then only reports the error. Idle
                            // streams hold no recording and return nothing.
                            let result = match (&stream, completed.as_mut()) {
                                (Some(open), _) => {
                                    #[cfg(target_os = "macos")]
                                    open.check_input();
                                    open.buffer.drain()
                                },
                                (None, Some(done)) if !done.samples.is_empty() => {
                                    Ok(Some(done.drain(Vec::new())))
                                }
                                _ => Ok(None),
                            };
                            let _ = reply.send(result);
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
            microphone,
        })
    }
}

fn open_stream(
    config: &RecordingConfig,
    selected: (cpal::Device, DeviceChoice),
    sender: mpsc::SyncSender<Command>,
    level: Arc<AtomicU32>,
    generation: u64,
    finished: Arc<AtomicBool>,
    opened: Instant,
) -> Result<OpenStream> {
    let (device, choice) = selected;
    tracing::debug!(device_index = ?choice.index, device_id = ?choice.device_id, system_default = choice.is_default, "opening resolved microphone");
    #[cfg(target_os = "macos")]
    let mut choice = choice;
    #[cfg(target_os = "macos")]
    if voice_processing(config) && choice.is_default {
        match open_voice_unit(config, &sender, &level, generation, &finished, opened) {
            Ok(mut open) => {
                open.microphone = choice.description();
                open.device_id = choice.device_id;
                return Ok(open);
            }
            Err(error) => {
                choice.echo_unavailable();
                static FALLBACK: std::sync::Once = std::sync::Once::new();
                FALLBACK.call_once(|| {
                    tracing::warn!(%error, "voice processing unavailable; using the plain microphone")
                });
            }
        }
    }
    #[cfg(target_os = "macos")]
    if !choice.is_default {
        return open_hal_input(config, &choice, sender, level, generation, finished, opened);
    }
    let supported = device
        .default_input_config()
        .context("cannot inspect microphone")?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let buffer = CaptureBuffer::open(sample_rate, channels, config, finished)?;
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
    #[cfg(target_os = "macos")]
    let stream = PauseOnDrop(stream);
    #[cfg(target_os = "macos")]
    stream.0.play().context("cannot start microphone")?;
    #[cfg(not(target_os = "macos"))]
    stream.play().context("cannot start microphone")?;
    Ok(OpenStream {
        _stream: Box::new(stream),
        buffer,
        sample_rate,
        channels,
        generation,
        microphone: choice.description(),
        device_id: choice.device_id,
    })
}

fn smooth_level(previous: f32, rms: f32, seconds: f32) -> f32 {
    let tau = if rms > previous { 0.025 } else { 0.12 };
    previous + (rms - previous) * (1.0 - (-seconds / tau).exp())
}

/// The per-callback work shared by every capture backend: fill the recording,
/// publish the level and wake the worker at the limit. Never blocks or allocates.
fn input_handler<T: Sample>(
    buffer: Arc<CaptureBuffer>,
    sender: mpsc::SyncSender<Command>,
    level: Arc<AtomicU32>,
    generation: u64,
    opened: Instant,
    samples_per_second: f32,
) -> impl FnMut(&[T])
where
    f32: FromSample<T>,
{
    let mut first = true;
    let mut smoothed = 0.0;
    move |input: &[T]| {
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
                smoothed = smooth_level(smoothed, rms, input.len() as f32 / samples_per_second);
                level.store(smoothed.to_bits(), Ordering::Relaxed);
                if limit {
                    buffer.active.store(false, Ordering::Release);
                    buffer.limited.store(true, Ordering::Release);
                    let _ = sender.try_send(Command::Limit(generation));
                }
            }
        }
    }
}

fn build_stream<T: SizedSample + 'static>(
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
    let error_buffer = buffer.clone();
    let mut handle = input_handler::<T>(
        buffer,
        sender.clone(),
        level,
        generation,
        opened,
        samples_per_second,
    );
    device
        .build_input_stream(
            config,
            move |input: &[T], _| handle(input),
            move |error| {
                error_buffer.fail(error.to_string());
                // Never block a CPAL callback on the worker that drops its stream.
                let _ = sender.try_send(Command::Limit(generation));
            },
            Some(Duration::from_secs(3)),
        )
        .context("cannot open microphone stream")
}

#[cfg(target_os = "macos")]
fn hal_input_format(
    sample_rate: f64,
    channels: u32,
) -> Result<coreaudio::audio_unit::StreamFormat> {
    use coreaudio::audio_unit::{audio_format::LinearPcmFlags, SampleFormat, StreamFormat};
    if !sample_rate.is_finite() || !(1.0..=f64::from(u32::MAX)).contains(&sample_rate) {
        bail!("invalid HAL input sample rate");
    }
    let channels = u16::try_from(channels).context("invalid HAL input channel count")?;
    if channels == 0 {
        bail!("invalid HAL input channel count");
    }
    Ok(StreamFormat {
        sample_rate,
        sample_format: SampleFormat::F32,
        flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
        channels: u32::from(channels),
    })
}

#[cfg(target_os = "macos")]
fn open_hal_input(
    config: &RecordingConfig,
    choice: &DeviceChoice,
    sender: mpsc::SyncSender<Command>,
    level: Arc<AtomicU32>,
    generation: u64,
    finished: Arc<AtomicBool>,
    opened: Instant,
) -> Result<OpenStream> {
    use coreaudio::{
        audio_unit::{
            render_callback::{data, Args},
            AudioUnit, Element, IOType, Scope,
        },
        sys,
    };
    let id = choice.device_id.context("microphone ID unavailable")?;
    let mut owned = OwnedHalUnit::new(AudioUnit::new(IOType::HalOutput)?);
    let unit = owned.unit.as_mut().unwrap();
    unit.uninitialize()?;
    unit.set_property(
        sys::kAudioOutputUnitProperty_EnableIO,
        Scope::Input,
        Element::Input,
        Some(&1_u32),
    )?;
    unit.set_property(
        sys::kAudioOutputUnitProperty_EnableIO,
        Scope::Output,
        Element::Output,
        Some(&0_u32),
    )?;
    unit.set_property(
        sys::kAudioOutputUnitProperty_CurrentDevice,
        Scope::Global,
        Element::Output,
        Some(&id),
    )?;
    let hardware: sys::AudioStreamBasicDescription = unit.get_property(
        sys::kAudioUnitProperty_StreamFormat,
        Scope::Input,
        Element::Input,
    )?;
    let format = hal_input_format(hardware.mSampleRate, hardware.mChannelsPerFrame)?;
    let sample_rate = format.sample_rate as u32;
    let channels = format.channels as u16;
    let format = format.to_asbd();
    unit.set_property(
        sys::kAudioUnitProperty_StreamFormat,
        Scope::Output,
        Element::Input,
        Some(&format),
    )?;
    let buffer = CaptureBuffer::open(sample_rate, channels, config, finished)?;
    let mut handle = input_handler::<f32>(
        buffer.clone(),
        sender,
        level,
        generation,
        opened,
        sample_rate as f32 * f32::from(channels),
    );
    unit.set_input_callback(move |args: Args<data::Interleaved<f32>>| {
        handle(args.data.buffer);
        Ok(())
    })?;
    unit.initialize()?;
    unit.start()?;
    Ok(OpenStream {
        _stream: Box::new(owned),
        buffer,
        sample_rate,
        channels,
        generation,
        microphone: choice.description(),
        device_id: Some(id),
    })
}

/// Echo cancellation follows the system default input and output; a chosen
/// device keeps the plain microphone.
// shortcut: default device only; pin VPIO input when a chosen mic needs echo cancellation.
#[cfg(any(target_os = "macos", test))]
fn voice_processing(config: &RecordingConfig) -> bool {
    config.ignore_speakers && config.device.is_none()
}

/// Apple's voice processing (the FaceTime echo canceller). Measured on a
/// MacBook Air: it removes what other apps play through the default output,
/// but only while its own output element runs, so that plays silence. It
/// exists only while the stream is open, like the plain microphone.
#[cfg(target_os = "macos")]
fn open_voice_unit(
    config: &RecordingConfig,
    sender: &mpsc::SyncSender<Command>,
    level: &Arc<AtomicU32>,
    generation: u64,
    finished: &Arc<AtomicBool>,
    opened: Instant,
) -> Result<OpenStream> {
    use coreaudio::audio_unit::{
        audio_format::LinearPcmFlags,
        render_callback::{data, Args},
        AudioUnit, Element, IOType, SampleFormat, Scope, StreamFormat,
    };
    use coreaudio::sys;
    // AUVoiceIOOtherAudioDuckingConfiguration (AudioUnitProperties.h, macOS 14+).
    #[repr(C)]
    struct Ducking {
        advanced: u8,
        level: u32,
    }
    // AudioUnit::new initializes; I/O and formats only apply before initialization.
    let mut unit = AudioUnit::new(IOType::VoiceProcessingIO)?;
    unit.uninitialize()?;
    for (scope, element) in [
        (Scope::Input, Element::Input),
        (Scope::Output, Element::Output),
    ] {
        unit.set_property(
            sys::kAudioOutputUnitProperty_EnableIO,
            scope,
            element,
            Some(&1_u32),
        )?;
    }
    let hardware: sys::AudioStreamBasicDescription = unit.get_property(
        sys::kAudioUnitProperty_StreamFormat,
        Scope::Input,
        Element::Input,
    )?;
    let sample_rate = hardware.mSampleRate;
    if !(8_000.0..=192_000.0).contains(&sample_rate) {
        bail!("voice processing reported sample rate {sample_rate}");
    }
    let format = StreamFormat {
        sample_rate,
        sample_format: SampleFormat::F32,
        flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
        channels: 1,
    }
    .to_asbd();
    // Capture leaves the input element; silence enters the output element.
    // (set_stream_format only addresses the output element.)
    for (scope, element) in [
        (Scope::Output, Element::Input),
        (Scope::Input, Element::Output),
    ] {
        unit.set_property(
            sys::kAudioUnitProperty_StreamFormat,
            scope,
            element,
            Some(&format),
        )?;
    }
    unit.set_render_callback(|args: Args<data::Interleaved<f32>>| {
        args.data.buffer.fill(0.0);
        Ok(())
    })?;
    let sample_rate = sample_rate as u32;
    let buffer = CaptureBuffer::open(sample_rate, 1, config, finished.clone())?;
    let mut handle = input_handler::<f32>(
        buffer.clone(),
        sender.clone(),
        level.clone(),
        generation,
        opened,
        sample_rate as f32,
    );
    unit.set_input_callback(move |args: Args<data::Interleaved<f32>>| {
        handle(args.data.buffer);
        Ok(())
    })?;
    unit.initialize()?;
    // Duck other apps' audio as little as possible (the default ducks it hard);
    // cancellation held in testing. 2108 = OtherAudioDuckingConfiguration.
    if let Err(error) = unit.set_property(
        2108,
        Scope::Global,
        Element::Output,
        Some(&Ducking {
            advanced: 0,
            level: 10,
        }),
    ) {
        tracing::debug!(%error, "voice processing keeps default ducking");
    }
    unit.start()?;
    Ok(OpenStream {
        _stream: Box::new(unit),
        buffer,
        sample_rate,
        channels: 1,
        generation,
        microphone: String::new(),
        device_id: None,
    })
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
    async fn drain(&self) -> Result<Option<AudioClip>> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::Drain(reply))
            .context("audio thread stopped")?;
        receive.await.context("audio thread stopped")?
    }
    fn microphone(&self) -> Option<String> {
        self.microphone.lock().ok()?.clone()
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
    #[cfg(target_os = "macos")]
    #[test]
    fn owned_hal_units_are_disposed_after_each_stop_or_cancel() {
        struct Unit {
            callback_buffer: Arc<CaptureBuffer>,
            disposed: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Drop for Unit {
            fn drop(&mut self) {
                assert!(!self.callback_buffer.active.load(Ordering::Acquire));
                self.disposed.fetch_add(1, Ordering::Relaxed);
            }
        }
        let baseline = LIVE_HAL_UNITS.load(Ordering::Relaxed);
        let disposed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for cycle in 0..50 {
            let buffer = CaptureBuffer::open(
                8_000,
                1,
                &RecordingConfig::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
            let callback = Arc::downgrade(&buffer);
            let unit = OwnedHalUnit::new(Unit {
                callback_buffer: buffer.clone(),
                disposed: disposed.clone(),
            });
            assert_eq!(LIVE_HAL_UNITS.load(Ordering::Relaxed), baseline + 1);
            let open = OpenStream {
                _stream: Box::new(unit),
                buffer,
                sample_rate: 8_000,
                channels: 1,
                generation: cycle,
                microphone: "Built-in fallback".into(),
                device_id: Some(2),
            };
            if cycle % 2 == 0 {
                open.buffer.take().unwrap().unwrap().finish().unwrap();
            }
            drop(open);
            assert!(
                callback.upgrade().is_none(),
                "unit callback must be disposed"
            );
            assert_eq!(LIVE_HAL_UNITS.load(Ordering::Relaxed), baseline);
        }
        assert_eq!(disposed.load(Ordering::Relaxed), 50);
    }

    #[test]
    fn retired_microphone_errors_cannot_finish_a_new_dictation() {
        let finished = Arc::new(AtomicBool::new(false));
        let old =
            CaptureBuffer::open(8_000, 1, &RecordingConfig::default(), finished.clone()).unwrap();
        old.take().unwrap();
        let current =
            CaptureBuffer::open(8_000, 1, &RecordingConfig::default(), finished.clone()).unwrap();
        old.fail("late error from an earlier device".into());
        assert!(!finished.load(Ordering::Acquire));
        assert!(current.active.load(Ordering::Acquire));
        current.fail("current device failed".into());
        assert!(finished.load(Ordering::Acquire));
    }

    #[test]
    fn poisoned_active_buffer_still_signals_capture_failure() {
        let finished = Arc::new(AtomicBool::new(false));
        let buffer =
            CaptureBuffer::open(8_000, 1, &RecordingConfig::default(), finished.clone()).unwrap();
        let _ = std::panic::catch_unwind(|| {
            let _guard = buffer.recording.lock().unwrap();
            panic!("fixture: poison recording buffer");
        });
        buffer.fail("capture failed".into());
        assert!(finished.load(Ordering::Acquire));
        assert!(!buffer.active.load(Ordering::Acquire));
    }

    #[test]
    fn warm_reuse_requires_the_same_device_id_in_the_inventory() {
        assert!(warm_input_present(Some(2), &[1, 2]));
        assert!(!warm_input_present(Some(2), &[1, 3]));
        assert!(!warm_input_present(Some(2), &[]));
        assert!(!warm_input_present(None, &[1, 2]));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hal_input_format_preserves_hardware_rate_and_channels() {
        let format = hal_input_format(44_100.0, 2).unwrap();
        assert_eq!(format.sample_rate, 44_100.0);
        assert_eq!(format.channels, 2);
        for (rate, channels) in [
            (f64::NAN, 1),
            (f64::INFINITY, 1),
            (0.0, 1),
            (48_000.0, 0),
            (48_000.0, 65_536),
        ] {
            assert!(hal_input_format(rate, channels).is_err());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plain_stream_stop_and_cancel_pause_before_releasing_the_handle() {
        struct FakeStream(Arc<Mutex<Vec<&'static str>>>);
        impl StreamTrait for FakeStream {
            fn play(&self) -> std::result::Result<(), cpal::PlayStreamError> {
                Ok(())
            }
            fn pause(&self) -> std::result::Result<(), cpal::PauseStreamError> {
                self.0.lock().unwrap().push("pause/AudioUnit stop");
                Ok(())
            }
        }
        impl Drop for FakeStream {
            fn drop(&mut self) {
                self.0.lock().unwrap().push("release handle");
            }
        }
        for stop in [true, false] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let buffer = CaptureBuffer::open(
                8_000,
                1,
                &RecordingConfig::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
            // Models the callback retained by CPAL's non-default disconnect listener.
            let retained_buffer = buffer.clone();
            let open = OpenStream {
                _stream: Box::new(PauseOnDrop(FakeStream(events.clone()))),
                buffer,
                sample_rate: 8_000,
                channels: 1,
                generation: 1,
                microphone: "Built-in fallback".into(),
                device_id: Some(2),
            };
            if stop {
                let _ = open.buffer.take().unwrap().unwrap().finish().unwrap();
            }
            drop(open);
            assert_eq!(
                *events.lock().unwrap(),
                ["pause/AudioUnit stop", "release handle"]
            );
            assert!(!retained_buffer.active.load(Ordering::Acquire));
            assert!(retained_buffer.recording.lock().unwrap().is_none());
        }
    }

    #[test]
    fn selected_default_uses_default_handle_and_other_inputs_keep_their_identity() {
        let choice = || DeviceChoice {
            index: None,
            device_id: Some(2),
            name: "Built-in".into(),
            is_default: true,
            notice: None,
        };
        let (handle, chosen) = prefer_default_handle(
            ("enumerated", choice()),
            Some((2, "default API")),
            &RecordingConfig::default(),
        );
        assert_eq!(handle, "default API");
        assert!(chosen.is_default);
        let (handle, chosen) = prefer_default_handle(
            ("enumerated", choice()),
            Some((3, "new default API")),
            &RecordingConfig {
                ignore_speakers: true,
                ..Default::default()
            },
        );
        assert_eq!(handle, "enumerated");
        assert_eq!(chosen.device_id, Some(2));
        assert!(
            !chosen.is_default,
            "VPIO must not follow a different default"
        );
        assert!(chosen.description().contains("Echo cancellation is off"));
    }

    #[test]
    fn inventory_churn_and_read_errors_fall_back_to_default_after_two_attempts() {
        let mut attempts = 0;
        let inputs = stable_inputs(
            || {
                attempts += 1;
                if attempts == 1 {
                    Err(anyhow!("CoreAudio inventory changed"))
                } else {
                    Ok(None)
                }
            },
            || {
                Ok(NativeInput {
                    id: 2,
                    device: "default API",
                    candidate: Candidate {
                        name: "Built-in".into(),
                        transport: Transport::BuiltIn,
                        is_default: true,
                    },
                })
            },
        )
        .unwrap();
        assert_eq!(attempts, 2);
        let (handle, choice) = choose_input(inputs, &RecordingConfig::default()).unwrap();
        assert_eq!(handle, "default API");
        assert_eq!(choice.device_id, Some(2));
    }

    #[test]
    fn microphone_inventory_skips_failed_reads_and_uses_a_surviving_input() {
        let inputs = usable_inputs((1..=5_u32).map(|id| (id, id)), |id| match id {
            1 => Err(anyhow!("stream property failed during unplug")),
            2 => Err(anyhow!("name is too long for the library reader")),
            3 => Err(anyhow!("transport property failed during teardown")),
            4 => Ok(None),
            _ => Ok(Some(Candidate {
                name: "Built-in".into(),
                transport: Transport::BuiltIn,
                is_default: false,
            })),
        })
        .unwrap();
        let (handle, choice) = choose_input(inputs, &RecordingConfig::default()).unwrap();
        assert_eq!(handle, 5);
        assert_eq!(choice.device_id, Some(5));
        assert!(choice
            .description()
            .contains("system default microphone unavailable"));
        let none = usable_inputs([(1, ())], |_| Err(anyhow!("gone")));
        assert!(none
            .err()
            .unwrap()
            .to_string()
            .contains("no usable microphone"));
    }

    #[test]
    fn microphone_inventory_excludes_output_name_collisions_by_id() {
        let inputs = usable_inputs([(10, "output-only"), (20, "input")], |id| {
            Ok((id == 20).then(|| Candidate {
                name: "Same USB name".into(),
                transport: Transport::Other,
                is_default: true,
            }))
        })
        .unwrap();
        let config = RecordingConfig {
            device: Some("Same USB name".into()),
            ..Default::default()
        };
        let (handle, choice) = choose_input(inputs, &config).unwrap();
        assert_eq!(handle, "input");
        assert_eq!(choice.device_id, Some(20));
        assert!(choice.notice.is_none());
    }

    #[test]
    fn warm_microphone_resolution_preserves_linux_exclusive_streams() {
        assert!(resolve_on_start(false));
        assert_eq!(resolve_on_start(true), cfg!(target_os = "macos"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn automatic_echo_cancellation_closes_the_stream_at_stop() {
        let config = RecordingConfig {
            ignore_speakers: true,
            keep_warm_secs: 2,
            ..Default::default()
        };
        assert_eq!(warm_deadline(&config, true, Instant::now()), None);
    }

    #[test]
    fn voice_processing_failure_reports_echo_cancellation_off() {
        let devices = [Candidate {
            name: "Built-in".into(),
            transport: Transport::BuiltIn,
            is_default: true,
        }];
        let mut choice = resolve_device(&devices, &RecordingConfig::default()).unwrap();
        choice.echo_unavailable();
        assert!(choice
            .description()
            .contains("Echo cancellation is off; voice processing unavailable"));
    }

    #[test]
    fn microphone_resolver_table() {
        use Transport::*;
        // (label, devices, named, avoid Bluetooth, expected, notice substring)
        let rows = [
            (
                "named USB",
                vec![
                    ("USB", Other, false),
                    ("AirPods", Bluetooth, true),
                    ("Built-in", BuiltIn, false),
                ],
                Some("USB"),
                true,
                "USB",
                None,
            ),
            (
                "named Bluetooth",
                vec![("AirPods", Bluetooth, true), ("Built-in", BuiltIn, false)],
                Some("AirPods"),
                true,
                "AirPods",
                None,
            ),
            (
                "named virtual",
                vec![("Virtual", Virtual, false), ("Built-in", BuiltIn, true)],
                Some("Virtual"),
                true,
                "Virtual",
                None,
            ),
            (
                "missing USB",
                vec![("AirPods", Bluetooth, true), ("Built-in", BuiltIn, false)],
                Some("USB"),
                true,
                "Built-in",
                Some("recording.device \"USB\" not found — using Built-in"),
            ),
            (
                "missing USB wired default",
                vec![("Built-in", BuiltIn, true)],
                Some("USB"),
                true,
                "Built-in",
                Some("not found"),
            ),
            (
                "Bluetooth",
                vec![
                    ("AirPods", Bluetooth, true),
                    ("USB", Other, false),
                    ("Built-in", BuiltIn, false),
                ],
                None,
                true,
                "Built-in",
                Some("headphones kept in high quality"),
            ),
            (
                "lid closed",
                vec![("AirPods", Bluetooth, true)],
                None,
                true,
                "AirPods",
                None,
            ),
            (
                "Bluetooth LE",
                vec![("AirPods", BluetoothLe, true), ("Built-in", BuiltIn, false)],
                None,
                true,
                "Built-in",
                Some("headphones kept in high quality"),
            ),
            (
                "allow Bluetooth",
                vec![("AirPods", Bluetooth, true), ("Built-in", BuiltIn, false)],
                None,
                false,
                "AirPods",
                None,
            ),
            (
                "virtual default",
                vec![("Virtual", Virtual, true), ("Built-in", BuiltIn, false)],
                None,
                false,
                "Built-in",
                Some("virtual/aggregate"),
            ),
            (
                "aggregate default",
                vec![("Aggregate", Aggregate, true), ("Built-in", BuiltIn, false)],
                None,
                true,
                "Built-in",
                Some("virtual/aggregate"),
            ),
            (
                "virtual only",
                vec![("Virtual", Virtual, true)],
                None,
                true,
                "Virtual",
                None,
            ),
            (
                "wired default",
                vec![("USB", Other, true), ("Built-in", BuiltIn, false)],
                None,
                true,
                "USB",
                None,
            ),
            (
                "unrelated duplicate with pinned Yeti",
                vec![
                    ("AirPods", Bluetooth, true),
                    ("Yeti", Other, false),
                    ("USB Audio Device", Other, false),
                    ("USB Audio Device", Other, false),
                ],
                Some("Yeti"),
                true,
                "Yeti",
                None,
            ),
            (
                "unrelated duplicate with built-in fallback",
                vec![
                    ("AirPods", Bluetooth, true),
                    ("Built-in", BuiltIn, false),
                    ("USB Audio Device", Other, false),
                    ("USB Audio Device", Other, false),
                ],
                None,
                true,
                "Built-in",
                Some("headphones kept in high quality"),
            ),
            (
                "unrelated duplicate with missing pin",
                vec![
                    ("AirPods", Bluetooth, true),
                    ("Built-in", BuiltIn, false),
                    ("USB", Other, false),
                    ("USB", Other, false),
                ],
                Some("Missing"),
                true,
                "Built-in",
                Some("not found"),
            ),
            (
                "duplicate default with unique built-in",
                vec![
                    ("Same", Bluetooth, true),
                    ("Same", Bluetooth, false),
                    ("Built-in", BuiltIn, false),
                ],
                None,
                true,
                "Built-in",
                Some("headphones kept in high quality"),
            ),
            (
                "duplicate built-in pick",
                vec![
                    ("AirPods", Bluetooth, true),
                    ("Same", BuiltIn, false),
                    ("Same", BuiltIn, false),
                ],
                None,
                true,
                "Same",
                Some("headphones kept in high quality"),
            ),
            (
                "duplicate CPAL names",
                vec![("Same", Bluetooth, true), ("Same", BuiltIn, false)],
                Some("Same"),
                true,
                "Same",
                Some("duplicate"),
            ),
            (
                "duplicate CoreAudio names",
                vec![
                    ("AirPods", Bluetooth, true),
                    ("Same", Other, false),
                    ("Same", Other, false),
                    ("Built-in", BuiltIn, false),
                ],
                Some("Same"),
                true,
                "Built-in",
                Some("duplicate"),
            ),
        ];
        for (label, devices, named, avoid_bluetooth, expected, notice) in rows {
            let candidates: Vec<_> = devices
                .into_iter()
                .map(|(name, transport, is_default)| Candidate {
                    name: name.into(),
                    transport,
                    is_default,
                })
                .collect();
            let config = RecordingConfig {
                device: named.map(String::from),
                avoid_bluetooth,
                ..Default::default()
            };
            let choice = resolve_device(&candidates, &config).unwrap();
            assert_eq!(choice.name, expected, "{label}");
            if label == "duplicate built-in pick" || label == "duplicate CPAL names" {
                assert_eq!(
                    choice.index,
                    Some(1),
                    "built-in chosen by ID, regardless of name"
                );
            }
            match notice {
                Some(notice) => assert!(
                    choice.description().contains(notice),
                    "{label}: {}",
                    choice.description()
                ),
                None => assert!(choice.notice.is_none(), "{label}"),
            }
        }
    }

    #[test]
    fn microphone_resolver_empty_errors_and_missing_default_falls_back() {
        assert!(resolve_device(&[], &RecordingConfig::default())
            .err()
            .unwrap()
            .to_string()
            .contains("no usable microphone"));
        let devices = [Candidate {
            name: "USB".into(),
            transport: Transport::Other,
            is_default: false,
        }];
        assert_eq!(
            resolve_device(&devices, &RecordingConfig::default())
                .unwrap()
                .name,
            "USB"
        );
        let config = RecordingConfig {
            device: Some("USB".into()),
            ..Default::default()
        };
        assert_eq!(resolve_device(&devices, &config).unwrap().name, "USB");
    }

    #[test]
    fn bluetooth_fallback_disables_voice_processing_before_any_mic_opens() {
        let devices = [
            Candidate {
                name: "AirPods".into(),
                transport: Transport::Bluetooth,
                is_default: true,
            },
            Candidate {
                name: "Built-in".into(),
                transport: Transport::BuiltIn,
                is_default: false,
            },
        ];
        let config = RecordingConfig {
            ignore_speakers: true,
            ..Default::default()
        };
        let choice = resolve_device(&devices, &config).unwrap();
        assert!(!(voice_processing(&config) && choice.is_default));
        assert!(choice.description().contains("Echo cancellation is off"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "read-only hardware enumeration; never opens a microphone"]
    fn enumerate_microphones_without_opening_streams() {
        let devices = input_devices().unwrap();
        let inputs = mac_inputs().unwrap();
        for device in devices {
            eprintln!(
                "input {:?}, default={}, transport={:?}",
                device.name,
                device.default,
                inputs
                    .iter()
                    .filter(|input| input.candidate.name == device.name)
                    .map(|input| (input.id, input.candidate.transport))
                    .collect::<Vec<_>>()
            );
        }
        let mut timings = Vec::new();
        for _ in 0..30 {
            let start = Instant::now();
            let choice = selected_microphone(&RecordingConfig::default()).unwrap();
            timings.push(start.elapsed().as_micros());
            if timings.len() == 1 {
                eprintln!("next recording: {choice}");
            }
        }
        timings.sort();
        eprintln!("read-only device resolution, 30 runs: p50={} us, p95={} us (not key-down→first sample)", timings[14], timings[28]);
    }

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
    fn speaker_cancelling_is_opt_in_and_yields_to_a_chosen_microphone() {
        assert!(!voice_processing(&RecordingConfig::default()));
        let on = RecordingConfig {
            ignore_speakers: true,
            ..Default::default()
        };
        assert!(voice_processing(&on));
        let usb = RecordingConfig {
            device: Some("USB Microphone".into()),
            ..on
        };
        assert!(!voice_processing(&usb));
    }
    #[test]
    fn shared_input_callback_captures_audio_and_reports_the_limit() {
        let config = RecordingConfig {
            max_seconds: 1,
            ..Default::default()
        };
        let finished = Arc::new(AtomicBool::new(false));
        let buffer = CaptureBuffer::open(4, 1, &config, finished.clone()).unwrap();
        let level = Arc::new(AtomicU32::new(0));
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut handle = input_handler::<f32>(
            buffer.clone(),
            sender,
            level.clone(),
            7,
            Instant::now(),
            4.0,
        );
        handle(&[0.5; 5]);
        assert!(buffer.first_callback_us.load(Ordering::Relaxed) > 0);
        assert!(f32::from_bits(level.load(Ordering::Relaxed)) > 0.0);
        assert!(finished.load(Ordering::Acquire));
        assert!(matches!(receiver.try_recv(), Ok(Command::Limit(7))));
        let clip = buffer.take().unwrap().unwrap().finish().unwrap();
        assert_eq!(clip.samples, vec![0.5; 4]);
        handle(&[1.0]);
        assert!(buffer.take().unwrap().is_none());
        assert!(receiver.try_recv().is_err());
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
    fn capture_preserves_silence_and_internal_pauses_for_the_daemon_gate() {
        let config = RecordingConfig {
            max_seconds: 1,
            silence_threshold: 0.1,
            ..Default::default()
        };
        let mut silent = Recording::new(4, 1, &config).unwrap();
        silent.push(&[0.01_f32; 4]);
        assert_eq!(silent.finish().unwrap().samples, vec![0.01; 4]);
        let mut voiced = Recording::new(4, 1, &config).unwrap();
        voiced.push(&[0.5_f32, 0.0, 0.0, 0.5]);
        assert_eq!(voiced.finish().unwrap().samples.len(), 4);
    }
    #[test]
    fn drains_hand_over_audio_without_gaps_and_keep_recording() {
        let config = RecordingConfig {
            max_seconds: 10,
            ..Default::default()
        };
        let mut recording = Recording::new(4, 2, &config).unwrap();
        assert_eq!(recording.reserve, 4 * 2 * 10);
        recording.push(&[0.1_f32, 0.2, 0.3, 0.4]);
        let first = recording.drain(Vec::with_capacity(recording.reserve));
        assert_eq!((first.sample_rate, first.channels), (4, 2));
        assert_eq!(first.samples, vec![0.1, 0.2, 0.3, 0.4]);
        recording.push(&[0.5_f32, 0.6]);
        assert_eq!(recording.finish().unwrap().samples, vec![0.5, 0.6]);
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
    fn high_rate_capture_stays_within_memory_and_whole_frame_limits() {
        assert_eq!(
            Recording::sample_limit(192_000, 1, 120).unwrap(),
            MAX_SAMPLES
        );
        assert_eq!(
            Recording::sample_limit(48_000, 1, 300).unwrap(),
            48_000 * 300
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
            microphone: Arc::new(Mutex::new(None)),
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
