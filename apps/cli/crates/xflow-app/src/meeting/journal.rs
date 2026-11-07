//! Durable meeting audio. Each source is cut into 5 s pieces of 16 kHz mono
//! PCM16. A piece is written to a private `.part`, fsynced, renamed, its
//! directory fsynced, and only then recorded by a FULL-sync SQLite commit. That
//! commit is the only acknowledgement. Recovery is local: it never captures
//! audio and never sends anything to a provider.
use super::fault;
use super::store::{open_writer, store_id};
use anyhow::{anyhow, bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use xflow_core::meeting::{Gap, MeetingId, Source, TimeRange};

pub const RATE: i64 = 16_000;
/// Samples per durable piece: 5 s.
pub const PIECE_SAMPLES: usize = 80_000;
/// New audio per STT job: 30 s, plus 2 s of the previous audio for context.
pub const WINDOW_SAMPLES: i64 = 480_000;
pub const OVERLAP_SAMPLES: i64 = 32_000;
pub const MAX_SOURCES: usize = 2;
pub const MAX_SESSION_MS: i64 = 3 * 3_600_000;
/// Hard cap on jobs per meeting, tails and splits included.
pub const MAX_JOBS: i64 = 1024;
/// Shorter tails carry no speech worth a request; they are recorded as a gap.
const MIN_JOB_SAMPLES: i64 = 1_600;
const HEADER: usize = 88;
const MAGIC: &[u8; 8] = b"XFMPIECE";
const PIECE_BYTES: u64 = (HEADER + PIECE_SAMPLES * 2) as u64;

/// Disk policy. Quotas refuse new capture or stop it visibly; they never
/// evict audio that has not been transcribed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub session_bytes: u64,
    pub global_bytes: u64,
    /// Free space that must remain on the filesystem.
    pub reserve_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            session_bytes: 1 << 30,
            global_bytes: 2 << 30,
            reserve_bytes: 1 << 30,
        }
    }
}
impl Limits {
    /// Test-support builds may shrink limits with `XFLOW_MEETING_LIMITS=session,global,reserve`.
    pub fn from_env() -> Self {
        #[cfg(feature = "test-support")]
        if let Ok(value) = std::env::var("XFLOW_MEETING_LIMITS") {
            let n: Vec<u64> = value.split(',').filter_map(|v| v.parse().ok()).collect();
            if let [session_bytes, global_bytes, reserve_bytes] = n[..] {
                return Self {
                    session_bytes,
                    global_bytes,
                    reserve_bytes,
                };
            }
        }
        Self::default()
    }
}

pub fn spool_root(db_path: &Path) -> PathBuf {
    db_path.with_file_name("meeting-audio")
}
fn meeting_dir(root: &Path, store: &str, meeting: i64) -> PathBuf {
    root.join(format!("{store}-{meeting}"))
}
fn piece_name(source: i64, seq: i64) -> String {
    format!("s{source}-{seq:06}.pcm")
}
fn parse_piece_name(name: &str) -> Option<(i64, i64)> {
    let rest = name.strip_prefix('s')?.strip_suffix(".pcm")?;
    let (source, seq) = rest.split_once('-')?;
    let (source, seq) = (source.parse().ok()?, seq.parse().ok()?);
    (piece_name(source, seq) == name).then_some((source, seq))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
    store: [u8; 16],
    meeting: i64,
    source: u32,
    seq: u64,
    start: u64,
    samples: u32,
    payload_hash: u64,
}
fn store_bytes(store: &str) -> Result<[u8; 16]> {
    ensure!(store.len() == 32, "invalid store id");
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&store[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}
impl Header {
    fn encode(&self) -> [u8; HEADER] {
        let mut b = [0u8; HEADER];
        b[0..8].copy_from_slice(MAGIC);
        b[8..10].copy_from_slice(&1u16.to_le_bytes());
        b[10..12].copy_from_slice(&(HEADER as u16).to_le_bytes());
        b[12..28].copy_from_slice(&self.store);
        b[28..36].copy_from_slice(&self.meeting.to_le_bytes());
        b[36..40].copy_from_slice(&self.source.to_le_bytes());
        // 40..44: epoch, always 0 until a recorder reports format changes.
        b[44..52].copy_from_slice(&self.seq.to_le_bytes());
        b[52..60].copy_from_slice(&self.start.to_le_bytes());
        b[60..64].copy_from_slice(&(RATE as u32).to_le_bytes());
        b[64..66].copy_from_slice(&1u16.to_le_bytes()); // channels
        b[66..68].copy_from_slice(&1u16.to_le_bytes()); // PCM16LE
        b[68..72].copy_from_slice(&self.samples.to_le_bytes());
        b[72..80].copy_from_slice(&self.payload_hash.to_le_bytes());
        let hash = xflow_core::stable_hash(&b[..80]);
        b[80..88].copy_from_slice(&hash.to_le_bytes());
        b
    }
    fn decode(b: &[u8]) -> Option<Self> {
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if b.len() < HEADER
            || &b[0..8] != MAGIC
            || u16_at(8) != 1
            || u16_at(10) as usize != HEADER
            || u64_at(80) != xflow_core::stable_hash(&b[..80])
            || u32_at(40) != 0
            || i64::from(u32_at(60)) != RATE
            || u16_at(64) != 1
            || u16_at(66) != 1
        {
            return None;
        }
        let samples = u32_at(68);
        (samples > 0 && samples as usize <= PIECE_SAMPLES).then(|| Self {
            store: b[12..28].try_into().unwrap(),
            meeting: i64::from_le_bytes(b[28..36].try_into().unwrap()),
            source: u32_at(36),
            seq: u64_at(44),
            start: u64_at(52),
            samples,
            payload_hash: u64_at(72),
        })
    }
}

/// Reads and fully validates one piece; `None` when it is torn or corrupt.
fn read_piece(path: &Path) -> Result<Option<(Header, Vec<u8>)>> {
    let mut file = match open_nofollow(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(e.into()),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::with_capacity(PIECE_BYTES as usize);
    (&mut file).take(PIECE_BYTES + 1).read_to_end(&mut bytes)?;
    let Some(header) = Header::decode(&bytes) else {
        return Ok(None);
    };
    let payload = bytes.split_off(HEADER);
    if payload.len() != header.samples as usize * 2
        || xflow_core::stable_hash(&payload) != header.payload_hash
    {
        return Ok(None);
    }
    Ok(Some((header, payload)))
}
fn open_nofollow(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}
/// Owner-only lock file; never follows a symlink.
pub fn lock_file(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}
fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

/// The file protocol. Returns only after the renamed file and its directory
/// entry are on disk; the caller's commit then acknowledges it.
fn write_piece(dir: &Path, name: &str, header: &Header, payload: &[u8]) -> std::io::Result<()> {
    let part = dir.join(format!("{name}.part"));
    let target = dir.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(&part)?;
    let written = (|| {
        fault("piece:open")?;
        let mut bytes = Vec::with_capacity(HEADER + payload.len());
        bytes.extend_from_slice(&header.encode());
        bytes.extend_from_slice(payload);
        let half = bytes.len() / 2;
        file.write_all(&bytes[..half])?;
        // A fault here leaves a torn `.part`: header and half the samples.
        fault("piece:half")?;
        file.write_all(&bytes[half..])?;
        fault("piece:written")?;
        file.sync_all()?;
        fault("piece:synced")
    })();
    drop(file);
    if let Err(error) = written {
        // Never acknowledged; removing it loses nothing that was promised.
        let _ = std::fs::remove_file(&part);
        return Err(error);
    }
    match std::fs::symlink_metadata(&target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            let _ = std::fs::remove_file(&part);
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "piece already exists",
            ));
        }
        Err(e) => return Err(e),
    }
    std::fs::rename(&part, &target)?;
    fault("piece:renamed")?;
    fsync_dir(dir)?;
    fault("piece:dirsynced")
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Free bytes for this user on the spool's filesystem.
fn free_bytes(path: &Path) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::statvfs(c.as_ptr(), &mut stat) } == 0,
            "could not read free disk space: {}",
            std::io::Error::last_os_error()
        );
        #[allow(clippy::unnecessary_cast)]
        Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(u64::MAX)
    }
}

/// Bytes of retained audio: acknowledged pieces plus quarantined files.
pub(crate) fn usage(db: &Connection, root: &Path, meeting: Option<i64>) -> Result<u64> {
    let pieces: i64 = db.query_row(
        "SELECT COALESCE(SUM(bytes),0) FROM pieces WHERE state IN ('ready','unlink_pending') AND (?1 IS NULL OR meeting_id=?1)",
        [meeting],
        |r| r.get(0),
    )?;
    let mut total = pieces as u64;
    if meeting.is_none() {
        total += quarantine_bytes(root);
    }
    Ok(total)
}
fn quarantine_bytes(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|e| std::fs::read_dir(e.path().join("quarantine")).ok())
        .flat_map(|d| d.flatten())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Why a capture stopped early. Shown to the user; nothing is deleted.
#[derive(Debug)]
pub struct Stopped(pub String);
impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "capture stopped: {}", self.0)
    }
}
impl std::error::Error for Stopped {}

pub struct CaptureSpec {
    pub title: Option<String>,
    /// One label per source, at most `MAX_SOURCES`.
    pub labels: Vec<String>,
    /// Known total length (file input); `None` for live audio.
    pub expected_ms: Option<i64>,
    /// The user asked for transcription: the background worker may send
    /// this meeting's audio to the provider. Recovery never sets it.
    pub queued: bool,
}

struct SourceBuf {
    id: String,
    buf: Vec<i16>,
    next_seq: i64,
    next_sample: i64,
    /// The live gap this source is still in: (gap row, start sample, reason).
    gap: Option<(i64, i64, String)>,
}

/// One meeting being written. The later live recorder drives this same type.
pub struct Capture {
    db: Connection,
    dir: PathBuf,
    root: PathBuf,
    store: String,
    store_bytes: [u8; 16],
    meeting: i64,
    sources: Vec<SourceBuf>,
    limits: Limits,
    expected_ms: Option<i64>,
    session: u64,
    global: u64,
    /// Visible notices for the caller, such as the 80% quota warning.
    pub warnings: Vec<String>,
    warned: bool,
    stopped: bool,
    _lock: File,
}

/// How long a recorder commit waits for another writer before it fails.
const RECORDER_BUSY: Duration = Duration::from_secs(10);

impl Capture {
    pub fn begin(db_path: &Path, spec: CaptureSpec, limits: Limits) -> Result<Self> {
        ensure!(
            (1..=MAX_SOURCES).contains(&spec.labels.len()),
            "a meeting takes 1 to {MAX_SOURCES} audio sources"
        );
        if let Some(ms) = spec.expected_ms {
            ensure!(
                ms <= MAX_SESSION_MS,
                Stopped("audio is longer than the 3 hour meeting limit".into())
            );
        }
        let mut db = open_writer(db_path)?;
        let store = store_id(&db)?;
        let root = spool_root(db_path);
        crate::paths::private_dir(&root)?;
        let session_need = spec
            .expected_ms
            .map(|ms| (ms as u64 * RATE as u64 / 1000).div_ceil(PIECE_SAMPLES as u64) * PIECE_BYTES)
            .unwrap_or(0)
            * spec.labels.len() as u64;
        let global = usage(&db, &root, None)?;
        let free = free_bytes(&root)?;
        let mut warnings = Vec::new();
        ensure!(
            session_need <= limits.session_bytes,
            Stopped(format!(
                "needs {} MiB, over the {} MiB per-meeting audio limit",
                mib(session_need),
                mib(limits.session_bytes)
            ))
        );
        ensure!(
            global + session_need + PIECE_BYTES <= limits.global_bytes,
            Stopped(format!(
                "meeting audio would use {} MiB of the {} MiB limit; transcribe or delete waiting meetings first",
                mib(global + session_need),
                mib(limits.global_bytes)
            ))
        );
        ensure!(
            free >= limits.reserve_bytes + session_need + PIECE_BYTES,
            Stopped(format!(
                "only {} MiB free; {} MiB must stay free",
                mib(free),
                mib(limits.reserve_bytes)
            ))
        );
        if global + session_need >= limits.global_bytes / 5 * 4 {
            warnings.push(format!(
                "meeting audio will use {} MiB, over 80% of the {} MiB limit",
                mib(global + session_need),
                mib(limits.global_bytes)
            ));
        }
        let now = now_ms();
        let mut tag = [0u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut tag))
            .context("could not read random bytes")?;
        let import_id = format!(
            "audio:{}",
            tag.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO meetings(import_id,import_hash,import_json,title,started_at_ms,ended_at_ms,timezone) VALUES (?1,'none',x'',?2,?3,?4,'Z')",
            params![import_id, spec.title, now, now + spec.expected_ms.unwrap_or(1).max(1)])?;
        let meeting = tx.last_insert_rowid();
        let mut sources = Vec::new();
        for (idx, label) in spec.labels.iter().enumerate() {
            let source = Source {
                id: format!("audio{}", idx + 1),
                label: Some(label.clone()),
                epoch: 0,
                provider: None,
                model: None,
            };
            tx.execute(
                "INSERT INTO sources(meeting_id,id,json) VALUES (?1,?2,?3)",
                params![meeting, source.id, serde_json::to_string(&source)?],
            )?;
            tx.execute(
                "INSERT INTO capture_sources(meeting_id,idx,source_id) VALUES (?1,?2,?3)",
                params![meeting, idx as i64, source.id],
            )?;
            sources.push(SourceBuf {
                id: source.id,
                buf: Vec::with_capacity(PIECE_SAMPLES),
                next_seq: 0,
                next_sample: 0,
                gap: None,
            });
        }
        tx.execute(
            "INSERT INTO captures(meeting_id,state,created_ms,expected_ms,queued) VALUES (?1,'capturing',?2,?3,?4)",
            params![meeting, now, spec.expected_ms, spec.queued],
        )?;
        tx.commit()?;
        // A piece that cannot commit stops the capture. Another writer (an
        // import, a delete, crash recovery) can hold the lock past the usual
        // 1 s on a busy Mac, so pieces wait longer; the recorder's audio queue
        // holds the audio meanwhile. Startup keeps the 1 s wait: the recorder
        // does not read the microphone until this returns.
        db.busy_timeout(RECORDER_BUSY)?;
        let dir = meeting_dir(&root, &store, meeting);
        crate::paths::private_dir(&dir)?;
        let lock = lock_file(&dir.join("capture.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock).context("capture lock is busy")?;
        Ok(Self {
            db,
            dir,
            root,
            store_bytes: store_bytes(&store)?,
            store,
            meeting,
            sources,
            limits,
            expected_ms: spec.expected_ms,
            session: 0,
            global,
            warnings,
            warned: false,
            stopped: false,
            _lock: lock,
        })
    }
    pub fn id(&self) -> MeetingId {
        MeetingId {
            store_id: self.store.clone(),
            id: self.meeting,
        }
    }
    /// 16 kHz mono samples for one source. Complete 5 s pieces become durable
    /// before this returns. An error means the capture stopped visibly.
    pub fn push(&mut self, source: usize, samples: &[i16]) -> Result<()> {
        ensure!(!self.stopped, "capture already stopped");
        ensure!(source < self.sources.len(), "unknown source");
        if !samples.is_empty() {
            self.sources[source].gap = None;
        }
        let mut rest = samples;
        while !rest.is_empty() {
            let buf = &mut self.sources[source].buf;
            let take = (PIECE_SAMPLES - buf.len()).min(rest.len());
            buf.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if buf.len() == PIECE_SAMPLES {
                self.flush(source)?;
            }
        }
        Ok(())
    }
    /// Live audio a source never delivered: what is buffered becomes durable,
    /// the missing range is recorded as a gap, and the source continues after
    /// it. No samples are invented.
    pub fn gap(&mut self, source: usize, samples: i64, reason: &str) -> Result<()> {
        ensure!(!self.stopped, "capture already stopped");
        ensure!(source < self.sources.len(), "unknown source");
        if samples <= 0 {
            return Ok(());
        }
        self.flush(source)?;
        let s = &mut self.sources[source];
        let start = s.next_sample;
        // A source that is still silent extends its gap instead of adding rows.
        match &s.gap {
            Some((row, from, why)) if why == reason => {
                let gap = Gap {
                    source_id: s.id.clone(),
                    range: TimeRange {
                        start_ms: from * 1000 / RATE,
                        end_ms: (start + samples) * 1000 / RATE,
                    },
                    reason: reason.into(),
                };
                self.db.execute(
                    "UPDATE gaps SET json=?2 WHERE rowid=?1",
                    params![row, serde_json::to_string(&gap)?],
                )?;
            }
            _ => {
                add_gap(
                    &self.db,
                    self.meeting,
                    &s.id,
                    start * 1000 / RATE,
                    (start + samples) * 1000 / RATE,
                    reason,
                )?;
                s.gap = Some((self.db.last_insert_rowid(), start, reason.into()));
            }
        }
        s.next_sample += samples;
        Ok(())
    }
    /// Samples written or skipped so far for one source.
    pub fn position(&self, source: usize) -> i64 {
        let s = &self.sources[source];
        s.next_sample + s.buf.len() as i64
    }
    fn flush(&mut self, source: usize) -> Result<()> {
        if self.sources[source].buf.is_empty() {
            return Ok(());
        }
        match self.flush_inner(source) {
            Ok(()) => Ok(()),
            Err(error) => {
                let reason = format!("{error:#}");
                // Already acknowledged pieces stay; only the unwritten tail becomes a gap.
                let _ = self.stop(&reason);
                Err(error.context(Stopped(reason)))
            }
        }
    }
    fn flush_inner(&mut self, source: usize) -> Result<()> {
        let bytes = (HEADER + self.sources[source].buf.len() * 2) as u64;
        if self.session + bytes > self.limits.session_bytes {
            bail!(
                "per-meeting audio limit of {} MiB reached",
                mib(self.limits.session_bytes)
            );
        }
        if self.global + bytes > self.limits.global_bytes {
            bail!(
                "meeting audio limit of {} MiB reached",
                mib(self.limits.global_bytes)
            );
        }
        if free_bytes(&self.root)? < self.limits.reserve_bytes + bytes {
            bail!(
                "free disk space is below the {} MiB reserve",
                mib(self.limits.reserve_bytes)
            );
        }
        let s = &self.sources[source];
        let payload: Vec<u8> = s.buf.iter().flat_map(|v| v.to_le_bytes()).collect();
        let header = Header {
            store: self.store_bytes,
            meeting: self.meeting,
            source: source as u32,
            seq: s.next_seq as u64,
            start: s.next_sample as u64,
            samples: s.buf.len() as u32,
            payload_hash: xflow_core::stable_hash(&payload),
        };
        let name = piece_name(source as i64, s.next_seq);
        write_piece(&self.dir, &name, &header, &payload).with_context(|| {
            format!(
                "could not write meeting audio ({})",
                io_reason_of(&self.dir)
            )
        })?;
        let (seq, start, count) = (s.next_seq, s.next_sample, s.buf.len() as i64);
        let committed = (|| -> Result<()> {
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM captures WHERE meeting_id=?1",
                    [self.meeting],
                    |r| r.get(0),
                )
                .optional()?;
            ensure!(
                state.as_deref() == Some("capturing"),
                "the meeting was deleted or closed during capture"
            );
            tx.execute("INSERT INTO pieces(meeting_id,source_idx,seq,start_sample,samples,bytes,hash,state,created_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,'ready',?8)",
                params![self.meeting, source as i64, seq, start, count, bytes as i64, format!("{:016x}", header.payload_hash), now_ms()])?;
            tx.execute("UPDATE capture_sources SET next_seq=?3,next_sample=?4 WHERE meeting_id=?1 AND idx=?2",
                params![self.meeting, source as i64, seq + 1, start + count])?;
            plan_jobs(&tx, self.meeting, source as i64, start + count, false)?;
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = committed {
            // Not acknowledged, so it is not kept: a deleted meeting must not return.
            let _ = std::fs::remove_file(self.dir.join(&name));
            return Err(error);
        }
        fault("piece:committed")?;
        self.session += bytes;
        self.global += bytes;
        let s = &mut self.sources[source];
        s.next_seq += 1;
        s.next_sample += count;
        s.buf.clear();
        if !self.warned
            && (self.session >= self.limits.session_bytes / 5 * 4
                || self.global >= self.limits.global_bytes / 5 * 4)
        {
            self.warned = true;
            self.warnings.push(format!(
                "meeting audio is over 80% of its limit ({} of {} MiB)",
                mib(self.global),
                mib(self.limits.global_bytes)
            ));
        }
        Ok(())
    }
    /// Ends early and visibly, keeping every durable piece; the rest is a gap.
    pub fn fail(mut self, reason: &str) -> Result<MeetingId> {
        if !self.stopped {
            for source in 0..self.sources.len() {
                if self.flush(source).is_err() {
                    return Ok(self.id());
                }
            }
            self.stop(reason)?;
        }
        Ok(self.id())
    }
    /// Flush the short last pieces and close every source.
    pub fn finish(mut self) -> Result<MeetingId> {
        for source in 0..self.sources.len() {
            self.flush(source)?;
        }
        self.close("captured", None)?;
        Ok(self.id())
    }
    fn stop(&mut self, reason: &str) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        self.close("stopped", Some(reason))
    }
    fn close(&mut self, state: &str, reason: Option<&str>) -> Result<()> {
        let now = now_ms();
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ends: Vec<(String, i64)> = self
            .sources
            .iter()
            .map(|s| (s.id.clone(), s.next_sample))
            .collect();
        close_capture(
            &tx,
            self.meeting,
            state,
            reason,
            &ends,
            self.expected_ms,
            now,
        )?;
        tx.commit()?;
        Ok(())
    }
}
fn mib(bytes: u64) -> u64 {
    bytes.div_ceil(1 << 20)
}
fn io_reason_of(dir: &Path) -> String {
    format!("meeting audio folder {}", dir.display())
}

/// Ends a capture: tail jobs, gaps for audio that never became durable, and
/// the meeting's length. Shared by finish, a visible stop and recovery.
fn close_capture(
    db: &Connection,
    meeting: i64,
    state: &str,
    reason: Option<&str>,
    ends: &[(String, i64)],
    expected_ms: Option<i64>,
    now: i64,
) -> Result<()> {
    let mut longest = 0;
    for (idx, (source_id, durable)) in ends.iter().enumerate() {
        db.execute(
            "UPDATE capture_sources SET finished=1 WHERE meeting_id=?1 AND idx=?2",
            params![meeting, idx as i64],
        )?;
        plan_jobs(db, meeting, idx as i64, *durable, true)?;
        let durable_ms = durable * 1000 / RATE;
        longest = longest.max(durable_ms);
        if let (Some(expected), Some(reason)) = (expected_ms, reason) {
            if expected > durable_ms {
                add_gap(db, meeting, source_id, durable_ms, expected, reason)?;
            }
        }
    }
    let longest = longest.max(expected_ms.unwrap_or(0)).max(1);
    db.execute(
        "UPDATE meetings SET ended_at_ms=started_at_ms+?2 WHERE id=?1",
        params![meeting, longest],
    )?;
    db.execute(
        "UPDATE captures SET state=?2,reason=?3,ended_ms=?4 WHERE meeting_id=?1",
        params![meeting, state, reason, now],
    )?;
    Ok(())
}

pub(crate) fn add_gap(
    db: &Connection,
    meeting: i64,
    source_id: &str,
    start_ms: i64,
    end_ms: i64,
    reason: &str,
) -> Result<()> {
    if end_ms <= start_ms {
        return Ok(());
    }
    let mut reason = reason.to_owned();
    if reason.len() > 128 {
        let mut cut = 125;
        while !reason.is_char_boundary(cut) {
            cut -= 1;
        }
        reason.truncate(cut);
        reason.push_str("...");
    }
    let gap = Gap {
        source_id: source_id.into(),
        range: TimeRange { start_ms, end_ms },
        reason,
    };
    db.execute(
        "INSERT INTO gaps(meeting_id,source_id,json) VALUES (?1,?2,?3)",
        params![meeting, source_id, serde_json::to_string(&gap)?],
    )?;
    Ok(())
}

/// Creates every STT job whose audio is now durable. Jobs hold sample ranges,
/// never audio. The job after a window shares its last 2 s.
fn plan_jobs(
    db: &Connection,
    meeting: i64,
    source: i64,
    durable: i64,
    finished: bool,
) -> Result<()> {
    let mut win: i64 = db.query_row(
        "SELECT COALESCE(MAX(win)+1,0) FROM jobs WHERE meeting_id=?1 AND source_idx=?2",
        params![meeting, source],
        |r| r.get(0),
    )?;
    loop {
        let start = win * WINDOW_SAMPLES;
        if start >= durable {
            return Ok(());
        }
        let mut end = start + WINDOW_SAMPLES;
        if end > durable {
            if !finished {
                return Ok(());
            }
            end = durable;
        }
        let count: i64 = db.query_row(
            "SELECT count(*) FROM jobs WHERE meeting_id=?1",
            [meeting],
            |r| r.get(0),
        )?;
        ensure!(count < MAX_JOBS, "meeting reached the {MAX_JOBS} job limit");
        let tiny = end - start < MIN_JOB_SAMPLES;
        db.execute("INSERT INTO jobs(meeting_id,source_idx,win,overlap_sample,start_sample,end_sample,state,error) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![meeting, source, win, (start - OVERLAP_SAMPLES).max(0), start, end,
                if tiny {"retained"} else {"pending"}, tiny.then_some("under 100 ms; not sent")])?;
        if tiny {
            let source_id: String = db.query_row(
                "SELECT source_id FROM capture_sources WHERE meeting_id=?1 AND idx=?2",
                params![meeting, source],
                |r| r.get(0),
            )?;
            add_gap(
                db,
                meeting,
                &source_id,
                start * 1000 / RATE,
                (end * 1000 / RATE).max(start * 1000 / RATE + 1),
                "under 100 ms of audio; not transcribed",
            )?;
        }
        win += 1;
    }
}

/// Pieces whose every dependent job is accepted. While a source
/// is still capturing, a piece reaching into the next window's 2 s overlap
/// waits for that window. Run inside the accepting transaction.
pub(crate) fn release_pieces(db: &Connection, meeting: i64) -> Result<usize> {
    // Old versions allowed abandoned jobs to authorize unlink; revoke and revalidate.
    db.execute(
        "UPDATE pieces SET state='ready' WHERE meeting_id=?1 AND state='unlink_pending'",
        [meeting],
    )?;
    const OVERLAPS: &str = "j.meeting_id=pieces.meeting_id AND j.source_idx=pieces.source_idx AND j.overlap_sample<pieces.start_sample+pieces.samples AND j.end_sample>pieces.start_sample";
    Ok(db.execute(&format!(
        "UPDATE pieces SET state='unlink_pending' WHERE meeting_id=?1 AND state IN ('ready','missing')
         AND EXISTS (SELECT 1 FROM jobs j WHERE {OVERLAPS})
         AND NOT EXISTS (SELECT 1 FROM jobs j WHERE {OVERLAPS} AND j.state!='accepted')
         AND (EXISTS (SELECT 1 FROM capture_sources c WHERE c.meeting_id=pieces.meeting_id AND c.idx=pieces.source_idx AND c.finished=1)
              OR pieces.start_sample+pieces.samples<=(SELECT (MAX(j.win)+1)*{WINDOW_SAMPLES}-{OVERLAP_SAMPLES} FROM jobs j WHERE j.meeting_id=pieces.meeting_id AND j.source_idx=pieces.source_idx))"),
        [meeting])?)
}

/// The deferred unlink after a durable result: remove files, fsync the
/// directory, then forget the rows. Safe to repeat after a crash.
pub(crate) fn unlink_released(db: &Connection, db_path: &Path, meeting: i64) -> Result<usize> {
    let tx = db.unchecked_transaction()?;
    release_pieces(&tx, meeting)?;
    tx.commit()?;
    let store = store_id(db)?;
    let dir = meeting_dir(&spool_root(db_path), &store, meeting);
    let mut statement = db.prepare(
        "SELECT id,source_idx,seq FROM pieces WHERE meeting_id=?1 AND state='unlink_pending'",
    )?;
    let rows = statement
        .query_map([meeting], |r| {
            Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, i64, i64)>>>()?;
    if rows.is_empty() {
        return Ok(0);
    }
    for (_, source, seq) in &rows {
        match std::fs::remove_file(dir.join(piece_name(*source, *seq))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    fault("result:unlinked")?;
    if dir.exists() {
        fsync_dir(&dir)?;
    }
    let tx = db.unchecked_transaction()?;
    for (id, ..) in &rows {
        tx.execute(
            "DELETE FROM pieces WHERE id=?1 AND state='unlink_pending'",
            [id],
        )?;
    }
    let done: bool = tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM pieces WHERE meeting_id=?1) AND EXISTS(SELECT 1 FROM captures WHERE meeting_id=?1 AND state!='capturing')",
        [meeting],
        |r| r.get(0),
    )?;
    tx.commit()?;
    if done {
        // Nothing left to protect; a quarantine folder keeps the directory.
        let _ = std::fs::remove_file(dir.join("capture.lock"));
        let _ = std::fs::remove_dir(&dir);
    }
    Ok(rows.len())
}

/// Removes audio folders of meetings that no longer exist in this store.
/// Folders from another store id are left alone and reported by status.
pub(crate) fn remove_deleted_audio(db_path: &Path, store: &str, db: &Connection) -> Result<usize> {
    let root = spool_root(db_path);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(0);
    };
    let mut removed = 0;
    let mut failed = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((owner, id)) = name.to_str().and_then(|n| n.split_once('-')) else {
            continue;
        };
        let Ok(id) = id.parse::<i64>() else { continue };
        if owner != store || format!("{store}-{id}") != name.to_string_lossy() {
            continue;
        }
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM meetings WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?;
        let meta = entry.path().symlink_metadata()?;
        if !exists && meta.is_dir() && !meta.file_type().is_symlink() {
            match std::fs::remove_dir_all(entry.path()) {
                Ok(()) => removed += 1,
                Err(e) => failed = Some(e),
            }
        }
    }
    if removed > 0 {
        fsync_dir(&root)?;
    }
    match failed {
        Some(e) => Err(anyhow!("could not remove deleted meeting audio: {e}")),
        None => Ok(removed),
    }
}

/// Window audio for one job, as normalized samples. Missing or corrupt pieces
/// are quarantined, recorded as gaps and filled with silence so that every
/// timestamp in the window stays where it was.
pub(crate) struct Window {
    pub samples: Vec<f32>,
    pub present: usize,
}
pub(crate) fn load_window(
    db: &Connection,
    db_path: &Path,
    meeting: i64,
    source: i64,
    from: i64,
    to: i64,
) -> Result<Window> {
    let store = store_id(db)?;
    let dir = meeting_dir(&spool_root(db_path), &store, meeting);
    let store_b = store_bytes(&store)?;
    let mut samples = vec![0f32; (to - from) as usize];
    let mut statement = db.prepare("SELECT id,seq,start_sample,samples,hash,state FROM pieces WHERE meeting_id=?1 AND source_idx=?2 AND start_sample<?4 AND start_sample+samples>?3 ORDER BY seq")?;
    let rows = statement
        .query_map(params![meeting, source, from, to], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut present = 0;
    for (id, seq, start, count, hash, state) in rows {
        let path = dir.join(piece_name(source, seq));
        let valid = if state == "missing" {
            None
        } else {
            match read_piece(&path) {
                Ok(Some((h, payload)))
                    if h.store == store_b
                        && h.meeting == meeting
                        && i64::from(h.source) == source
                        && h.seq as i64 == seq
                        && h.start as i64 == start
                        && i64::from(h.samples) == count
                        && format!("{:016x}", h.payload_hash) == hash =>
                {
                    Some(payload)
                }
                Ok(_) => {
                    quarantine(&dir, &path)?;
                    None
                }
                Err(e)
                    if e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    None
                }
                Err(e) => return Err(e),
            }
        };
        match valid {
            Some(payload) => {
                present += 1;
                for (i, pair) in payload.as_chunks::<2>().0.iter().enumerate() {
                    let at = start + i as i64;
                    if (from..to).contains(&at) {
                        samples[(at - from) as usize] =
                            f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0;
                    }
                }
            }
            None => {
                if state != "missing" {
                    mark_missing(db, meeting, source, id, start, count)?;
                }
            }
        }
    }
    Ok(Window { samples, present })
}
fn mark_missing(
    db: &Connection,
    meeting: i64,
    source: i64,
    piece: i64,
    start: i64,
    count: i64,
) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE pieces SET state='missing' WHERE id=?1 AND state='ready'",
        [piece],
    )?;
    if changed == 1 {
        let source_id: String = tx.query_row(
            "SELECT source_id FROM capture_sources WHERE meeting_id=?1 AND idx=?2",
            params![meeting, source],
            |r| r.get(0),
        )?;
        add_gap(
            &tx,
            meeting,
            &source_id,
            start * 1000 / RATE,
            (start + count) * 1000 / RATE,
            "audio piece missing or corrupt; kept in quarantine if present",
        )?;
    }
    tx.commit()?;
    Ok(())
}
fn quarantine(dir: &Path, path: &Path) -> Result<()> {
    let target = dir.join("quarantine");
    crate::paths::private_dir(&target)?;
    let name = path.file_name().context("piece has no name")?;
    let mut dest = target.join(name);
    let mut n = 1;
    while dest.symlink_metadata().is_ok() {
        dest = target.join(format!("{}.{n}", name.to_string_lossy()));
        n += 1;
    }
    std::fs::rename(path, &dest)?;
    fsync_dir(dir)?;
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Recovery {
    /// Renamed pieces whose commit never happened, validated and kept.
    pub imported: usize,
    pub quarantined: usize,
    /// Unfinished `.part` files: never acknowledged, recorded as gaps.
    pub torn: usize,
    pub missing: usize,
    pub interrupted: usize,
    /// Requests whose outcome was never recorded; they count as attempts.
    pub ambiguous: usize,
    pub unlinked: usize,
}

/// Local integrity pass. Callers hold the worker lock so no attempt is in
/// flight. It never opens a microphone and never contacts a provider.
pub fn recover(db_path: &Path) -> Result<Recovery> {
    let mut report = Recovery::default();
    let db = open_writer(db_path)?;
    let store = store_id(&db)?;
    let store_b = store_bytes(&store)?;
    let root = spool_root(db_path);
    report.ambiguous = db.execute(
        "UPDATE attempts SET state='ambiguous',error='the process ended before the reply was recorded' WHERE state='reserved'",
        [],
    )?;
    // A crash right after the last reservation leaves an open job with no
    // attempts left; it is failed (audio kept) instead of being stuck.
    db.execute(
        "UPDATE jobs SET state='failed',error='all attempts used; the last reply was never recorded' WHERE state IN ('pending','waiting') AND attempts>=budget",
        [],
    )?;
    // Older journals called unsent tiny tails abandoned; retain their durable audio.
    db.execute("UPDATE jobs SET state='retained' WHERE state='abandoned' AND error='under 100 ms; not sent'", [])?;
    let _ = remove_deleted_audio(db_path, &store, &db);
    let captures = {
        let mut statement =
            db.prepare("SELECT meeting_id,state,expected_ms FROM captures ORDER BY meeting_id")?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (meeting, state, expected) in captures {
        let dir = meeting_dir(&root, &store, meeting);
        let lock = File::open(dir.join("capture.lock"))
            .ok()
            .filter(|f| fs2::FileExt::try_lock_exclusive(f).is_ok());
        let writer_alive = dir.join("capture.lock").exists() && lock.is_none();
        if writer_alive {
            continue;
        }
        let capturing = state == "capturing";
        let mut orphans = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = entry.path();
                if let Some(base) = name.strip_suffix(".part") {
                    // Torn: the gap is its declared range when the header survived.
                    if let (Some((source, _)), Ok(mut f)) =
                        (parse_piece_name(base), open_nofollow(&path))
                    {
                        let mut head = [0u8; HEADER];
                        if f.read_exact(&mut head).is_ok() {
                            if let Some(h) = Header::decode(&head) {
                                if let Some(source_id) = source_id(&db, meeting, source)? {
                                    add_gap(
                                        &db,
                                        meeting,
                                        &source_id,
                                        h.start as i64 * 1000 / RATE,
                                        (h.start as i64 + i64::from(h.samples)) * 1000 / RATE,
                                        "audio piece torn by a crash; never acknowledged",
                                    )?;
                                }
                            }
                        }
                    }
                    std::fs::remove_file(&path)?;
                    report.torn += 1;
                } else if let Some((source, seq)) = parse_piece_name(&name) {
                    let row: Option<String> = db
                        .query_row(
                            "SELECT state FROM pieces WHERE meeting_id=?1 AND source_idx=?2 AND seq=?3",
                            params![meeting, source, seq],
                            |r| r.get(0),
                        )
                        .optional()?;
                    match row.as_deref() {
                        Some("unlink_pending") => {}
                        Some(_) => {}
                        None => orphans.push((source, seq, path)),
                    }
                } else if name.ends_with(".pcm") {
                    quarantine(&dir, &path)?;
                    report.quarantined += 1;
                }
            }
        }
        orphans.sort();
        for (source, seq, path) in orphans {
            let next: Option<(i64, i64, bool)> = db
                .query_row(
                    "SELECT next_seq,next_sample,finished FROM capture_sources WHERE meeting_id=?1 AND idx=?2",
                    params![meeting, source],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let piece = read_piece(&path)?;
            let valid = match (&piece, next) {
                (Some((h, _)), Some((next_seq, next_sample, false))) => {
                    h.store == store_b
                        && h.meeting == meeting
                        && i64::from(h.source) == source
                        && h.seq as i64 == seq
                        && seq == next_seq
                        && h.start as i64 == next_sample
                }
                _ => false,
            };
            if !valid {
                quarantine(&dir, &path)?;
                report.quarantined += 1;
                continue;
            }
            let (h, _) = piece.unwrap();
            let tx = db.unchecked_transaction()?;
            tx.execute("INSERT INTO pieces(meeting_id,source_idx,seq,start_sample,samples,bytes,hash,state,created_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,'ready',?8)",
                params![meeting, source, seq, h.start as i64, i64::from(h.samples), (HEADER + h.samples as usize * 2) as i64, format!("{:016x}", h.payload_hash), now_ms()])?;
            let end = h.start as i64 + i64::from(h.samples);
            tx.execute("UPDATE capture_sources SET next_seq=?3,next_sample=?4 WHERE meeting_id=?1 AND idx=?2",
                params![meeting, source, seq + 1, end])?;
            plan_jobs(&tx, meeting, source, end, false)?;
            tx.commit()?;
            report.imported += 1;
        }
        // Rows whose file vanished: the gap stays visible.
        let rows = {
            let mut statement = db.prepare(
                "SELECT id,source_idx,seq,start_sample,samples,state FROM pieces WHERE meeting_id=?1",
            )?;
            let rows = statement
                .query_map([meeting], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (id, source, seq, start, count, state) in rows {
            if state == "ready" && !dir.join(piece_name(source, seq)).exists() {
                mark_missing(&db, meeting, source, id, start, count)?;
                report.missing += 1;
            }
        }
        if capturing {
            let ends = {
                let mut statement = db.prepare(
                    "SELECT source_id,next_sample FROM capture_sources WHERE meeting_id=?1 ORDER BY idx",
                )?;
                let rows = statement
                    .query_map([meeting], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
                rows
            };
            let tx = db.unchecked_transaction()?;
            close_capture(
                &tx,
                meeting,
                "interrupted",
                Some(
                    "capture ended without finishing; audio after the last saved piece is missing",
                ),
                &ends,
                expected,
                now_ms(),
            )?;
            tx.commit()?;
            report.interrupted += 1;
        }
        let tx = db.unchecked_transaction()?;
        release_pieces(&tx, meeting)?;
        tx.commit()?;
        report.unlinked += unlink_released(&db, db_path, meeting)?;
        drop(lock);
    }
    Ok(report)
}
fn source_id(db: &Connection, meeting: i64, source: i64) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT source_id FROM capture_sources WHERE meeting_id=?1 AND idx=?2",
            params![meeting, source],
            |r| r.get(0),
        )
        .optional()?)
}

/// Counts for status without changing anything.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct SpoolScan {
    pub quarantined_files: usize,
    pub quarantined_bytes: u64,
    pub unrecovered_files: usize,
    /// Audio folders from a different meeting database.
    pub foreign_folders: usize,
}
pub(crate) fn scan(db_path: &Path, store: &str) -> SpoolScan {
    let mut scan = SpoolScan::default();
    let Ok(entries) = std::fs::read_dir(spool_root(db_path)) else {
        return scan;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() {
            continue;
        }
        if !name.starts_with(&format!("{store}-")) {
            scan.foreign_folders += 1;
            continue;
        }
        for file in std::fs::read_dir(entry.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            if file.file_name().to_string_lossy().ends_with(".part") {
                scan.unrecovered_files += 1;
            }
        }
        for file in std::fs::read_dir(entry.path().join("quarantine"))
            .into_iter()
            .flatten()
            .flatten()
        {
            scan.quarantined_files += 1;
            scan.quarantined_bytes += file.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips_and_rejects_any_flipped_byte() {
        let header = Header {
            store: [7; 16],
            meeting: 3,
            source: 1,
            seq: 9,
            start: 720_000,
            samples: 80_000,
            payload_hash: 42,
        };
        let bytes = header.encode();
        assert_eq!(Header::decode(&bytes), Some(header));
        for i in 0..HEADER {
            let mut bad = bytes;
            bad[i] ^= 1;
            assert_eq!(Header::decode(&bad), None, "byte {i}");
        }
        assert_eq!(parse_piece_name("s1-000009.pcm"), Some((1, 9)));
        assert_eq!(parse_piece_name("s1-9.pcm"), None);
        assert_eq!(parse_piece_name("../s1-000009.pcm"), None);
    }
}
