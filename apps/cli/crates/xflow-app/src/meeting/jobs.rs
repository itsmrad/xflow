//! Persistent STT job ledger. One row per bounded window; every request that
//! may be billed is reserved here before it is sent, so a crash or a broken
//! connection still counts against the three-attempt ceiling.
use super::journal::{self, Limits, RATE};
use super::store::{has_pipeline, open_reader, open_writer, store_id, valid_text};
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::path::Path;
use xflow_core::meeting::{MeetingId, Source};
use xflow_core::{TimedSpan, TimedTranscript, Timing};
use xflow_providers::SttFailure;

pub const ATTEMPTS: i64 = 3;
/// Segment sequence = window * stride + index; a window holds at most 4096
/// provider spans plus splits of long text.
const SEQUENCE_STRIDE: i64 = 10_000;
/// Rate-limit waits longer than this end the run; `retry` resumes later.
const MAX_RETRY_AFTER_MS: i64 = 3_600_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Job {
    pub id: i64,
    pub meeting: i64,
    pub source: i64,
    pub win: i64,
    pub overlap: i64,
    pub start: i64,
    pub end: i64,
}

pub(crate) fn next_job(db: &Connection, meeting: i64, now: i64) -> Result<Option<Job>> {
    Ok(db
        .query_row(
            "SELECT id,meeting_id,source_idx,win,overlap_sample,start_sample,end_sample FROM jobs WHERE meeting_id=?1 AND state IN ('pending','waiting') AND not_before_ms<=?2 AND attempts<budget ORDER BY start_sample,source_idx LIMIT 1",
            params![meeting, now],
            |r| {
                Ok(Job {
                    id: r.get(0)?,
                    meeting: r.get(1)?,
                    source: r.get(2)?,
                    win: r.get(3)?,
                    overlap: r.get(4)?,
                    start: r.get(5)?,
                    end: r.get(6)?,
                })
            },
        )
        .optional()?)
}
/// When the next waiting job may run, if any is waiting.
pub(crate) fn next_wait(db: &Connection, meeting: i64) -> Result<Option<i64>> {
    Ok(db.query_row(
        "SELECT MIN(not_before_ms) FROM jobs WHERE meeting_id=?1 AND state IN ('pending','waiting') AND attempts<budget",
        [meeting],
        |r| r.get(0),
    )?)
}

/// Raised by the dispatch hook; the request is never sent.
#[derive(Debug)]
pub struct Refused(pub &'static str);
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Refused {}

/// Durable reservation for one dispatch. Fails, sending nothing, when the job
/// was deleted, finished, or has used its attempts.
pub(crate) fn reserve(db: &mut Connection, job: i64, provider: &str, now: i64) -> Result<i64> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<(String, i64, i64)> = tx
        .query_row(
            "SELECT state,attempts,budget FROM jobs WHERE id=?1",
            [job],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match row {
        None => bail!(Refused("the meeting was deleted")),
        Some((state, ..)) if state != "pending" && state != "waiting" => {
            bail!(Refused("the job is no longer open"))
        }
        Some((_, attempts, budget)) if attempts >= budget => {
            bail!(Refused("the job used all of its attempts"))
        }
        Some(_) => {}
    }
    tx.execute(
        "INSERT INTO attempts(job_id,provider,state,started_ms) VALUES (?1,?2,'reserved',?3)",
        params![job, provider, now],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute("UPDATE jobs SET attempts=attempts+1 WHERE id=?1", [job])?;
    tx.commit()?;
    Ok(id)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Accepted {
    Text(usize),
    Empty,
    /// Deleted or already finished: the result is dropped, never stored.
    Gone,
}

/// One transaction: text, FTS (by trigger), revisions, provenance, job and
/// attempt state, and the pieces this result releases.
pub(crate) fn accept(
    db: &mut Connection,
    job: &Job,
    attempt: i64,
    result: &TimedTranscript,
    model: &str,
) -> Result<Accepted> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let open: Option<String> = tx
        .query_row("SELECT state FROM jobs WHERE id=?1", [job.id], |r| r.get(0))
        .optional()?;
    let reserved: Option<String> = tx
        .query_row(
            "SELECT state FROM attempts WHERE id=?1 AND job_id=?2",
            params![attempt, job.id],
            |r| r.get(0),
        )
        .optional()?;
    if !matches!(open.as_deref(), Some("pending" | "waiting"))
        || reserved.as_deref() != Some("reserved")
    {
        return Ok(Accepted::Gone);
    }
    let source: (String, String) = tx.query_row(
        "SELECT c.source_id,s.json FROM capture_sources c JOIN sources s ON s.meeting_id=c.meeting_id AND s.id=c.source_id WHERE c.meeting_id=?1 AND c.idx=?2",
        params![job.meeting, job.source],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let previous: Option<String> = tx
        .query_row(
            "SELECT group_concat(text,' ') FROM (SELECT s.text FROM segments s JOIN segment_provenance p ON p.segment_id=s.id JOIN jobs j ON j.id=p.job_id WHERE j.meeting_id=?1 AND j.source_idx=?2 AND j.win=?3 ORDER BY s.sequence)",
            params![job.meeting, job.source, job.win - 1],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let previous_accepted: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE meeting_id=?1 AND source_idx=?2 AND win=?3 AND state='accepted')",
        params![job.meeting, job.source, job.win - 1],
        |r| r.get(0),
    )?;
    let pieces = segments(job, result, previous.as_deref(), previous_accepted);
    let language = result.transcript.language.as_deref();
    for (i, piece) in pieces.iter().enumerate() {
        tx.execute("INSERT INTO segments(meeting_id,source_id,sequence,start_ms,end_ms,text) VALUES (?1,?2,?3,?4,?5,?6)",
            params![job.meeting, source.0, job.win * SEQUENCE_STRIDE + i as i64, piece.start_ms, piece.end_ms, piece.text])?;
        let segment = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO revisions(segment_id,revision,text) VALUES (?1,1,?2)",
            params![segment, piece.text],
        )?;
        tx.execute("INSERT INTO segment_provenance(segment_id,job_id,attempt_id,timing,dedup,trimmed,language) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![segment, job.id, attempt, piece.timing, piece.dedup, piece.trimmed, language])?;
    }
    tx.execute(
        "UPDATE jobs SET state='accepted',error=NULL WHERE id=?1",
        [job.id],
    )?;
    tx.execute(
        "UPDATE attempts SET state='succeeded' WHERE id=?1",
        [attempt],
    )?;
    // An earlier reservation in the same call failed and was retried by the provider.
    tx.execute(
        "UPDATE attempts SET state='failed',error='retried by the provider' WHERE job_id=?1 AND state='reserved'",
        [job.id],
    )?;
    let mut info: Source = serde_json::from_str(&source.1)?;
    if info.provider.is_none() {
        info.provider = result.transcript.provider.clone();
        info.model = (!model.is_empty()).then(|| model.to_owned());
        tx.execute(
            "UPDATE sources SET json=?3 WHERE meeting_id=?1 AND id=?2",
            params![job.meeting, source.0, serde_json::to_string(&info)?],
        )?;
    }
    journal::release_pieces(&tx, job.meeting)?;
    tx.commit()?;
    super::fault("result:committed")?;
    Ok(if pieces.is_empty() {
        Accepted::Empty
    } else {
        Accepted::Text(pieces.len())
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Failed {
    /// May run again at this time, within the same run if it is soon.
    Retry(i64),
    Final,
    /// Nothing was sent: the provider could not be reached.
    Unavailable,
    Gone,
}

pub(crate) fn fail(
    db: &mut Connection,
    job: &Job,
    attempt: Option<i64>,
    failure: SttFailure,
    message: &str,
    now: i64,
) -> Result<Failed> {
    let message: String = message.chars().take(300).collect();
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<(i64, i64)> = tx
        .query_row(
            "SELECT attempts,budget FROM jobs WHERE id=?1 AND state IN ('pending','waiting')",
            [job.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((mut attempts, budget)) = row else {
        return Ok(Failed::Gone);
    };
    if let Some(attempt) = attempt {
        let state = match failure {
            SttFailure::NotSent => "not_sent",
            SttFailure::Ambiguous => "ambiguous",
            _ => "failed",
        };
        tx.execute(
            "UPDATE attempts SET state=?2,error=?3 WHERE id=?1 AND state='reserved'",
            params![attempt, state, message],
        )?;
        if failure == SttFailure::NotSent {
            // A refused connection carried no audio and cannot be billed.
            tx.execute(
                "UPDATE jobs SET attempts=attempts-1 WHERE id=?1 AND attempts>0",
                [job.id],
            )?;
            attempts -= 1;
        }
        tx.execute(
            "UPDATE attempts SET state='failed',error='retried by the provider' WHERE job_id=?1 AND state='reserved'",
            [job.id],
        )?;
    }
    let outcome = match failure {
        _ if attempt.is_none() => Failed::Final,
        SttFailure::NotSent => Failed::Unavailable,
        SttFailure::Rejected(_) => Failed::Final,
        _ if attempts >= budget => Failed::Final,
        SttFailure::RateLimited(after) => Failed::Retry(
            now + after
                .map(|d| d.as_millis() as i64)
                .unwrap_or(30_000)
                .clamp(1_000, MAX_RETRY_AFTER_MS),
        ),
        _ => Failed::Retry(now + 2_000 * (1 << attempts.clamp(0, 4))),
    };
    let (state, at) = match outcome {
        Failed::Retry(at) => ("waiting", at),
        Failed::Final => ("failed", 0),
        _ => ("pending", 0),
    };
    tx.execute(
        "UPDATE jobs SET state=?2,not_before_ms=?3,error=?4 WHERE id=?1",
        params![job.id, state, at, message],
    )?;
    tx.commit()?;
    Ok(outcome)
}

/// Explicit user retry: failed and waiting jobs get a fresh three-attempt
/// budget. Attempt history is kept.
pub fn reopen(db_path: &Path, meeting: Option<i64>) -> Result<usize> {
    let db = open_writer(db_path)?;
    Ok(db.execute(
        "UPDATE jobs SET state='pending',attempts=0,not_before_ms=0 WHERE state IN ('failed','waiting','pending') AND (?1 IS NULL OR meeting_id=?1)",
        [meeting],
    )?)
}
/// Queues meetings for the background worker: an explicit user request.
pub fn queue(db_path: &Path, meeting: Option<i64>) -> Result<usize> {
    let db = open_writer(db_path)?;
    Ok(db.execute(
        "UPDATE captures SET queued=1 WHERE (?1 IS NULL OR meeting_id=?1) AND EXISTS(SELECT 1 FROM jobs j WHERE j.meeting_id=captures.meeting_id AND j.state IN ('pending','waiting'))",
        [meeting],
    )?)
}
/// Queued meetings the worker can make progress on.
pub fn queued_meetings(db: &Connection) -> Result<Vec<i64>> {
    let mut statement = db.prepare("SELECT c.meeting_id FROM captures c WHERE c.queued=1 AND EXISTS(SELECT 1 FROM jobs j WHERE j.meeting_id=c.meeting_id AND j.state IN ('pending','waiting') AND j.attempts<j.budget) ORDER BY c.meeting_id")?;
    let rows = statement
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(rows)
}
/// Meetings with open jobs, oldest first.
pub fn open_meetings(db_path: &Path, meeting: Option<i64>) -> Result<Vec<i64>> {
    let db = open_writer(db_path)?;
    let mut statement = db.prepare("SELECT DISTINCT meeting_id FROM jobs WHERE state IN ('pending','waiting') AND (?1 IS NULL OR meeting_id=?1) ORDER BY meeting_id")?;
    let rows = statement
        .query_map([meeting], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(rows)
}

#[derive(Clone)]
struct Piece {
    start_ms: i64,
    end_ms: i64,
    text: String,
    timing: &'static str,
    dedup: &'static str,
    trimmed: Option<String>,
}

/// Window result as stored segments. Times come from the provider when it
/// reported them, otherwise the segment spans the whole window.
fn segments(
    job: &Job,
    result: &TimedTranscript,
    previous: Option<&str>,
    previous_accepted: bool,
) -> Vec<Piece> {
    let ws = job.overlap * 1000 / RATE;
    let we = job.end * 1000 / RATE;
    let overlap_ms = ((job.start - job.overlap) * 1000 / RATE) as u32;
    let duration = (we - ws) as u32;
    let fits = |spans: &[TimedSpan]| spans.iter().all(|s| s.end_ms <= duration + 500);
    let window = |text: &str, timing| {
        vec![Piece {
            start_ms: ws,
            end_ms: we.max(ws + 1),
            text: text.trim().to_owned(),
            timing,
            dedup: "none",
            trimmed: None,
        }]
    };
    let at = |s: &TimedSpan, text: String, timing| Piece {
        start_ms: ws + i64::from(s.start_ms.min(duration)),
        end_ms: (ws + i64::from(s.end_ms.min(duration)))
            .max(ws + i64::from(s.start_ms.min(duration)) + 1),
        text,
        timing,
        dedup: "none",
        trimmed: None,
    };
    let text = result.transcript.text.as_str();
    let (mut pieces, max_tokens) = match &result.timing {
        Timing::Segments(spans) if fits(spans) => (
            spans
                .iter()
                .filter(|s| !s.text.trim().is_empty())
                .map(|s| at(s, s.text.trim().to_owned(), "provider_segment"))
                .collect::<Vec<_>>(),
            overlap_token_budget(spans, overlap_ms),
        ),
        Timing::Words(words) if fits(words) => (
            group_words(words)
                .into_iter()
                .map(|(first, last, text)| {
                    let span = TimedSpan {
                        start_ms: first.start_ms,
                        end_ms: last.end_ms,
                        text: String::new(),
                    };
                    at(&span, text, "provider_word")
                })
                .collect(),
            overlap_token_budget(words, overlap_ms),
        ),
        Timing::TextOnly => (window(text, "window"), 12),
        // Out-of-range provider times are not trusted for placement.
        _ => (window(text, "window_timing_rejected"), 12),
    };
    pieces.retain(|p| !p.text.is_empty());
    if pieces.is_empty() && !text.trim().is_empty() {
        pieces = window(text, "window");
    }
    if let Some(first) = pieces.first_mut() {
        if job.win > 0 {
            match (previous, previous_accepted) {
                (_, false) => first.dedup = "unchecked",
                (None, true) => {}
                (Some(previous), true) => {
                    dedup(first, previous, max_tokens);
                    if matches!(&result.timing, Timing::Segments(spans) if spans.iter().any(|s| s.start_ms < overlap_ms && s.end_ms > overlap_ms))
                        && first.dedup == "none"
                    {
                        first.dedup = "uncertain";
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for piece in pieces {
        for (i, chunk) in split_text(&piece.text).into_iter().enumerate() {
            out.push(Piece {
                text: chunk,
                trimmed: if i == 0 { piece.trimmed.clone() } else { None },
                dedup: if i == 0 { piece.dedup } else { "none" },
                ..piece.clone()
            });
        }
    }
    out
}

/// Tokens the provider placed inside the 2 s overlap. Text after the window
/// boundary is never trimmed.
fn overlap_token_budget(spans: &[TimedSpan], overlap_ms: u32) -> usize {
    spans
        .iter()
        .take_while(|s| s.end_ms <= overlap_ms)
        .map(|s| tokens(&s.text).len())
        .sum::<usize>()
        .min(12)
}

fn group_words(words: &[TimedSpan]) -> Vec<(&TimedSpan, &TimedSpan, String)> {
    let mut groups = Vec::new();
    let mut start = 0;
    for i in 0..words.len() {
        let last = i + 1 == words.len();
        let ends = words[i]
            .text
            .chars()
            .last()
            .is_some_and(|c| ".?!。？！।".contains(c));
        let pause = !last && words[i + 1].start_ms.saturating_sub(words[i].end_ms) > 1_500;
        let long = words[i].end_ms.saturating_sub(words[start].start_ms) > 15_000;
        if last || ends || pause || long {
            let mut text = String::new();
            for w in &words[start..=i] {
                let w = w.text.trim();
                if w.is_empty() {
                    continue;
                }
                let joined =
                    text.chars().last().is_some_and(is_cjk) && w.chars().next().is_some_and(is_cjk);
                if !text.is_empty() && !joined {
                    text.push(' ');
                }
                text.push_str(w);
            }
            groups.push((&words[start], &words[i], text));
            start = i + 1;
        }
    }
    groups
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF66..=0xFF9F)
}
/// Normalized tokens with the byte offset where each ends. Words split on
/// whitespace with edge punctuation removed; each CJK character is a token.
fn tokens(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (offset, word) in text
        .split_whitespace()
        .map(|w| (w.as_ptr() as usize - text.as_ptr() as usize, w))
    {
        if word.chars().any(is_cjk) {
            for (i, c) in word.char_indices() {
                if c.is_alphanumeric() {
                    out.push((c.to_lowercase().collect(), offset + i + c.len_utf8()));
                }
            }
            continue;
        }
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric());
        if trimmed.is_empty() {
            continue;
        }
        let end = offset + (trimmed.as_ptr() as usize - word.as_ptr() as usize) + trimmed.len();
        out.push((trimmed.to_lowercase(), end));
    }
    out
}

/// Removes the start of `piece` that repeats the end of the previous window,
/// only when the match is long enough to be the shared overlap. Shorter or
/// whole-segment matches are kept and marked uncertain.
fn dedup(piece: &mut Piece, previous: &str, max_tokens: usize) {
    let prev = tokens(previous);
    let next = tokens(&piece.text);
    let limit = max_tokens.min(prev.len()).min(next.len());
    let k = (1..=limit)
        .rev()
        .find(|&k| {
            prev[prev.len() - k..]
                .iter()
                .zip(&next[..k])
                .all(|(a, b)| a.0 == b.0)
        })
        .unwrap_or(0);
    if k == 0 {
        return;
    }
    let chars: usize = next[..k].iter().map(|t| t.0.chars().count()).sum();
    let cut = next[k - 1].1;
    let rest = piece.text[cut..]
        .trim_start_matches(|c: char| c.is_whitespace() || (!c.is_alphanumeric() && !is_cjk(c)));
    if k >= 2 && chars >= 4 && !rest.is_empty() {
        piece.trimmed = Some(piece.text[..cut].to_owned());
        piece.text = rest.to_owned();
        piece.dedup = "trimmed";
    } else {
        piece.dedup = "uncertain";
    }
}

/// Splits text so every stored segment passes the store's text bound. Never truncates.
fn split_text(text: &str) -> Vec<String> {
    if valid_text(text).is_ok() {
        return vec![text.to_owned()];
    }
    let mut mid = text.len() / 2;
    while !text.is_char_boundary(mid) {
        mid -= 1;
    }
    if let Some(space) = text[..mid].rfind(char::is_whitespace) {
        if space > text.len() / 4 {
            mid = space;
        }
    }
    if mid == 0 {
        return vec![text.to_owned()];
    }
    let (a, b) = text.split_at(mid);
    let mut out = split_text(a.trim_end());
    out.extend(split_text(b.trim_start()));
    out.retain(|t| !t.is_empty());
    out
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Windows {
    pub total: i64,
    pub accepted: i64,
    pub empty: i64,
    pub pending: i64,
    pub waiting: i64,
    pub failed: i64,
    pub abandoned: i64,
    pub retained: i64,
}
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct PendingAudio {
    pub pieces: i64,
    pub bytes: i64,
    pub seconds: i64,
    /// Since the capture ended, or since it started if it never ended.
    pub age_ms: Option<i64>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MeetingPipeline {
    pub meeting: MeetingId,
    pub title: Option<String>,
    pub capture: String,
    pub reason: Option<String>,
    pub started_at_ms: i64,
    pub windows: Windows,
    pub attempts: i64,
    pub ambiguous_attempts: i64,
    pub pending_audio: PendingAudio,
    pub gaps: i64,
    pub dedup_trimmed: i64,
    pub dedup_uncertain: i64,
    pub next_retry_ms: Option<i64>,
    pub last_error: Option<String>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct WorkerStatus {
    pub running: bool,
    pub state: Option<String>,
    pub error: Option<String>,
    pub updated_ms: Option<i64>,
    /// Queued meetings that still have work; a stopped worker should restart.
    pub queued: usize,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PipelineStatus {
    pub worker: WorkerStatus,
    pub meetings: Vec<MeetingPipeline>,
    pub audio_bytes: u64,
    pub limit_bytes: u64,
    pub spool: journal::SpoolScan,
    pub warnings: Vec<String>,
}

pub const WARN_AFTER_MS: i64 = 24 * 3_600_000;

/// Read-only. `now` is explicit so the 24-hour warning is testable.
pub fn status(
    db_path: &Path,
    meeting: Option<i64>,
    limits: Limits,
    now: i64,
) -> Result<PipelineStatus> {
    let db = open_reader(db_path)?;
    let store = store_id(&db)?;
    let mut status = PipelineStatus {
        worker: WorkerStatus {
            running: super::transcribe::worker_running(db_path),
            ..Default::default()
        },
        meetings: Vec::new(),
        audio_bytes: 0,
        limit_bytes: limits.global_bytes,
        spool: journal::scan(db_path, &store),
        warnings: Vec::new(),
    };
    if !has_pipeline(&db)? {
        return Ok(status);
    }
    if let Some((state, error, updated)) = db
        .query_row("SELECT state,error,updated_ms FROM worker", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?
    {
        status.worker.state = Some(state);
        status.worker.error = error;
        status.worker.updated_ms = Some(updated);
    }
    status.worker.queued = queued_meetings(&db)?.len();
    if let Some(error) = &status.worker.error {
        status
            .warnings
            .push(format!("background transcription: {error}"));
    }
    status.audio_bytes = journal::usage(&db, &journal::spool_root(db_path), None)?;
    if status.audio_bytes >= limits.global_bytes / 5 * 4 {
        status.warnings.push(format!(
            "meeting audio uses {} of {} MiB (over 80%); new capture is refused at the limit and nothing is deleted",
            status.audio_bytes >> 20,
            limits.global_bytes >> 20
        ));
    }
    if status.spool.quarantined_files > 0 {
        status.warnings.push(format!(
            "{} damaged audio file(s) kept in quarantine",
            status.spool.quarantined_files
        ));
    }
    let ids = {
        let mut statement = db.prepare("SELECT c.meeting_id FROM captures c WHERE ?1 IS NULL OR c.meeting_id=?1 ORDER BY EXISTS(SELECT 1 FROM pieces p WHERE p.meeting_id=c.meeting_id) DESC, c.meeting_id DESC LIMIT 50")?;
        let rows = statement
            .query_map([meeting], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        rows
    };
    for id in ids {
        let (title, started, capture, reason, created, ended): (
            Option<String>,
            i64,
            String,
            Option<String>,
            i64,
            Option<i64>,
        ) = db.query_row(
            "SELECT m.title,m.started_at_ms,c.state,c.reason,c.created_ms,c.ended_ms FROM meetings m JOIN captures c ON c.meeting_id=m.id WHERE m.id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
        let count = |sql: &str| -> rusqlite::Result<i64> { db.query_row(sql, [id], |r| r.get(0)) };
        let windows = Windows {
            total: count("SELECT count(*) FROM jobs WHERE meeting_id=?1")?,
            accepted: count("SELECT count(*) FROM jobs WHERE meeting_id=?1 AND state='accepted'")?,
            empty: count("SELECT count(*) FROM jobs j WHERE meeting_id=?1 AND state='accepted' AND NOT EXISTS(SELECT 1 FROM segment_provenance p WHERE p.job_id=j.id)")?,
            pending: count("SELECT count(*) FROM jobs WHERE meeting_id=?1 AND state='pending'")?,
            waiting: count("SELECT count(*) FROM jobs WHERE meeting_id=?1 AND state='waiting'")?,
            failed: count("SELECT count(*) FROM jobs WHERE meeting_id=?1 AND state='failed'")?,
            retained: count("SELECT count(*) FROM jobs WHERE meeting_id=?1 AND state='retained'")?,
            abandoned: count("SELECT count(*) FROM jobs WHERE meeting_id=?1 AND state='abandoned'")?,
        };
        let (pieces, bytes, samples): (i64, i64, i64) = db.query_row(
            "SELECT count(*),COALESCE(SUM(bytes),0),COALESCE(SUM(samples),0) FROM pieces WHERE meeting_id=?1 AND state IN ('ready','unlink_pending')",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let age = (pieces > 0).then(|| now - ended.unwrap_or(created));
        let mut warnings = Vec::new();
        if age.is_some_and(|a| a >= WARN_AFTER_MS) {
            warnings.push(format!(
                "audio has waited over 24 hours for transcription; run `xflow meeting retry {store}:{id}` or delete the meeting. It is never deleted automatically."
            ));
        }
        if capture == "interrupted" || capture == "stopped" {
            warnings.push(format!(
                "capture {capture}: {}",
                reason.clone().unwrap_or_default()
            ));
        }
        status.meetings.push(MeetingPipeline {
            meeting: MeetingId {
                store_id: store.clone(),
                id,
            },
            title,
            capture,
            reason,
            started_at_ms: started,
            windows,
            attempts: count("SELECT count(*) FROM attempts a JOIN jobs j ON j.id=a.job_id WHERE j.meeting_id=?1 AND a.state!='not_sent'")?,
            ambiguous_attempts: count("SELECT count(*) FROM attempts a JOIN jobs j ON j.id=a.job_id WHERE j.meeting_id=?1 AND a.state='ambiguous'")?,
            pending_audio: PendingAudio {
                pieces,
                bytes,
                seconds: samples / RATE,
                age_ms: age,
            },
            gaps: count("SELECT count(*) FROM gaps WHERE meeting_id=?1")?,
            dedup_trimmed: count("SELECT count(*) FROM segment_provenance p JOIN jobs j ON j.id=p.job_id WHERE j.meeting_id=?1 AND p.dedup='trimmed'")?,
            dedup_uncertain: count("SELECT count(*) FROM segment_provenance p JOIN jobs j ON j.id=p.job_id WHERE j.meeting_id=?1 AND p.dedup IN ('uncertain','unchecked')")?,
            next_retry_ms: db.query_row(
                "SELECT MIN(not_before_ms) FROM jobs WHERE meeting_id=?1 AND state='waiting'",
                [id],
                |r| r.get(0),
            )?,
            last_error: db
                .query_row(
                    "SELECT error FROM jobs WHERE meeting_id=?1 AND state IN ('failed','waiting','pending') AND error IS NOT NULL ORDER BY id DESC LIMIT 1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?,
            warnings,
        });
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xflow_core::Transcript;

    fn job(win: i64) -> Job {
        let start = win * journal::WINDOW_SAMPLES;
        Job {
            id: 1,
            meeting: 1,
            source: 0,
            win,
            overlap: (start - journal::OVERLAP_SAMPLES).max(0),
            start,
            end: start + journal::WINDOW_SAMPLES,
        }
    }
    fn timed(text: &str, timing: Timing) -> TimedTranscript {
        TimedTranscript {
            transcript: Transcript {
                provider: Some("stub".into()),
                text: text.into(),
                language: None,
            },
            timing,
        }
    }
    fn span(start_ms: u32, end_ms: u32, text: &str) -> TimedSpan {
        TimedSpan {
            start_ms,
            end_ms,
            text: text.into(),
        }
    }

    #[test]
    fn crossing_segment_keeps_words_and_marks_uncertainty() {
        let out = segments(
            &job(1),
            &timed(
                "on Friday we ship",
                Timing::Segments(vec![span(1900, 6000, "on Friday we ship")]),
            ),
            Some("we meet on Friday"),
            true,
        );
        assert_eq!(out[0].text, "on Friday we ship");
        assert_eq!(out[0].dedup, "uncertain");
        assert_eq!(out[0].trimmed, None);
    }

    #[test]
    fn overlap_is_trimmed_but_genuine_repetition_stays() {
        let prev = "We will deploy to production on Friday";
        let out = segments(
            &job(1),
            &timed(
                "on Friday. Then we deploy to production on Friday again",
                Timing::TextOnly,
            ),
            Some(prev),
            true,
        );
        assert_eq!(out[0].text, "Then we deploy to production on Friday again");
        assert_eq!(out[0].dedup, "trimmed");
        assert_eq!(out[0].trimmed.as_deref(), Some("on Friday"));
        // One shared word could be real speech: kept and marked.
        let out = segments(
            &job(1),
            &timed("Friday works for me", Timing::TextOnly),
            Some(prev),
            true,
        );
        assert_eq!(out[0].text, "Friday works for me");
        assert_eq!(out[0].dedup, "uncertain");
        // "no no no" repeated by the speaker is never reduced to nothing.
        let out = segments(
            &job(1),
            &timed("no no", Timing::TextOnly),
            Some("I said no no"),
            true,
        );
        assert_eq!(out[0].text, "no no");
        assert_eq!(out[0].dedup, "uncertain");
        // Previous window not accepted: overlap kept, marked unchecked.
        let out = segments(
            &job(1),
            &timed("on Friday. Then", Timing::TextOnly),
            None,
            false,
        );
        assert_eq!(out[0].dedup, "unchecked");
        assert_eq!(out[0].text, "on Friday. Then");
    }

    #[test]
    fn native_timings_bound_the_trim_to_the_overlap() {
        let prev = "we ship on Friday";
        // Provider says the repeated words start after the 2 s overlap: genuine speech.
        let late = Timing::Segments(vec![span(2_500, 6_000, "on Friday we ship")]);
        let out = segments(&job(1), &timed("on Friday we ship", late), Some(prev), true);
        assert_eq!(out[0].text, "on Friday we ship");
        assert_eq!(out[0].dedup, "none");
        assert_eq!(out[0].timing, "provider_segment");
        assert_eq!(out[0].start_ms, 28_000 + 2_500);
        let early = Timing::Segments(vec![
            span(100, 1_900, "on Friday"),
            span(2_000, 5_000, "we ship"),
        ]);
        let out = segments(
            &job(1),
            &timed("on Friday we ship", early),
            Some(prev),
            true,
        );
        assert_eq!(
            out[0].dedup, "uncertain",
            "a whole segment is never removed"
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn cjk_and_devanagari_tokens_and_timing_shapes() {
        let prev = "明日の会議は東京で行います";
        let out = segments(
            &job(1),
            &timed("東京で行います。資料を共有します", Timing::TextOnly),
            Some(prev),
            true,
        );
        assert_eq!(out[0].text, "資料を共有します");
        assert_eq!(out[0].dedup, "trimmed");
        let prev = "हम कल मीटिंग करेंगे";
        let out = segments(
            &job(1),
            &timed("मीटिंग करेंगे और फिर लंच", Timing::TextOnly),
            Some(prev),
            true,
        );
        assert_eq!(out[0].text, "और फिर लंच");
        let words = Timing::Words(vec![
            span(0, 400, "Hola,"),
            span(450, 900, "José."),
            span(3_000, 3_500, "Adiós"),
        ]);
        let out = segments(&job(0), &timed("Hola, José. Adiós", words), None, false);
        assert_eq!(
            out.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(),
            ["Hola, José.", "Adiós"]
        );
        assert_eq!(
            (out[0].start_ms, out[0].end_ms, out[1].start_ms),
            (0, 900, 3_000)
        );
        assert_eq!(out[0].timing, "provider_word");
        // A time past the window is not trusted: one window-wide segment.
        let bad = Timing::Segments(vec![span(0, 99_000, "too long")]);
        let out = segments(&job(0), &timed("too long", bad), None, false);
        assert_eq!(
            (out[0].timing, out[0].start_ms, out[0].end_ms),
            ("window_timing_rejected", 0, 30_000)
        );
        assert!(segments(&job(0), &timed("", Timing::TextOnly), None, false).is_empty());
    }

    #[test]
    fn long_text_is_split_not_truncated() {
        let text = "kubectl rollout restart deployment ".repeat(400);
        let parts = split_text(text.trim());
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|p| valid_text(p).is_ok()));
        assert_eq!(parts.join(" "), text.trim());
    }
}
