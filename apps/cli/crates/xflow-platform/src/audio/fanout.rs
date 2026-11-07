//! One microphone, two consumers. Dictation keeps its recording buffer.
//! A meeting lease gets its own ring, sized to the live format and freed with
//! the lease. The callback never waits on the meeting consumer and never grows
//! the ring: audio that finds it full becomes a counted gap, and the lease goes on.
use cpal::{FromSample, Sample};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex, TryLockError,
};
use std::time::Instant;
use xflow_core::config::RecordingConfig;
use xflow_core::{CaptureEnvelope, CaptureGap, MeetingPull};

/// Most frames in one delivered envelope, so a late pull still fits one bridge frame.
pub(crate) const ENVELOPE_FRAMES: usize = 4096;
/// Audio the ring keeps while its consumer is late. Later audio becomes a gap.
pub(crate) const RING_MS: u64 = 2_000;
/// Ceiling for many-channel, high-rate devices: the size of the old fixed queue.
const RING_MAX_BYTES: usize = 4 << 20;
/// Gaps waiting for the consumer. A loss that finds these full joins a later gap.
const MARKS: u64 = 64;
pub(crate) const MAX_CHANNELS: u16 = 32;
pub(crate) const MAX_RATE: u32 = 192_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Hardware,
    Layout,
    Superseded,
}

/// Lost frames the consumer has not been told about. `from == 0` means none.
#[derive(Clone, Copy, Default)]
struct Loss {
    from: u64,
    to: u64,
    frames: u64,
}

impl Loss {
    fn add(&mut self, sequence: u64, frames: u64) {
        if self.from == 0 {
            self.from = sequence;
        }
        self.to = sequence;
        self.frames = self.frames.saturating_add(frames);
    }
    fn gap(self) -> Option<CaptureGap> {
        (self.from != 0).then_some(CaptureGap {
            from_sequence: self.from,
            to_sequence: self.to,
            frames: self.frames,
        })
    }
}

#[derive(Default)]
struct Mark {
    /// Ring frame the loss comes before.
    at: AtomicU64,
    from: AtomicU64,
    to: AtomicU64,
    frames: AtomicU64,
}

/// One producer (the callback, under the state lock) and one consumer (pull
/// or release, under the reader lock). Each side stores only its own cursors,
/// so a pull never makes the callback wait or miss.
struct Ring {
    pcm: Box<[AtomicU32]>,
    channels: usize,
    capacity: u64,
    /// Frames written. Only the producer stores it.
    head: AtomicU64,
    /// Frames consumed. Only the consumer stores it.
    tail: AtomicU64,
    marks: Box<[Mark]>,
    mark_head: AtomicU64,
    mark_tail: AtomicU64,
    host_us: AtomicU64,
}

impl Ring {
    fn new(sample_rate: u32, channels: u16) -> Self {
        let channels = usize::from(channels);
        let wanted = u64::from(sample_rate) * RING_MS / 1000;
        let ceiling = (RING_MAX_BYTES / (channels * 4)) as u64;
        let capacity = wanted.min(ceiling).max(ENVELOPE_FRAMES as u64);
        Self {
            pcm: (0..capacity as usize * channels)
                .map(|_| AtomicU32::new(0))
                .collect(),
            channels,
            capacity,
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            marks: (0..MARKS).map(|_| Mark::default()).collect(),
            mark_head: AtomicU64::new(0),
            mark_tail: AtomicU64::new(0),
            host_us: AtomicU64::new(0),
        }
    }

    fn bytes(&self) -> usize {
        self.pcm.len() * 4 + self.marks.len() * std::mem::size_of::<Mark>()
    }

    /// Producer side. Writes all of `input` or none of it; `false` means the ring is full.
    fn write<T: Sample>(&self, input: &[T], loss: &mut Loss, host_us: u64) -> bool
    where
        f32: FromSample<T>,
    {
        let frames = (input.len() / self.channels) as u64;
        let head = self.head.load(Ordering::Relaxed);
        if head - self.tail.load(Ordering::Acquire) + frames > self.capacity {
            return false;
        }
        if loss.frames > 0 {
            let at = self.mark_head.load(Ordering::Relaxed);
            if at - self.mark_tail.load(Ordering::Acquire) < MARKS {
                let mark = &self.marks[(at % MARKS) as usize];
                mark.at.store(head, Ordering::Relaxed);
                mark.from.store(loss.from, Ordering::Relaxed);
                mark.to.store(loss.to, Ordering::Relaxed);
                mark.frames.store(loss.frames, Ordering::Relaxed);
                self.mark_head.store(at + 1, Ordering::Release);
                *loss = Loss::default();
            }
        }
        let len = self.pcm.len() as u64;
        let mut index = (head * self.channels as u64) % len;
        for sample in input {
            let value: f32 = sample.to_sample();
            self.pcm[index as usize].store(value.to_bits(), Ordering::Relaxed);
            index += 1;
            if index == len {
                index = 0;
            }
        }
        self.host_us.store(host_us, Ordering::Relaxed);
        self.head.store(head + frames, Ordering::Release);
        true
    }
}

/// The consumer's view of one generation.
struct Reader {
    generation: u64,
    ring: Arc<Ring>,
    sample_rate: u32,
    channels: u16,
    epoch: u64,
    source_id: u64,
    /// Envelopes delivered so far.
    sequence: u64,
    /// Frames lost before the ring's tail, so offsets stay on the device timeline.
    lost: u64,
}

impl Reader {
    fn envelope(
        &mut self,
        frame_offset: u64,
        samples: Vec<f32>,
        gap: Option<CaptureGap>,
    ) -> CaptureEnvelope {
        self.sequence += 1;
        CaptureEnvelope {
            generation: self.generation,
            source_id: self.source_id,
            sample_rate: self.sample_rate,
            channels: self.channels,
            epoch: self.epoch,
            host_us: self.ring.host_us.load(Ordering::Relaxed),
            frame_offset,
            frame_count: (samples.len() / usize::from(self.channels.max(1))) as u32,
            sequence: self.sequence,
            gap,
            samples,
        }
    }

    /// Everything written so far, cut at each gap. `all` also takes a gap that
    /// no audio follows yet: the lease is ending.
    fn drain(&mut self, all: bool) -> Vec<CaptureEnvelope> {
        let ring = Arc::clone(&self.ring);
        let head = ring.head.load(Ordering::Acquire);
        let mark_head = ring.mark_head.load(Ordering::Acquire);
        let mut tail = ring.tail.load(Ordering::Relaxed);
        let mut mark_tail = ring.mark_tail.load(Ordering::Relaxed);
        let channels = ring.channels as u64;
        let len = ring.pcm.len() as u64;
        let mut out = Vec::new();
        loop {
            let mut gap = Loss::default();
            while mark_tail < mark_head {
                let mark = &ring.marks[(mark_tail % MARKS) as usize];
                if mark.at.load(Ordering::Relaxed) > tail || (tail == head && !all) {
                    break;
                }
                if gap.from == 0 {
                    gap.from = mark.from.load(Ordering::Relaxed);
                }
                gap.to = mark.to.load(Ordering::Relaxed);
                gap.frames += mark.frames.load(Ordering::Relaxed);
                mark_tail += 1;
            }
            self.lost += gap.frames;
            let next = if mark_tail < mark_head {
                ring.marks[(mark_tail % MARKS) as usize]
                    .at
                    .load(Ordering::Relaxed)
            } else {
                head
            };
            let frames = (next.min(head) - tail).min(ENVELOPE_FRAMES as u64);
            if frames == 0 {
                if gap.from != 0 {
                    out.push(self.envelope(tail + self.lost, Vec::new(), gap.gap()));
                }
                break;
            }
            let mut samples = Vec::with_capacity((frames * channels) as usize);
            let mut index = (tail * channels) % len;
            for _ in 0..frames * channels {
                samples.push(f32::from_bits(
                    ring.pcm[index as usize].load(Ordering::Relaxed),
                ));
                index += 1;
                if index == len {
                    index = 0;
                }
            }
            out.push(self.envelope(tail + self.lost, samples, gap.gap()));
            tail += frames;
        }
        ring.tail.store(tail, Ordering::Release);
        ring.mark_tail.store(mark_tail, Ordering::Release);
        out
    }
}

struct State {
    generation: u64,
    active: bool,
    closed: bool,
    /// Set once `release` has handed this generation back. A later release is stale.
    released: bool,
    sample_rate: u32,
    channels: u16,
    /// The producer's handle on this generation's ring. `None` between leases.
    ring: Option<Arc<Ring>>,
    /// Callbacks seen this generation. Sequences start at 1.
    next_sequence: u64,
    /// Loss not yet placed in the ring.
    loss: Loss,
    fault: Fault,
    detail: Option<String>,
}

impl State {
    fn empty() -> Self {
        Self {
            generation: 0,
            active: false,
            closed: false,
            released: false,
            sample_rate: 0,
            channels: 0,
            ring: None,
            next_sequence: 1,
            loss: Loss::default(),
            fault: Fault::None,
            detail: None,
        }
    }

    fn lose(&mut self, frames: u64) {
        if frames == 0 {
            return;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.loss.add(sequence, frames);
    }

    fn fault_message(&self) -> Option<String> {
        match self.fault {
            Fault::None => None,
            Fault::Hardware => Some(
                self.detail
                    .clone()
                    .unwrap_or_else(|| "microphone stream failed".to_owned()),
            ),
            Fault::Layout => Some("meeting capture stopped: incompatible live format".to_owned()),
            Fault::Superseded => self.detail.clone(),
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
    /// A fault is waiting in the state. Lets `pull` skip the state lock.
    faulted: AtomicBool,
    pending_chunks: AtomicU64,
    pending_frames: AtomicU64,
    /// Callbacks that already observed an active lease. `release` waits for zero
    /// before it finishes the gap, so a `try_lock` miss is not a silent tail.
    inflight: AtomicU64,
    /// `MeetingLease` drop could not queue a release. The audio thread closes hardware.
    close_stream: AtomicBool,
    /// The callback's side. Pulls never take it, so it is uncontended while audio flows.
    inner: Mutex<State>,
    /// The consumer's side.
    reader: Mutex<Option<Reader>>,
}

impl MeetingHub {
    pub(crate) fn inactive() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            hot: AtomicBool::new(false),
            level: AtomicU32::new(0),
            channels: AtomicU16::new(0),
            generation_flag: AtomicU64::new(0),
            faulted: AtomicBool::new(false),
            pending_chunks: AtomicU64::new(0),
            pending_frames: AtomicU64::new(0),
            inflight: AtomicU64::new(0),
            close_stream: AtomicBool::new(false),
            inner: Mutex::new(State::empty()),
            reader: Mutex::new(None),
        })
    }

    pub(crate) fn request_stream_close(&self) {
        self.close_stream.store(true, Ordering::Release);
    }

    pub(crate) fn take_stream_close(&self) -> bool {
        self.close_stream.load(Ordering::Acquire) && self.close_stream.swap(false, Ordering::AcqRel)
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

    fn reader(&self) -> std::sync::MutexGuard<'_, Option<Reader>> {
        self.reader.lock().unwrap_or_else(|p| p.into_inner())
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
        if self.hot.load(Ordering::Acquire) {
            return Err("a meeting capture is already active".to_owned());
        }
        // Allocated here, on the audio thread, never in the callback.
        let ring = Arc::new(Ring::new(format.sample_rate, format.channels));
        let mut reader = self.reader();
        let mut state = self.lock();
        if state.active {
            return Err("a meeting capture is already active".to_owned());
        }
        xflow_core::log(
            "info",
            &format!(
                "meeting microphone ring: {} bytes for {} Hz {} ch",
                ring.bytes(),
                format.sample_rate,
                format.channels
            ),
        );
        state.active = true;
        state.closed = false;
        state.released = false;
        state.sample_rate = format.sample_rate;
        state.channels = format.channels;
        state.ring = Some(Arc::clone(&ring));
        state.next_sequence = 1;
        state.loss = Loss::default();
        state.fault = Fault::None;
        state.detail = None;
        state.generation = state.generation.wrapping_add(1);
        if state.generation == 0 {
            state.generation = 1;
        }
        let generation = state.generation;
        *reader = Some(Reader {
            generation,
            ring,
            sample_rate: format.sample_rate,
            channels: format.channels,
            epoch: format.epoch,
            source_id: format.source_id,
            sequence: 0,
            lost: 0,
        });
        self.faulted.store(false, Ordering::Release);
        self.pending_chunks.store(0, Ordering::Relaxed);
        self.pending_frames.store(0, Ordering::Relaxed);
        self.channels.store(format.channels, Ordering::Release);
        self.generation_flag.store(generation, Ordering::Release);
        self.level.store(0, Ordering::Relaxed);
        self.hot.store(true, Ordering::SeqCst);
        Ok(generation)
    }

    pub(crate) fn push<T: Sample>(&self, input: &[T], opened: Instant) -> Option<(f32, u64)>
    where
        f32: FromSample<T>,
    {
        // Inactive lease: one atomic load. Do not add atomics on this path.
        // shortcut: inflight starts after this load so a no-lease callback stays one atomic; enter before the load when a missed tail after stop is worth those atomics.
        if !self.hot.load(Ordering::Acquire) || input.is_empty() {
            return None;
        }
        let channels = self.channels.load(Ordering::Acquire) as usize;
        if channels == 0 {
            return None;
        }
        let frames = (input.len() / channels) as u64;
        let _flight = Flight::enter(&self.inflight);
        if !self.hot.load(Ordering::SeqCst) {
            self.note_gap(frames);
            return None;
        }
        let host_us = opened.elapsed().as_micros() as u64;
        let rms = rms_of(input);
        // Only lease changes take this lock; a miss is a gap, never a wait.
        let mut state = match self.inner.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                self.note_gap(frames);
                return Some((rms, self.generation_flag.load(Ordering::Acquire)));
            }
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        if !state.active {
            // The callback already saw the lease. A sealed release still owes this gap.
            state.lose(frames.max(1));
            return None;
        }
        fold_pending(&mut state, &self.pending_chunks, &self.pending_frames);
        if !input.len().is_multiple_of(channels) {
            state.fault = Fault::Layout;
            self.faulted.store(true, Ordering::Release);
            state.lose(frames.max(1));
            return Some((rms, state.generation));
        }
        let State {
            ring,
            next_sequence,
            loss,
            ..
        } = &mut *state;
        let sequence = *next_sequence;
        *next_sequence = next_sequence.wrapping_add(1);
        if let Some(ring) = ring {
            if !ring.write(input, loss, host_us) {
                loss.add(sequence, frames);
            }
        }
        Some((rms, state.generation))
    }

    pub(crate) fn pull(&self, generation: u64) -> MeetingPull {
        let mut reader = self.reader();
        let Some(reader) = reader.as_mut().filter(|r| r.generation == generation) else {
            return MeetingPull::stale();
        };
        let fault = if self.faulted.load(Ordering::Acquire) {
            self.lock().fault_message()
        } else {
            None
        };
        MeetingPull {
            stale: false,
            envelopes: reader.drain(false),
            fault,
        }
    }

    /// Ends this generation and hands back whatever is still queued, including after
    /// `supersede`. A different generation, or a generation already released, is stale.
    pub(crate) fn release(&self, generation: u64) -> MeetingPull {
        {
            let mut state = self.lock();
            if generation == 0 || state.generation != generation || state.released {
                return MeetingPull::stale();
            }
            state.active = false;
            state.closed = true;
            state.released = true;
            self.hot.store(false, Ordering::SeqCst);
            self.level.store(0, Ordering::Relaxed);
        }
        self.collect(generation)
    }

    /// Frames that landed after `release` built its pull. Same generation only.
    pub(crate) fn fold_late(&self, generation: u64, pull: &mut MeetingPull) {
        while self.inflight.load(Ordering::SeqCst) != 0 {
            std::hint::spin_loop();
        }
        let mut state = self.lock();
        if state.generation != generation {
            return;
        }
        fold_pending(&mut state, &self.pending_chunks, &self.pending_frames);
        push_tail_gap(pull, &mut state);
    }

    fn note_gap(&self, frames: u64) {
        self.pending_chunks.fetch_add(1, Ordering::Relaxed);
        self.pending_frames
            .fetch_add(frames.max(1), Ordering::Relaxed);
    }

    /// The rest of a released generation, then its ring goes back to the allocator.
    fn collect(&self, generation: u64) -> MeetingPull {
        while self.inflight.load(Ordering::SeqCst) != 0 {
            std::hint::spin_loop();
        }
        let mut reader = self.reader();
        let mut state = self.lock();
        fold_pending(&mut state, &self.pending_chunks, &self.pending_frames);
        let mut pull = MeetingPull {
            stale: false,
            envelopes: match reader.take() {
                Some(mut r) if r.generation == generation => r.drain(true),
                _ => Vec::new(),
            },
            fault: state.fault_message(),
        };
        push_tail_gap(&mut pull, &mut state);
        state.ring = None;
        pull
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
        self.faulted.store(true, Ordering::Release);
        self.hot.store(false, Ordering::SeqCst);
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
        self.faulted.store(true, Ordering::Release);
    }
}

struct Flight<'a>(&'a AtomicU64);

impl<'a> Flight<'a> {
    fn enter(counter: &'a AtomicU64) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// Loss that no audio followed: one trailing gap, merged into a trailing gap already there.
fn push_tail_gap(pull: &mut MeetingPull, state: &mut State) {
    let loss = std::mem::take(&mut state.loss);
    let Some(gap) = loss.gap() else {
        return;
    };
    if let Some(last) = pull
        .envelopes
        .last_mut()
        .filter(|e| e.frame_count == 0 && e.gap.is_some())
    {
        let had = last.gap.unwrap();
        last.gap = Some(CaptureGap {
            from_sequence: had.from_sequence,
            to_sequence: gap.to_sequence,
            frames: had.frames + gap.frames,
        });
        return;
    }
    let (frame_offset, sequence) = pull.envelopes.last().map_or((0, 0), |e| {
        (e.frame_offset + u64::from(e.frame_count), e.sequence)
    });
    pull.envelopes.push(CaptureEnvelope {
        generation: state.generation,
        source_id: pull.envelopes.last().map_or(0, |e| e.source_id),
        sample_rate: state.sample_rate,
        channels: state.channels,
        epoch: pull.envelopes.last().map_or(0, |e| e.epoch),
        host_us: 0,
        frame_offset: frame_offset + gap.frames,
        frame_count: 0,
        sequence: sequence + 1,
        gap: Some(gap),
        samples: Vec::new(),
    });
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

    fn mono_48k(hub: &MeetingHub) -> u64 {
        hub.activate(Format {
            sample_rate: 48_000,
            channels: 1,
            epoch: 1,
            source_id: 1,
        })
        .unwrap()
    }

    fn delivered(pull: &MeetingPull) -> u64 {
        pull.envelopes
            .iter()
            .map(|e| u64::from(e.frame_count))
            .sum()
    }

    fn lost(pull: &MeetingPull) -> u64 {
        pull.envelopes
            .iter()
            .filter_map(|e| e.gap)
            .map(|g| g.frames)
            .sum()
    }

    /// The release blocker: a consumer that stops pulling must cost a gap, not the meeting.
    #[test]
    fn a_stalled_consumer_is_a_gap_and_the_lease_keeps_going() {
        let hub = MeetingHub::inactive();
        let generation = mono_48k(&hub);
        let mut dictation = Recording::new(48_000, 1, &RecordingConfig::default()).unwrap();
        let callback = vec![0.3_f32; 512];
        // Three seconds of 512-frame callbacks with nobody pulling.
        let started = Instant::now();
        for _ in 0..281 {
            hub.push(callback.as_slice(), Instant::now());
            dictation.push(callback.as_slice());
        }
        let full_ns = started.elapsed().as_nanos() / 281;
        let kept = 48_000 * RING_MS / 1000 / 512 * 512;
        let stalled = hub.pull(generation);
        assert!(stalled.fault.is_none(), "{:?}", stalled.fault);
        assert_eq!(delivered(&stalled), kept);
        assert_eq!(lost(&stalled), 0, "the gap is placed before the next audio");
        // The consumer is back: the next callback lands after one exact gap.
        let after = vec![0.7_f32; 512];
        hub.push(after.as_slice(), Instant::now());
        dictation.push(after.as_slice());
        let resumed = hub.pull(generation);
        assert!(resumed.fault.is_none());
        assert_eq!(resumed.envelopes.len(), 1);
        let first = &resumed.envelopes[0];
        assert_eq!(first.gap.unwrap().frames, 281 * 512 - kept);
        assert_eq!(first.samples, after);
        assert_eq!(
            first.frame_offset,
            281 * 512,
            "offsets stay on the device timeline"
        );
        // Audio keeps flowing with no further gap.
        hub.push(after.as_slice(), Instant::now());
        let later = hub.pull(generation);
        assert!(later.fault.is_none() && later.envelopes[0].gap.is_none());
        assert_eq!(later.envelopes[0].samples, after);
        assert_eq!(
            dictation.samples.len(),
            282 * 512,
            "dictation kept every frame"
        );
        assert!(hub.is_active());
        let rest = hub.release(generation);
        assert!(rest.fault.is_none() && !rest.stale);
        eprintln!(
            "meeting ring bytes={} (48 kHz mono, {RING_MS} ms) full_callback_ns={full_ns}",
            Ring::new(48_000, 1).bytes()
        );
        assert!(full_ns < 50_000_000, "full callback {full_ns} ns");
    }

    /// A pull used to hold the callback's lock, and one missed `try_lock` ended the meeting.
    #[test]
    fn pulls_racing_the_callback_lose_nothing() {
        let hub = MeetingHub::inactive();
        let generation = mono_48k(&hub);
        let producer = {
            let hub = Arc::clone(&hub);
            std::thread::spawn(move || {
                let mut next = 0u32;
                for _ in 0..2_000 {
                    let chunk: Vec<f32> = (0..512).map(|i| (next + i) as f32).collect();
                    next += 512;
                    hub.push(chunk.as_slice(), Instant::now());
                    std::thread::sleep(Duration::from_micros(50));
                }
            })
        };
        let mut samples = Vec::new();
        let mut gaps = 0;
        while !producer.is_finished() {
            let pull = hub.pull(generation);
            assert!(pull.fault.is_none(), "{:?}", pull.fault);
            gaps += pull.envelopes.iter().filter(|e| e.gap.is_some()).count();
            samples.extend(pull.envelopes.into_iter().flat_map(|e| e.samples));
        }
        producer.join().unwrap();
        let rest = hub.release(generation);
        gaps += rest.envelopes.iter().filter(|e| e.gap.is_some()).count();
        samples.extend(rest.envelopes.into_iter().flat_map(|e| e.samples));
        assert_eq!(gaps, 0);
        assert_eq!(samples.len(), 2_000 * 512);
        assert!(
            samples.iter().enumerate().all(|(i, s)| *s == i as f32),
            "order kept across ring wraps"
        );
    }

    #[test]
    fn many_separate_gaps_each_land_before_their_audio() {
        let hub = MeetingHub::inactive();
        let generation = mono_48k(&hub);
        let fill = vec![0.1_f32; ENVELOPE_FRAMES];
        let capacity = Ring::new(48_000, 1).capacity as usize;
        for round in 0..3u64 {
            while hub
                .pull(generation)
                .envelopes
                .iter()
                .any(|e| e.frame_count > 0)
            {}
            for _ in 0..capacity / ENVELOPE_FRAMES {
                hub.push(fill.as_slice(), Instant::now());
            }
            let tail = capacity % ENVELOPE_FRAMES;
            hub.push(&fill[..tail], Instant::now());
            // Full: these two callbacks are lost.
            hub.push(&[0.2_f32; 100], Instant::now());
            hub.push(&[0.2_f32; 50], Instant::now());
            let pulled = hub.pull(generation);
            assert_eq!(delivered(&pulled), capacity as u64, "round {round}");
            hub.push(&[0.9_f32; 10], Instant::now());
            let next = hub.pull(generation);
            let gap = next.envelopes[0].gap.unwrap();
            assert_eq!(gap.frames, 150);
            assert_eq!(gap.to_sequence - gap.from_sequence, 1);
            assert_eq!(next.envelopes[0].samples, vec![0.9; 10]);
            assert!(next.fault.is_none());
        }
        // A loss nothing followed is handed back at release.
        for chunk in vec![0.4_f32; capacity].chunks(ENVELOPE_FRAMES) {
            hub.push(chunk, Instant::now());
        }
        hub.push(&[0.5_f32; 7], Instant::now());
        let rest = hub.release(generation);
        assert_eq!(rest.envelopes.last().unwrap().gap.unwrap().frames, 7);
        assert_eq!(rest.envelopes.last().unwrap().frame_count, 0);
        assert_eq!(delivered(&rest), capacity as u64);
    }

    #[test]
    fn the_ring_is_sized_to_the_live_format_and_freed_with_the_lease() {
        assert_eq!(Ring::new(48_000, 1).capacity, 96_000);
        assert_eq!(Ring::new(48_000, 1).pcm.len(), 96_000);
        assert_eq!(Ring::new(16_000, 2).pcm.len(), 64_000);
        let wide = Ring::new(MAX_RATE, MAX_CHANNELS);
        assert!(wide.pcm.len() * 4 <= RING_MAX_BYTES);
        let hub = MeetingHub::inactive();
        assert!(hub.lock().ring.is_none(), "no lease, no ring");
        let generation = mono_48k(&hub);
        assert!(hub.lock().ring.is_some());
        hub.release(generation);
        assert!(
            hub.lock().ring.is_none(),
            "idle memory goes back after the meeting"
        );
        assert!(hub.reader().is_none());
    }

    #[test]
    fn a_held_meeting_lock_does_not_stall_the_callback() {
        let hub = MeetingHub::inactive();
        hub.activate(format()).unwrap();
        let guard = hub.inner.lock().unwrap();
        let started = Instant::now();
        for _ in 0..100 {
            hub.push(&[0.1_f32; 128], Instant::now());
        }
        let ns = started.elapsed().as_nanos() / 100;
        eprintln!("meeting callback while sink holds the queue: {ns} ns");
        assert!(ns < 50_000_000, "blocked callback {ns} ns");
        drop(guard);
        let generation = hub.generation_flag.load(Ordering::Acquire);
        hub.push(&[0.6_f32; 4], Instant::now());
        let pulled = hub.pull(generation);
        assert!(pulled.fault.is_none());
        assert_eq!(pulled.envelopes[0].gap.unwrap().frames, 100 * 128);
        assert_eq!(pulled.envelopes[0].samples, vec![0.6; 4]);
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
        let frames = ENVELOPE_FRAMES + 3;
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
        assert_eq!(pulled.envelopes[0].frame_count, ENVELOPE_FRAMES as u32);
        assert_eq!(pulled.envelopes[1].frame_count, 3);
        assert_eq!(
            pulled.envelopes[0].sequence + 1,
            pulled.envelopes[1].sequence
        );
        assert_eq!(pulled.envelopes[0].frame_offset, 0);
        assert_eq!(pulled.envelopes[1].frame_offset, ENVELOPE_FRAMES as u64);
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

    #[test]
    fn release_after_supersede_returns_the_queued_audio_and_the_reason() {
        let hub = MeetingHub::inactive();
        let generation = hub.activate(format()).unwrap();
        hub.push(&[0.4_f32, 0.6], Instant::now());
        assert!(hub.supersede(
            "meeting capture stopped: dictation switched microphone; dictation settings were kept"
        ));
        let pulled = hub.release(generation);
        assert!(!pulled.stale);
        assert_eq!(pulled.envelopes[0].samples, vec![0.4, 0.6]);
        assert!(pulled.fault.unwrap().contains("switched microphone"));
        assert!(hub.release(generation).stale);
    }

    #[test]
    fn callback_that_saw_the_lease_after_it_was_sealed_is_a_gap() {
        let hub = MeetingHub::inactive();
        let generation = hub.activate(format()).unwrap();
        {
            let mut state = hub.inner.lock().unwrap();
            state.active = false;
        }
        hub.push(&[0.5_f32, 0.25, 0.125], Instant::now());
        let pulled = hub.release(generation);
        assert!(!pulled.stale);
        let gap = pulled
            .envelopes
            .iter()
            .find_map(|envelope| envelope.gap)
            .unwrap();
        assert_eq!(gap.frames, 3);
        let delivered: u64 = pulled
            .envelopes
            .iter()
            .map(|envelope| u64::from(envelope.frame_count))
            .sum();
        assert_eq!(delivered, 0);
    }
}
