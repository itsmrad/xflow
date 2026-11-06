//! One microphone, two consumers. Dictation keeps its recording buffer.
//! A meeting lease gets its own preallocated queue. The callback never waits
//! on the meeting consumer and never grows that queue.
use cpal::{FromSample, Sample};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering},
    Mutex, TryLockError,
};
use std::time::Instant;
use xflow_core::config::RecordingConfig;
use xflow_core::{CaptureEnvelope, CaptureGap, MeetingPull};

/// Frames of interleaved PCM one queue slot holds, for every channel up to [`MAX_CHANNELS`].
pub(crate) const SLOT_FRAMES: usize = 4096;
/// Preallocated slots. A full queue records a gap instead of growing or blocking.
pub(crate) const SLOT_COUNT: usize = 8;
pub(crate) const MAX_CHANNELS: u16 = 32;
pub(crate) const MAX_RATE: u32 = 192_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Overflow,
    Hardware,
    Layout,
    Superseded,
}

struct Slot {
    sequence: u64,
    frame_offset: u64,
    frame_count: u32,
    host_us: u64,
    pcm_len: usize,
    pcm: Box<[f32]>,
}

struct State {
    generation: u64,
    active: bool,
    closed: bool,
    sample_rate: u32,
    channels: u16,
    epoch: u64,
    source_id: u64,
    slots: Vec<Slot>,
    read: usize,
    write: usize,
    next_sequence: u64,
    frame_cursor: u64,
    /// 0 means no gap. Sequences start at 1.
    gap_from: u64,
    gap_to: u64,
    gap_frames: u64,
    degraded: bool,
    fault: Fault,
    detail: Option<String>,
}

impl State {
    fn empty() -> Self {
        Self {
            generation: 0,
            active: false,
            closed: false,
            sample_rate: 0,
            channels: 0,
            epoch: 0,
            source_id: 0,
            slots: Vec::new(),
            read: 0,
            write: 0,
            next_sequence: 1,
            frame_cursor: 0,
            gap_from: 0,
            gap_to: 0,
            gap_frames: 0,
            degraded: false,
            fault: Fault::None,
            detail: None,
        }
    }

    fn ensure_slots(&mut self) {
        if !self.slots.is_empty() {
            return;
        }
        self.slots = (0..SLOT_COUNT)
            .map(|_| Slot {
                sequence: 0,
                frame_offset: 0,
                frame_count: 0,
                host_us: 0,
                pcm_len: 0,
                pcm: vec![0.0; SLOT_FRAMES * MAX_CHANNELS as usize].into_boxed_slice(),
            })
            .collect();
    }

    fn lose(&mut self, frames: u64) {
        if frames == 0 {
            return;
        }
        let seq = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.frame_cursor = self.frame_cursor.wrapping_add(frames);
        if self.gap_from == 0 {
            self.gap_from = seq;
        }
        self.gap_to = seq;
        self.gap_frames = self.gap_frames.saturating_add(frames);
        self.degraded = true;
        if self.fault == Fault::None {
            self.fault = Fault::Overflow;
        }
    }

    fn gap(&self) -> Option<CaptureGap> {
        (self.gap_from != 0).then_some(CaptureGap {
            from_sequence: self.gap_from,
            to_sequence: self.gap_to,
            frames: self.gap_frames,
        })
    }

    fn fault_message(&self) -> Option<String> {
        match self.fault {
            Fault::None => None,
            Fault::Overflow => self.gap().map(|gap| {
                format!(
                    "meeting queue overflow; lost sequences {}..={} ({} frames)",
                    gap.from_sequence, gap.to_sequence, gap.frames
                )
            }),
            Fault::Hardware => Some(
                self.detail
                    .clone()
                    .unwrap_or_else(|| "microphone stream failed".to_owned()),
            ),
            Fault::Layout => Some("meeting capture stopped: incompatible live format".to_owned()),
            Fault::Superseded => self.detail.clone(),
        }
    }

    fn take_envelopes(&mut self) -> MeetingPull {
        let fault = self.fault_message();
        let gap = self.gap();
        let mut envelopes = Vec::new();
        while self.read < self.write {
            let slot = &self.slots[self.read % SLOT_COUNT];
            let mut samples = vec![0.0; slot.pcm_len];
            samples.copy_from_slice(&slot.pcm[..slot.pcm_len]);
            envelopes.push(CaptureEnvelope {
                generation: self.generation,
                source_id: self.source_id,
                sample_rate: self.sample_rate,
                channels: self.channels,
                epoch: self.epoch,
                host_us: slot.host_us,
                frame_offset: slot.frame_offset,
                frame_count: slot.frame_count,
                sequence: slot.sequence,
                gap: None,
                samples,
            });
            self.read += 1;
        }
        if let Some(gap) = gap {
            if let Some(first) = envelopes.first_mut() {
                first.gap = Some(gap);
            } else {
                envelopes.push(CaptureEnvelope {
                    generation: self.generation,
                    source_id: self.source_id,
                    sample_rate: self.sample_rate,
                    channels: self.channels,
                    epoch: self.epoch,
                    host_us: 0,
                    frame_offset: self.frame_cursor,
                    frame_count: 0,
                    sequence: gap.to_sequence,
                    gap: Some(gap),
                    samples: Vec::new(),
                });
            }
        }
        MeetingPull {
            stale: false,
            envelopes,
            fault,
        }
    }
}

pub(crate) struct Format {
    pub sample_rate: u32,
    pub channels: u16,
    pub epoch: u64,
    pub source_id: u64,
}

/// Meeting subscription. The dictation buffer is a different object.
pub(crate) struct MeetingHub {
    hot: AtomicBool,
    level: AtomicU32,
    channels: AtomicU16,
    generation_flag: AtomicU64,
    pending_chunks: AtomicU64,
    pending_frames: AtomicU64,
    inner: Mutex<State>,
}

impl MeetingHub {
    pub(crate) fn inactive() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            hot: AtomicBool::new(false),
            level: AtomicU32::new(0),
            channels: AtomicU16::new(0),
            generation_flag: AtomicU64::new(0),
            pending_chunks: AtomicU64::new(0),
            pending_frames: AtomicU64::new(0),
            inner: Mutex::new(State::empty()),
        })
    }

    pub(crate) fn is_active(&self) -> bool {
        self.hot.load(Ordering::Acquire)
    }

    pub(crate) fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub(crate) fn store_level(&self, level: f32) {
        self.level.store(level.to_bits(), Ordering::Relaxed);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn activate(&self, format: Format) -> Result<u64, String> {
        if format.sample_rate == 0
            || format.channels == 0
            || format.sample_rate > MAX_RATE
            || format.channels > MAX_CHANNELS
        {
            return Err(format!(
                "meeting capture refused: live format {} Hz {} ch is incompatible with the meeting queue",
                format.sample_rate, format.channels
            ));
        }
        let mut state = self.lock();
        if state.active {
            return Err("a meeting capture is already active".to_owned());
        }
        state.ensure_slots();
        state.active = true;
        state.closed = false;
        state.sample_rate = format.sample_rate;
        state.channels = format.channels;
        state.epoch = format.epoch;
        state.source_id = format.source_id;
        state.read = 0;
        state.write = 0;
        state.next_sequence = 1;
        state.frame_cursor = 0;
        state.gap_from = 0;
        state.gap_to = 0;
        state.gap_frames = 0;
        state.degraded = false;
        state.fault = Fault::None;
        state.detail = None;
        state.generation = state.generation.wrapping_add(1);
        if state.generation == 0 {
            state.generation = 1;
        }
        let generation = state.generation;
        self.pending_chunks.store(0, Ordering::Relaxed);
        self.pending_frames.store(0, Ordering::Relaxed);
        self.channels.store(format.channels, Ordering::Release);
        self.generation_flag.store(generation, Ordering::Release);
        self.level.store(0, Ordering::Relaxed);
        self.hot.store(true, Ordering::Release);
        Ok(generation)
    }

    pub(crate) fn push<T: Sample>(&self, input: &[T], opened: Instant) -> Option<(f32, u64)>
    where
        f32: FromSample<T>,
    {
        if !self.hot.load(Ordering::Acquire) || input.is_empty() {
            return None;
        }
        let channels = self.channels.load(Ordering::Acquire) as usize;
        if channels == 0 {
            return None;
        }
        let host_us = opened.elapsed().as_micros() as u64;
        let rms = rms_of(input);
        let frames = (input.len() / channels) as u64;
        let mut state = match self.inner.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                self.pending_chunks.fetch_add(1, Ordering::Relaxed);
                self.pending_frames.fetch_add(frames, Ordering::Relaxed);
                return Some((rms, self.generation_flag.load(Ordering::Acquire)));
            }
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        if !state.active {
            return None;
        }
        fold_pending(&mut state, &self.pending_chunks, &self.pending_frames);
        if !input.len().is_multiple_of(channels) {
            state.fault = Fault::Layout;
            state.lose(frames.max(1));
            return Some((rms, state.generation));
        }
        write_samples(&mut state, input, channels, host_us);
        Some((rms, state.generation))
    }

    pub(crate) fn pull(&self, generation: u64) -> MeetingPull {
        let mut state = self.lock();
        if state.generation != generation {
            return MeetingPull::stale();
        }
        fold_pending(&mut state, &self.pending_chunks, &self.pending_frames);
        state.take_envelopes()
    }

    /// Ends this generation. A different generation is left untouched, including its audio.
    pub(crate) fn release(&self, generation: u64) -> MeetingPull {
        let mut state = self.lock();
        if state.generation != generation || state.closed {
            return MeetingPull::stale();
        }
        state.active = false;
        state.closed = true;
        self.hot.store(false, Ordering::Release);
        self.level.store(0, Ordering::Relaxed);
        fold_pending(&mut state, &self.pending_chunks, &self.pending_frames);
        state.take_envelopes()
    }

    pub(crate) fn supersede(&self, reason: &str) -> bool {
        let mut state = self.lock();
        if !state.active {
            return false;
        }
        state.active = false;
        state.closed = true;
        state.detail = Some(reason.to_owned());
        state.fault = Fault::Superseded;
        self.hot.store(false, Ordering::Release);
        self.level.store(0, Ordering::Relaxed);
        true
    }

    pub(crate) fn fail_hardware(&self, message: &str) {
        let mut state = self.lock();
        if !state.active {
            return;
        }
        state.detail = Some(message.to_owned());
        state.fault = Fault::Hardware;
    }
}

fn fold_pending(state: &mut State, chunks: &AtomicU64, frames: &AtomicU64) {
    let chunks = chunks.swap(0, Ordering::AcqRel);
    let frames = frames.swap(0, Ordering::AcqRel);
    if chunks == 0 {
        return;
    }
    let each = frames / chunks;
    let mut extra = frames % chunks;
    for _ in 0..chunks {
        let mut n = each;
        if extra > 0 {
            n += 1;
            extra -= 1;
        }
        state.lose(n);
    }
}

fn write_samples<T: Sample>(state: &mut State, input: &[T], channels: usize, host_us: u64)
where
    f32: FromSample<T>,
{
    let frames = input.len() / channels;
    let mut consumed = 0usize;
    let mut left = frames;
    while left > 0 {
        if state.degraded || state.write - state.read >= SLOT_COUNT {
            state.lose(left as u64);
            break;
        }
        let n = left.min(SLOT_FRAMES);
        let seq = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);
        let frame_offset = state.frame_cursor;
        state.frame_cursor = state.frame_cursor.wrapping_add(n as u64);
        let slot = &mut state.slots[state.write % SLOT_COUNT];
        let count = n * channels;
        for i in 0..count {
            slot.pcm[i] = input[consumed + i].to_sample();
        }
        slot.sequence = seq;
        slot.frame_offset = frame_offset;
        slot.frame_count = n as u32;
        slot.host_us = host_us;
        slot.pcm_len = count;
        state.write += 1;
        consumed += count;
        left -= n;
    }
}

fn rms_of<T: Sample>(input: &[T]) -> f32
where
    f32: FromSample<T>,
{
    let mut energy = 0.0f64;
    for sample in input {
        let value: f32 = sample.to_sample();
        let value = if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        energy += f64::from(value).powi(2);
    }
    (energy / input.len() as f64).sqrt() as f32
}

pub(crate) struct LiveCapture {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

fn same_device(configured: &str, live_description: &str) -> bool {
    live_description == configured || live_description.starts_with(&format!("{configured} ("))
}

/// Why this dictation setup cannot share the microphone. `None` means a lease may attach.
pub(crate) fn meeting_refusal(
    config: &RecordingConfig,
    live: Option<&LiveCapture>,
    requested_device: Option<&str>,
) -> Option<String> {
    if config.ignore_speakers {
        return Some(
            "meeting capture refused: dictation uses speaker cancellation (ignore_speakers); dictation settings were left unchanged"
                .to_owned(),
        );
    }
    if let Some(want) = requested_device {
        if let Some(live) = live {
            if !same_device(want, &live.device_name) {
                return Some(format!(
                    "meeting capture refused: dictation is using {:?}, not {want:?}",
                    live.device_name
                ));
            }
        } else if config.device.as_deref() != Some(want) {
            return Some(format!(
                "meeting capture refused: dictation microphone is {:?}, not {want:?}",
                config.device
            ));
        }
    }
    let live = live?;
    if let Some(configured) = config.device.as_deref() {
        if !same_device(configured, &live.device_name) {
            return Some(format!(
                "meeting capture refused: dictation is configured for {configured:?} but the live microphone is {:?}",
                live.device_name
            ));
        }
    }
    if live.sample_rate == 0
        || live.channels == 0
        || live.sample_rate > MAX_RATE
        || live.channels > MAX_CHANNELS
    {
        return Some(format!(
            "meeting capture refused: live format {} Hz {} ch is incompatible with the meeting queue",
            live.sample_rate, live.channels
        ));
    }
    None
}

/// `true` when dictation stop should close the hardware. A warm deadline or a meeting lease keeps it.
pub(crate) fn drop_stream_on_dictation_stop(
    meeting_active: bool,
    warm_until: Option<Instant>,
) -> bool {
    warm_until.is_none() && !meeting_active
}

/// Cancel closes the hardware only when dictation is the last consumer. It never arms a warm lease.
pub(crate) fn drop_stream_on_dictation_cancel(meeting_active: bool) -> bool {
    !meeting_active
}

/// After the meeting lease ends: keep the stream for an active dictation or an unexpired warm lease.
pub(crate) fn stream_after_meeting_end(
    dictation_active: bool,
    warm_until: Option<Instant>,
    now: Instant,
) -> bool {
    dictation_active || warm_until.is_some_and(|until| now < until)
}

#[cfg(test)]
mod tests {
    use super::super::Recording;
    use super::*;
    use std::time::Duration;

    fn format() -> Format {
        Format {
            sample_rate: 16_000,
            channels: 1,
            epoch: 3,
            source_id: 7,
        }
    }

    fn config(device: Option<&str>, ignore_speakers: bool) -> RecordingConfig {
        RecordingConfig {
            device: device.map(str::to_owned),
            ignore_speakers,
            ..RecordingConfig::default()
        }
    }

    #[test]
    fn refusal_covers_speaker_cancellation_device_and_format() {
        let plain = config(None, false);
        assert!(meeting_refusal(&plain, None, None).is_none());
        let speakers = meeting_refusal(&config(None, true), None, None).unwrap();
        assert!(speakers.contains("ignore_speakers") && speakers.contains("unchanged"));
        let other = meeting_refusal(&config(Some("Built-in"), false), None, Some("USB")).unwrap();
        assert!(other.contains("USB") && other.contains("Built-in"));
        let live = LiveCapture {
            device_name: "Built-in".into(),
            sample_rate: 48_000,
            channels: 1,
        };
        assert!(meeting_refusal(&config(Some("Built-in"), false), Some(&live), None).is_none());
        let mismatch = meeting_refusal(&config(Some("USB"), false), Some(&live), None).unwrap();
        assert!(mismatch.contains("USB") && mismatch.contains("Built-in"));
        let asked = meeting_refusal(&plain, Some(&live), Some("USB")).unwrap();
        assert!(asked.contains("USB"));
        let wide = LiveCapture {
            device_name: "Built-in".into(),
            sample_rate: 48_000,
            channels: MAX_CHANNELS + 1,
        };
        assert!(meeting_refusal(&plain, Some(&wide), None)
            .unwrap()
            .contains("incompatible"));
        let noted = LiveCapture {
            device_name: "Built-in (default)".into(),
            sample_rate: 48_000,
            channels: 2,
        };
        assert!(meeting_refusal(&config(Some("Built-in"), false), Some(&noted), None).is_none());
    }

    #[test]
    fn hardware_closes_on_the_last_lease_and_warm_mic_still_holds_it() {
        let now = Instant::now();
        let warm = Some(now + Duration::from_secs(2));
        assert!(drop_stream_on_dictation_stop(false, None));
        assert!(!drop_stream_on_dictation_stop(false, warm));
        assert!(!drop_stream_on_dictation_stop(true, None));
        assert!(!drop_stream_on_dictation_stop(true, warm));
        assert!(drop_stream_on_dictation_cancel(false));
        assert!(!drop_stream_on_dictation_cancel(true));
        assert!(stream_after_meeting_end(true, None, now));
        assert!(stream_after_meeting_end(false, warm, now));
        assert!(!stream_after_meeting_end(false, None, now));
        assert!(!stream_after_meeting_end(
            false,
            Some(now - Duration::from_secs(1)),
            now
        ));
    }

    #[test]
    fn one_consumer_ending_leaves_the_other_audio_intact() {
        let hub = MeetingHub::inactive();
        let generation = hub.activate(format()).unwrap();
        let mut dictation = Recording::new(16_000, 1, &RecordingConfig::default()).unwrap();
        let samples = [0.25_f32, 0.5, -0.5, 0.1];
        dictation.push(&samples);
        hub.push(&samples, Instant::now());
        let pulled = hub.pull(generation);
        assert!(!pulled.stale);
        assert_eq!(pulled.envelopes[0].samples, samples);
        assert_eq!(dictation.samples, samples);
        let handed = dictation.drain(Vec::new());
        assert_eq!(handed.samples, samples);
        hub.push(&[0.75_f32], Instant::now());
        assert!(dictation.samples.is_empty());
        let rest = hub.release(generation);
        assert_eq!(rest.envelopes[0].samples, vec![0.75]);
        dictation.push(&[0.2_f32]);
        assert_eq!(dictation.finish().unwrap().samples, vec![0.2]);
        assert!(hub.release(generation).stale);
    }

    #[test]
    fn stale_generation_does_not_consume_the_new_lease() {
        let hub = MeetingHub::inactive();
        let first = hub.activate(format()).unwrap();
        hub.push(&[1.0_f32], Instant::now());
        assert!(!hub.release(first).stale);
        let second = hub.activate(format()).unwrap();
        assert_ne!(first, second);
        hub.push(&[2.0_f32, 3.0], Instant::now());
        assert!(hub.pull(first).stale);
        assert!(hub.release(first).stale);
        assert!(hub.is_active());
        let live = hub.pull(second);
        assert!(!live.stale);
        assert_eq!(live.envelopes[0].samples, vec![2.0, 3.0]);
        assert_eq!(live.envelopes[0].generation, second);
        assert_eq!(live.envelopes[0].source_id, 7);
        assert_eq!(live.envelopes[0].epoch, 3);
        assert_eq!(
            (live.envelopes[0].sample_rate, live.envelopes[0].channels),
            (16_000, 1)
        );
    }

    #[test]
    fn full_queue_records_exact_loss_and_does_not_block_or_drop_silently() {
        let hub = MeetingHub::inactive();
        let generation = hub
            .activate(Format {
                sample_rate: 48_000,
                channels: 1,
                epoch: 1,
                source_id: 1,
            })
            .unwrap();
        let mut dictation = Recording::new(48_000, 1, &RecordingConfig::default()).unwrap();
        let chunk = vec![0.2_f32; 256];
        let started = Instant::now();
        for _ in 0..20 {
            hub.push(chunk.as_slice(), Instant::now());
        }
        let empty_ns = started.elapsed().as_nanos() / 20;
        // Drop the timing traffic so the loss numbers below are exact.
        let _ = hub.release(generation);
        let generation = hub
            .activate(Format {
                sample_rate: 48_000,
                channels: 1,
                epoch: 1,
                source_id: 1,
            })
            .unwrap();
        for _ in 0..SLOT_COUNT {
            let fill = vec![0.3_f32; SLOT_FRAMES];
            hub.push(fill.as_slice(), Instant::now());
            dictation.push(fill.as_slice());
        }
        let overflow = vec![0.4_f32; 100];
        let overflow_started = Instant::now();
        for _ in 0..200 {
            hub.push(overflow.as_slice(), Instant::now());
            dictation.push(overflow.as_slice());
        }
        let full_ns = overflow_started.elapsed().as_nanos() / 200;
        eprintln!(
            "meeting queue slots={SLOT_COUNT} frames_per_slot={SLOT_FRAMES} max_channels={MAX_CHANNELS} bytes={} empty_callback_ns={empty_ns} full_callback_ns={full_ns}",
            SLOT_COUNT * SLOT_FRAMES * MAX_CHANNELS as usize * 4
        );
        assert!(empty_ns < 50_000_000, "empty callback {empty_ns} ns");
        assert!(full_ns < 50_000_000, "full callback {full_ns} ns");
        let pulled = hub.pull(generation);
        let delivered: u64 = pulled
            .envelopes
            .iter()
            .map(|envelope| u64::from(envelope.frame_count))
            .sum();
        let gap = pulled
            .envelopes
            .iter()
            .find_map(|envelope| envelope.gap)
            .unwrap();
        let subscribed = SLOT_COUNT * SLOT_FRAMES + 200 * overflow.len();
        assert_eq!(delivered, (SLOT_COUNT * SLOT_FRAMES) as u64);
        assert_eq!(delivered + gap.frames, subscribed as u64);
        assert_eq!(gap.frames, (200 * overflow.len()) as u64);
        assert_eq!(gap.from_sequence, SLOT_COUNT as u64 + 1);
        assert_eq!(gap.to_sequence - gap.from_sequence + 1, 200);
        assert!(pulled.fault.as_deref().unwrap().contains("overflow"));
        assert_eq!(dictation.samples.len(), subscribed);
        let again = vec![0.5_f32; 5];
        hub.push(again.as_slice(), Instant::now());
        dictation.push(again.as_slice());
        let later = hub.pull(generation);
        let later_gap = later
            .envelopes
            .iter()
            .find_map(|envelope| envelope.gap)
            .unwrap();
        assert_eq!(later_gap.frames, gap.frames + again.len() as u64);
        assert_eq!(dictation.samples.len(), subscribed + again.len());
    }

    #[test]
    fn a_held_meeting_lock_does_not_stall_the_callback() {
        let hub = MeetingHub::inactive();
        hub.activate(format()).unwrap();
        let _guard = hub.inner.lock().unwrap();
        let started = Instant::now();
        for _ in 0..100 {
            hub.push(&[0.1_f32; 128], Instant::now());
        }
        let ns = started.elapsed().as_nanos() / 100;
        eprintln!("meeting callback while sink holds the queue: {ns} ns");
        assert!(ns < 50_000_000, "blocked callback {ns} ns");
        drop(_guard);
        let generation = hub.generation_flag.load(Ordering::Acquire);
        let pulled = hub.pull(generation);
        let gap = pulled.envelopes.iter().find_map(|e| e.gap).unwrap();
        assert_eq!(gap.frames, 100 * 128);
    }

    #[test]
    fn split_callback_keeps_every_frame_and_the_envelope_fields() {
        let hub = MeetingHub::inactive();
        let generation = hub
            .activate(Format {
                sample_rate: 8_000,
                channels: 2,
                epoch: 4,
                source_id: 9,
            })
            .unwrap();
        let frames = SLOT_FRAMES + 3;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            samples.push(i as f32);
            samples.push(-(i as f32));
        }
        hub.push(samples.as_slice(), Instant::now());
        let pulled = hub.pull(generation);
        assert!(pulled.fault.is_none());
        assert!(pulled.envelopes.iter().all(|e| e.gap.is_none()));
        assert_eq!(pulled.envelopes.len(), 2);
        assert_eq!(pulled.envelopes[0].frame_count, SLOT_FRAMES as u32);
        assert_eq!(pulled.envelopes[1].frame_count, 3);
        assert_eq!(
            pulled.envelopes[0].sequence + 1,
            pulled.envelopes[1].sequence
        );
        assert_eq!(pulled.envelopes[0].frame_offset, 0);
        assert_eq!(pulled.envelopes[1].frame_offset, SLOT_FRAMES as u64);
        assert_eq!(pulled.envelopes[0].channels, 2);
        let mut joined = pulled.envelopes[0].samples.clone();
        joined.extend_from_slice(&pulled.envelopes[1].samples);
        assert_eq!(joined, samples);
    }

    #[test]
    fn superseded_lease_keeps_its_audio_and_the_reason() {
        let hub = MeetingHub::inactive();
        let generation = hub.activate(format()).unwrap();
        hub.push(&[0.4_f32, 0.6], Instant::now());
        assert!(hub.supersede(
            "meeting capture stopped: dictation switched microphone; dictation settings were kept"
        ));
        assert!(!hub.is_active());
        let pulled = hub.pull(generation);
        assert_eq!(pulled.envelopes[0].samples, vec![0.4, 0.6]);
        assert!(pulled.fault.unwrap().contains("switched microphone"));
        hub.push(&[0.9_f32], Instant::now());
        let later = hub.pull(generation);
        assert!(later
            .envelopes
            .iter()
            .all(|envelope| envelope.samples != vec![0.9]));
        assert!(later.fault.unwrap().contains("switched microphone"));
    }

    #[test]
    fn hardware_failure_is_visible_without_discarding_meeting_frames() {
        let hub = MeetingHub::inactive();
        let generation = hub.activate(format()).unwrap();
        hub.push(&[0.2_f32, 0.3], Instant::now());
        hub.fail_hardware("microphone stream failed: unplugged");
        let pulled = hub.pull(generation);
        assert_eq!(pulled.envelopes[0].samples, vec![0.2, 0.3]);
        assert!(pulled.fault.unwrap().contains("unplugged"));
    }
}
