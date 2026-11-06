//! The meeting worker. It runs only inside an explicit user command, for the
//! meetings that command names, with at most one active job across every
//! xflow process (a file lock). Dictation goes first: no meeting request is
//! sent while the daemon is listening or processing.
use super::jobs::{self, Accepted, Failed, Job};
use super::{fault, journal, store::open_writer};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::{fs::File, future::Future, path::Path, sync::Mutex, time::Duration};
use xflow_core::{AudioClip, SpeechToText, TranscriptionOptions};
use xflow_providers::{classify_stt_failure, SttFailure};

/// Waits up to this long inside a run for a rate limit or backoff to pass.
const INLINE_WAIT_MS: i64 = 120_000;
/// Gives dictation up to this long before sending the next meeting request.
const DICTATION_WAIT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Report {
    pub accepted: usize,
    pub empty: usize,
    pub failed: usize,
    /// Still waiting for a rate limit or backoff when the run ended.
    pub waiting: usize,
    /// Results dropped because the meeting was deleted meanwhile.
    pub dropped: usize,
    pub unavailable: Option<String>,
    pub cancelled: bool,
    pub next_retry_ms: Option<i64>,
}

pub struct WorkerLock(#[allow(dead_code)] File);
/// `None` when another process holds it and `wait` is false.
pub fn lock_worker(db_path: &Path, wait: bool) -> Result<Option<WorkerLock>> {
    let root = journal::spool_root(db_path);
    crate::paths::private_dir(&root)?;
    let file = journal::lock_file(&root.join("worker.lock"))?;
    if wait {
        fs2::FileExt::lock_exclusive(&file).context("could not lock the meeting worker")?;
    } else if fs2::FileExt::try_lock_exclusive(&file).is_err() {
        return Ok(None);
    }
    Ok(Some(WorkerLock(file)))
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
fn lock(db: &Mutex<Connection>) -> std::sync::MutexGuard<'_, Connection> {
    db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Processes every open job of `meetings`. Hold a `WorkerLock` and run
/// `journal::recover` first. `busy` reports whether dictation is active.
pub async fn run<B, F>(
    db_path: &Path,
    meetings: &[i64],
    stt: &dyn SpeechToText,
    busy: B,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    progress: &mut (dyn FnMut(&str) + Send),
) -> Result<Report>
where
    B: Fn() -> F,
    F: Future<Output = bool>,
{
    let db = Mutex::new(open_writer(db_path)?);
    let mut report = Report::default();
    for &meeting in meetings {
        loop {
            if *cancel.borrow() {
                report.cancelled = true;
                return Ok(report);
            }
            let now = now_ms();
            let next = jobs::next_job(&lock(&db), meeting, now)?;
            let Some(job) = next else {
                let wait = jobs::next_wait(&lock(&db), meeting)?;
                match wait {
                    Some(at) if at - now <= INLINE_WAIT_MS => {
                        let pause = Duration::from_millis((at - now).max(0) as u64 + 50);
                        tokio::select! {
                            _ = tokio::time::sleep(pause) => continue,
                            _ = cancel.changed() => { report.cancelled = true; return Ok(report); }
                        }
                    }
                    Some(at) => {
                        report.waiting += waiting(&lock(&db), meeting)?;
                        report.next_retry_ms =
                            Some(report.next_retry_ms.map_or(at, |n: i64| n.min(at)));
                    }
                    None => {}
                }
                break;
            };
            let window = {
                let db = lock(&db);
                journal::load_window(&db, db_path, meeting, job.source, job.overlap, job.end)?
            };
            if window.present == 0 {
                // Every piece is gone (gaps recorded); there is nothing to send.
                abandon(&lock(&db), &job)?;
                journal::unlink_released(&lock(&db), db_path, meeting)?;
                continue;
            }
            let mut waited = Duration::ZERO;
            while waited < DICTATION_WAIT && busy().await {
                tokio::time::sleep(Duration::from_millis(250)).await;
                waited += Duration::from_millis(250);
            }
            let attempt = Mutex::new(None);
            let hook = |provider: &str| -> Result<()> {
                let id = jobs::reserve(&mut lock(&db), job.id, provider, now_ms())?;
                *attempt.lock().unwrap() = Some(id);
                fault("result:reserved")?;
                Ok(())
            };
            let clip = AudioClip {
                samples: window.samples,
                sample_rate: journal::RATE as u32,
                channels: 1,
            };
            let result = tokio::select! {
                result = stt.transcribe_timed(clip, TranscriptionOptions::default(), &hook) => Some(result),
                _ = cancel.changed() => None,
            };
            let attempt = *attempt.lock().unwrap();
            let Some(result) = result else {
                // The request may have reached the provider: it counts.
                if attempt.is_some() {
                    jobs::fail(
                        &mut lock(&db),
                        &job,
                        attempt,
                        SttFailure::Ambiguous,
                        "cancelled while the request was in flight",
                        now_ms(),
                    )?;
                }
                report.cancelled = true;
                return Ok(report);
            };
            match (result, attempt) {
                (Ok(result), Some(attempt)) => {
                    fault("result:received")?;
                    let outcome =
                        jobs::accept(&mut lock(&db), &job, attempt, &result, stt.model())?;
                    match outcome {
                        Accepted::Text(_) => report.accepted += 1,
                        Accepted::Empty => {
                            report.accepted += 1;
                            report.empty += 1;
                        }
                        Accepted::Gone => report.dropped += 1,
                    }
                    journal::unlink_released(&lock(&db), db_path, meeting)?;
                    progress(&format!(
                        "window {} of source {}: {}",
                        job.win + 1,
                        job.source + 1,
                        match outcome {
                            Accepted::Text(n) => format!("{n} segment(s)"),
                            Accepted::Empty => "no speech".into(),
                            Accepted::Gone => "meeting deleted; result dropped".into(),
                        }
                    ));
                }
                (Ok(_), None) => anyhow::bail!("provider returned without a recorded dispatch"),
                (Err(error), attempt) => {
                    let failure = classify_stt_failure(&error);
                    let message = format!("{error:#}");
                    let outcome =
                        jobs::fail(&mut lock(&db), &job, attempt, failure, &message, now_ms())?;
                    progress(&format!(
                        "window {} of source {}: {message}",
                        job.win + 1,
                        job.source + 1
                    ));
                    match outcome {
                        Failed::Final => report.failed += 1,
                        Failed::Gone => report.dropped += 1,
                        Failed::Retry(_) => {}
                        Failed::Unavailable => {
                            report.unavailable = Some(message);
                            return Ok(report);
                        }
                    }
                }
            }
        }
    }
    Ok(report)
}

fn waiting(db: &Connection, meeting: i64) -> Result<usize> {
    Ok(db.query_row(
        "SELECT count(*) FROM jobs WHERE meeting_id=?1 AND state='waiting'",
        [meeting],
        |r| r.get::<_, i64>(0),
    )? as usize)
}
fn abandon(db: &Connection, job: &Job) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    tx.execute(
        "UPDATE jobs SET state='abandoned',error='all audio for this window is missing' WHERE id=?1 AND state IN ('pending','waiting')",
        [job.id],
    )?;
    journal::release_pieces(&tx, job.meeting)?;
    tx.commit()?;
    Ok(())
}
