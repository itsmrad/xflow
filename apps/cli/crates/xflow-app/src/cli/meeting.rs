use super::{args, ipc, output::Output, Error, Result};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use xflow_app::{
    meeting::{
        self, jobs,
        journal::{self, Capture, CaptureSpec, Limits},
        transcribe, ContractError, ExportFormat, MeetingReader, MeetingStore,
    },
    paths,
};
use xflow_core::meeting::{
    DeleteStatus, ListCursor, ListFilter, MeetingId, Provenance, SearchCursor, MAX_IMPORT_BYTES,
};

pub fn run(
    action: &args::MeetingCmd,
    config: impl FnOnce() -> Result<PathBuf>,
    out: &Output,
) -> Result<()> {
    match action {
        args::MeetingCmd::Process { files, title, wait } => {
            process(files, title.as_deref(), &config()?, *wait, out)
        }
        args::MeetingCmd::Status { id } => status(id.as_deref(), &config()?, out),
        args::MeetingCmd::Retry { id, wait } => {
            let id = id.as_deref().map(parse_id).transpose()?;
            retry(id.as_ref(), &config()?, *wait, out)
        }
        args::MeetingCmd::Import { file } => import(file, out),
        args::MeetingCmd::List {
            since,
            until,
            timezone,
            cursor,
        } => list(since, until, timezone.as_deref(), cursor.as_deref(), out),
        args::MeetingCmd::Search { query, cursor } => search(query, cursor.as_deref(), out),
        args::MeetingCmd::Get { id } => get(id, out),
        args::MeetingCmd::Export {
            id,
            format,
            output,
            force,
        } => export(id, *format, output, *force, out),
        args::MeetingCmd::Delete { id, retry } => delete(id.as_deref(), *retry, out),
    }
}

fn import(file: &Path, out: &Output) -> Result<()> {
    let bytes = read_capped(file)?;
    let result = MeetingStore::new()
        .and_then(|store| store.import_json(&bytes))
        .map_err(store_err)?;
    let message = if result.already_present {
        "already present"
    } else {
        "imported"
    };
    if out.json {
        out.text(&serde_json::to_string(&serde_json::json!({
            "ok": true,
            "message": message,
            "meeting": result.meeting,
            "already_present": result.already_present,
        }))?)?;
    } else {
        out.confirm(&format!(
            "{message} {}",
            meeting_label(Some(&result.meeting))
        ))?;
    }
    Ok(())
}

fn list(
    since: &str,
    until: &str,
    timezone: Option<&str>,
    cursor: Option<&str>,
    out: &Output,
) -> Result<()> {
    let filter = ListFilter {
        since_ms: bound(since, timezone)?,
        until_ms: bound(until, timezone)?,
    };
    let cursor = cursor
        .map(serde_json::from_str::<ListCursor>)
        .transpose()
        .map_err(|_| usage("List cursor is not JSON from a previous page"))?;
    let Some(reader) = reader()? else {
        return empty_list(out);
    };
    let page = reader.list(&filter, cursor.as_ref()).map_err(store_err)?;
    if out.json {
        out.data(&page, "")?;
    } else if page.meetings.is_empty() {
        out.text("No meetings")?;
    } else {
        let rows = page
            .meetings
            .iter()
            .map(|m| {
                vec![
                    format!("{}:{}", m.meeting.store_id, m.meeting.id),
                    m.started_at_ms.to_string(),
                    m.title
                        .as_deref()
                        .map(meeting::display_text)
                        .unwrap_or_default(),
                    m.segment_count.to_string(),
                    m.gap_count.to_string(),
                    "absent".into(),
                ]
            })
            .collect::<Vec<_>>();
        out.table(
            &["ID", "START", "TITLE", "SEGMENTS", "GAPS", "NOTES"],
            &rows,
        )?;
        if let Some(next) = &page.next {
            out.text(&format!("next: {}", serde_json::to_string(next)?))?;
        }
    }
    Ok(())
}

fn search(query: &str, cursor: Option<&str>, out: &Output) -> Result<()> {
    let cursor = cursor
        .map(serde_json::from_str::<SearchCursor>)
        .transpose()
        .map_err(|_| usage("Search cursor is not JSON from a previous page"))?;
    let Some(reader) = reader()? else {
        return empty_search(out);
    };
    let page = reader.search(query, cursor.as_ref()).map_err(store_err)?;
    if out.json {
        out.data(&page, "")?;
    } else if page.segments.is_empty() && page.next.is_none() {
        out.text("No matches")?;
    } else {
        let rows = page
            .segments
            .iter()
            .map(|s| {
                vec![
                    format!("{}:{}", s.meeting.store_id, s.id),
                    s.range.start_ms.to_string(),
                    meeting::display_text(&s.source_id),
                    meeting::display_text(&s.text),
                ]
            })
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            out.table(&["ID", "START_MS", "SOURCE", "TEXT"], &rows)?;
        }
        if let Some(next) = &page.next {
            out.text(&format!("next: {}", serde_json::to_string(next)?))?;
        }
    }
    Ok(())
}

fn get(id: &str, out: &Output) -> Result<()> {
    let id = parse_id(id)?;
    let reader = reader()?.ok_or_else(|| store_err(ContractError::NotFound.into()))?;
    let status = reader.status(&id).map_err(store_err)?;
    if out.json {
        out.data(&status, "")?;
        return Ok(());
    }
    let title = status
        .summary
        .title
        .as_deref()
        .map(meeting::display_text)
        .unwrap_or_else(|| "Untitled meeting".into());
    out.text(&format!(
        "{title}\nid: {}:{}\nprovenance: {}\nnotes: absent\ntimezone: {}\nstarted_ms: {}\nended_ms: {}\nsegments: {}\ngaps: {}",
        status.summary.meeting.store_id,
        status.summary.meeting.id,
        match status.summary.provenance {
            Provenance::ImportedUnverified => "imported_unverified",
            Provenance::TranscribedAudio => "transcribed_audio",
        },
        meeting::display_text(&status.summary.timezone),
        status.summary.started_at_ms,
        status.summary.ended_at_ms,
        status.summary.segment_count,
        status.summary.gap_count
    ))?;
    out.text("sources:")?;
    for source in &status.sources {
        out.text(&format!(
            "- {} epoch {} label {} provider {} model {}",
            meeting::display_text(&source.id),
            source.epoch,
            source
                .label
                .as_deref()
                .map(meeting::display_text)
                .unwrap_or_else(|| "absent".into()),
            source
                .provider
                .as_deref()
                .map(meeting::display_text)
                .unwrap_or_else(|| "absent".into()),
            source
                .model
                .as_deref()
                .map(meeting::display_text)
                .unwrap_or_else(|| "absent".into())
        ))?;
    }
    out.text("gaps:")?;
    if status.gaps.is_empty() {
        out.text("- none")?;
    }
    for gap in &status.gaps {
        out.text(&format!(
            "- {}–{} {} {}",
            gap.range.start_ms,
            gap.range.end_ms,
            meeting::display_text(&gap.source_id),
            meeting::display_text(&gap.reason)
        ))?;
    }
    Ok(())
}

fn export(
    id: &str,
    format: args::MeetingFormat,
    output: &Path,
    force: bool,
    out: &Output,
) -> Result<()> {
    let id = parse_id(id)?;
    let format = match format {
        args::MeetingFormat::Json => ExportFormat::Json,
        args::MeetingFormat::Md => ExportFormat::Markdown,
        args::MeetingFormat::Text => ExportFormat::Text,
    };
    meeting::export_meeting(db_path()?, &id, format, output, force).map_err(store_err)?;
    if !out.quiet {
        out.confirm(&format!("exported {}", output.display()))?;
    }
    Ok(())
}

fn delete(id: Option<&str>, retry: bool, out: &Output) -> Result<()> {
    let store = MeetingStore::new().map_err(store_err)?;
    let status = if let Some(id) = id {
        let id = parse_id(id)?;
        let mut status = store.delete(&id).map_err(store_err)?;
        if retry && status == DeleteStatus::PhysicalCleanupPending {
            status = store.retry_cleanup().map_err(store_err)?;
        }
        status
    } else if retry {
        store.retry_cleanup().map_err(store_err)?
    } else {
        return Err(usage("Pass a meeting id, or --retry to finish cleanup"));
    };
    let message = delete_text(status);
    if out.json {
        out.text(&serde_json::to_string(&serde_json::json!({
            "ok": true,
            "message": message,
            "delete_status": status,
        }))?)?;
    } else {
        out.confirm(message)?;
    }
    Ok(())
}

/// One WAV source, read in 5 s chunks; never loaded whole.
struct Input {
    reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
    spec: hound::WavSpec,
    label: String,
    duration_ms: i64,
}
fn open_input(path: &Path) -> Result<Input> {
    let unreadable = |why: &str| {
        Error::new(
            1,
            format!("Could not read {}: {why}", path.display()),
            "Pass 16-bit or float WAV files; convert others first, e.g. afconvert -f WAVE -d LEI16 in.m4a out.wav",
        )
    };
    let file = std::fs::File::open(path).map_err(|e| unreadable(&e.to_string()))?;
    if !file.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err(unreadable("not a regular file"));
    }
    let reader = hound::WavReader::new(std::io::BufReader::new(file))
        .map_err(|_| unreadable("not a WAV file"))?;
    let spec = reader.spec();
    let supported = (1..=8).contains(&spec.channels)
        && (8_000..=192_000).contains(&spec.sample_rate)
        && match spec.sample_format {
            hound::SampleFormat::Int => (8..=32).contains(&spec.bits_per_sample),
            hound::SampleFormat::Float => spec.bits_per_sample == 32,
        };
    if !supported {
        return Err(unreadable("unsupported WAV format"));
    }
    let duration_ms = i64::from(reader.duration()) * 1000 / i64::from(spec.sample_rate);
    if duration_ms <= 0 {
        return Err(unreadable("no audio"));
    }
    let mut label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audio".into());
    while label.len() > 128 {
        label.pop();
    }
    Ok(Input {
        reader,
        spec,
        label,
        duration_ms,
    })
}
impl Input {
    /// Next 5 s as 16 kHz mono PCM16; `None` at the end.
    fn chunk(&mut self) -> anyhow::Result<Option<Vec<i16>>> {
        let channels = usize::from(self.spec.channels);
        let wanted = self.spec.sample_rate as usize * 5 * channels;
        let samples: Vec<f32> = match self.spec.sample_format {
            hound::SampleFormat::Float => self
                .reader
                .samples::<f32>()
                .take(wanted)
                .collect::<std::result::Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let scale = (1u64 << (self.spec.bits_per_sample - 1)) as f32;
                self.reader
                    .samples::<i32>()
                    .take(wanted)
                    .map(|s| s.map(|s| s as f32 / scale))
                    .collect::<std::result::Result<_, _>>()?
            }
        };
        if samples.is_empty() {
            return Ok(None);
        }
        let samples = samples
            .chunks_exact(channels)
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        if samples.is_empty() {
            return Ok(None);
        }
        let mono = if channels == 1 && self.spec.sample_rate == 16_000 {
            samples
        } else {
            xflow_providers::upload_audio(&xflow_core::AudioClip {
                samples,
                sample_rate: self.spec.sample_rate,
                channels: self.spec.channels,
            })?
            .samples
        };
        Ok(Some(
            mono.iter()
                .map(|&s| {
                    let s = if s.is_finite() {
                        s.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                    (s * if s < 0.0 { 32768.0 } else { 32767.0 }).round() as i16
                })
                .collect(),
        ))
    }
}

fn process(
    files: &[PathBuf],
    title: Option<&str>,
    config: &Path,
    wait: bool,
    out: &Output,
) -> Result<()> {
    if let Some(title) = title {
        if title.is_empty() || title.len() > 256 {
            return Err(usage("Title must be 1 to 256 bytes"));
        }
    }
    let mut inputs = files
        .iter()
        .map(|f| open_input(f))
        .collect::<Result<Vec<_>>>()?;
    let db = db_path()?;
    let mut capture = Capture::begin(
        &db,
        CaptureSpec {
            title: title.map(str::to_owned),
            labels: inputs.iter().map(|i| i.label.clone()).collect(),
            expected_ms: inputs.iter().map(|i| i.duration_ms).max(),
            queued: true,
        },
        Limits::from_env(),
    )
    .map_err(|e| {
        Error::new(
            1,
            format!("Meeting audio not stored: {e:#}"),
            "Run xflow meeting status",
        )
    })?;
    let id = capture.id();
    let mut problem = None;
    'sources: for (idx, input) in inputs.iter_mut().enumerate() {
        loop {
            let chunk = match input.chunk() {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(e) => {
                    problem = Some((
                        false,
                        format!("{} could not be read to the end: {e}", input.label),
                    ));
                    break 'sources;
                }
            };
            if let Err(e) = capture.push(idx, &chunk) {
                problem = Some((true, format!("{e:#}")));
                break 'sources;
            }
        }
    }
    for warning in capture.warnings.drain(..) {
        eprintln!("xflow: warning: {warning}");
    }
    let closed = match &problem {
        None => capture.finish(),
        Some((_, reason)) => capture.fail(reason),
    };
    closed.map_err(|e| {
        Error::new(
            1,
            format!("Meeting audio was only partly stored: {e:#}"),
            "Run xflow meeting status",
        )
    })?;
    if !out.json && !out.quiet {
        eprintln!("xflow: meeting {}:{} stored privately", id.store_id, id.id);
    }
    let transcribed = start(Some(&id), config, wait, out);
    match problem {
        Some((_, reason)) if transcribed.is_ok() => Err(Error::new(
            1,
            format!(
                "Capture stopped early: {}. The saved part was transcribed",
                meeting::display_text(&reason)
            ),
            "Run xflow meeting status",
        )),
        _ => transcribed,
    }
}

/// The meeting STT route: the configured primary only. A backup is a second
/// destination and a second bill, so meetings never use it implicitly.
fn meeting_stt(config: &Path) -> Result<std::sync::Arc<dyn xflow_core::SpeechToText>> {
    let cfg = super::config::load(config)?
        .config()
        .map_err(Error::config)?;
    cfg.validate().map_err(Error::config)?;
    let mut stt = cfg.stt.clone();
    stt.fallback.clear();
    xflow_providers::build_stt(&stt, cfg.privacy.offline).map_err(|e| {
        Error::new(
            5,
            format!("Speech-to-text is unavailable: {e:#}. The audio is kept."),
            "Fix the provider (xflow providers info, xflow key status), then run xflow meeting retry",
        )
    })
}

async fn dictation_busy() -> bool {
    use xflow_core::{ipc::Request, State};
    matches!(
        tokio::time::timeout(
            Duration::from_millis(500),
            xflow_app::transport::request(&Request::Status)
        )
        .await,
        Ok(Ok(response)) if matches!(response.state, State::Listening | State::Processing)
    )
}

fn retry(meeting: Option<&MeetingId>, config: &Path, wait: bool, out: &Output) -> Result<()> {
    let db = db_path()?;
    if !db.exists() {
        out.text("Nothing waiting for transcription")?;
        return Ok(());
    }
    if let Some(id) = meeting {
        // Validates the store id before anything changes.
        MeetingReader::at(db.clone())
            .status(id)
            .map_err(store_err)?;
    }
    // Local recovery only when no worker holds the lock (a worker recovers
    // when it starts). It never contacts a provider.
    if let Some(lock) = transcribe::lock_worker(&db, false).map_err(store_err)? {
        let recovered = journal::recover(&db).map_err(store_err)?;
        drop(lock);
        if recovered != journal::Recovery::default() && !out.quiet && !out.json {
            eprintln!(
                "xflow: recovered meeting audio: {} piece(s) kept, {} torn, {} quarantined, {} missing, {} interrupted capture(s), {} unanswered request(s) counted",
                recovered.imported, recovered.torn, recovered.quarantined, recovered.missing, recovered.interrupted, recovered.ambiguous
            );
        }
    }
    jobs::reopen(&db, meeting.map(|m| m.id)).map_err(store_err)?;
    start(meeting, config, wait, out)
}

/// Queues the meetings and makes sure the background worker runs. With
/// `wait`, follows progress until nothing queued is open.
fn start(meeting: Option<&MeetingId>, config: &Path, wait: bool, out: &Output) -> Result<()> {
    let db = db_path()?;
    if jobs::queue(&db, meeting.map(|m| m.id)).map_err(store_err)? == 0 {
        out.text("Nothing waiting for transcription")?;
        return Ok(());
    }
    // Fail early and visibly on a broken provider; the worker would only record it.
    meeting_stt(config)?;
    start_worker(&db, config)?;
    if !wait {
        let target = meeting
            .map(|m| format!(" {}:{}", m.store_id, m.id))
            .unwrap_or_default();
        if out.json {
            out.text(&serde_json::to_string(&serde_json::json!({
                "ok": true,
                "message": "transcribing in the background",
                "meeting": meeting,
            }))?)?;
        } else {
            out.text(&format!(
                "transcribing in the background; see xflow meeting status{target}"
            ))?;
        }
        return Ok(());
    }
    wait_for(&db, meeting, config, out)
}

/// Starts `xflow meeting-worker` in its own session, ignoring SIGHUP, with
/// no terminal and no pipe to this process. A running worker makes this a no-op.
fn start_worker(db: &Path, config: &Path) -> Result<bool> {
    if transcribe::worker_running(db) {
        return Ok(false);
    }
    let root = journal::spool_root(db);
    paths::private_dir(&root).map_err(store_err)?;
    let log_path = root.join("worker.log");
    if std::fs::symlink_metadata(&log_path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::remove_file(&log_path);
    }
    let log = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&log_path)?
    };
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .arg("--config")
        .arg(config)
        .arg("meeting-worker")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    command.spawn().map_err(|e| {
        Error::new(
            1,
            format!("Could not start background transcription: {e}"),
            "Run xflow meeting retry",
        )
    })?;
    Ok(true)
}

/// The detached worker. Its stderr is a private log with no transcript text.
pub fn worker(config: &Path) -> Result<()> {
    let db = db_path()?;
    if !db.exists() {
        return Ok(());
    }
    ipc::block_on(async {
        let (tx, rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            if let Ok(mut term) = signal(SignalKind::terminate()) {
                term.recv().await;
                let _ = tx.send(true);
            }
        });
        let mut log = |line: &str| {
            eprintln!(
                "{} meeting-worker[{}]: {}",
                transcribe::now_ms(),
                std::process::id(),
                meeting::display_text(line)
            );
        };
        transcribe::work(
            &db,
            || meeting_stt(config).map_err(|e| anyhow::anyhow!(e.message)),
            dictation_busy,
            rx,
            &mut log,
        )
        .await
        .map_err(store_err)
    })
}

fn wait_for(db: &Path, meeting: Option<&MeetingId>, config: &Path, out: &Output) -> Result<()> {
    let verbose = !out.json && !out.quiet;
    let started = std::time::Instant::now();
    let mut shown = (-1, -1);
    let mut restarts = 0;
    loop {
        let status = jobs::status(
            db,
            meeting.map(|m| m.id),
            Limits::from_env(),
            transcribe::now_ms(),
        )
        .map_err(store_err)?;
        let mut sum = jobs::Windows::default();
        for m in &status.meetings {
            sum.total += m.windows.total;
            sum.accepted += m.windows.accepted;
            sum.empty += m.windows.empty;
            sum.failed += m.windows.failed;
            sum.pending += m.windows.pending;
            sum.waiting += m.windows.waiting;
            sum.retained += m.windows.retained;
            sum.abandoned += m.windows.abandoned;
        }
        if verbose && (sum.accepted, sum.total) != shown {
            shown = (sum.accepted, sum.total);
            eprintln!(
                "xflow: transcribed {} of {} part(s)",
                sum.accepted, sum.total
            );
        }
        let open = if meeting.is_some() {
            sum.pending + sum.waiting
        } else {
            status.worker.queued as i64
        };
        if open == 0 {
            if out.json {
                out.text(&serde_json::to_string(&serde_json::json!({
                    "ok": sum.failed == 0,
                    "windows": sum,
                }))?)?;
            } else {
                out.text(&format!(
                    "transcribed {} part(s) ({} without speech); {} failed",
                    sum.accepted, sum.empty, sum.failed
                ))?;
            }
            if sum.failed > 0 {
                return Err(Error::new(
                    1,
                    "Some audio could not be transcribed; it is kept",
                    "Run xflow meeting status, then xflow meeting retry",
                ));
            }
            return Ok(());
        }
        if !status.worker.running && started.elapsed() > Duration::from_secs(3) {
            if matches!(status.worker.state.as_deref(), Some("stopped" | "failed")) || restarts >= 3
            {
                return Err(Error::new(
                    5,
                    format!(
                        "Background transcription stopped: {}",
                        meeting::display_text(
                            status.worker.error.as_deref().unwrap_or("unknown reason")
                        )
                    ),
                    "The audio is kept. Fix the cause, then run xflow meeting retry",
                ));
            }
            restarts += 1;
            start_worker(db, config)?;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn status(id: Option<&str>, config: &Path, out: &Output) -> Result<()> {
    let id = id.map(parse_id).transpose()?;
    let db = db_path()?;
    if !db.exists() {
        return empty_status(out);
    }
    let status = jobs::status(
        &db,
        id.as_ref().map(|m| m.id),
        Limits::from_env(),
        transcribe::now_ms(),
    )
    .map_err(store_err)?;
    if let Some(id) = &id {
        if status.meetings.first().is_none_or(|m| m.meeting != *id) {
            return Err(store_err(ContractError::NotFound.into()));
        }
    }
    // Queued work whose worker died (crash, reboot): continue it. This only
    // resumes what the user already asked to transcribe.
    let mut status = status;
    if status.worker.queued > 0 && !status.worker.running && start_worker(&db, config)? {
        status.worker.running = true;
        status.worker.state = Some("restarted".into());
    }
    if out.json {
        return Ok(out.data(&status, "")?);
    }
    out.text(&format!(
        "background transcription: {}{}",
        if status.worker.running {
            "running"
        } else {
            "not running"
        },
        status
            .worker
            .state
            .as_deref()
            .map(|s| format!(" (last state {s})"))
            .unwrap_or_default()
    ))?;
    out.text(&format!(
        "meeting audio: {} MiB of {} MiB",
        status.audio_bytes.div_ceil(1 << 20),
        status.limit_bytes >> 20
    ))?;
    for warning in &status.warnings {
        out.text(&format!("warning: {}", meeting::display_text(warning)))?;
    }
    if status.meetings.is_empty() {
        out.text("No meeting audio")?;
        return Ok(());
    }
    let rows = status
        .meetings
        .iter()
        .map(|m| {
            vec![
                format!("{}:{}", m.meeting.store_id, m.meeting.id),
                m.capture.clone(),
                format!("{}/{}", m.windows.accepted, m.windows.total),
                m.windows.failed.to_string(),
                (m.windows.pending + m.windows.waiting).to_string(),
                format!("{}s", m.pending_audio.seconds),
                m.pending_audio
                    .age_ms
                    .map(|a| format!("{}h{:02}m", a / 3_600_000, a / 60_000 % 60))
                    .unwrap_or_else(|| "-".into()),
                m.gaps.to_string(),
            ]
        })
        .collect::<Vec<_>>();
    out.table(
        &[
            "ID", "CAPTURE", "DONE", "FAILED", "OPEN", "AUDIO", "AGE", "GAPS",
        ],
        &rows,
    )?;
    for m in &status.meetings {
        for warning in &m.warnings {
            out.text(&format!(
                "{}:{}: warning: {}",
                m.meeting.store_id,
                m.meeting.id,
                meeting::display_text(warning)
            ))?;
        }
        if let Some(error) = &m.last_error {
            out.text(&format!(
                "{}:{}: last error: {}",
                m.meeting.store_id,
                m.meeting.id,
                meeting::display_text(error)
            ))?;
        }
    }
    Ok(())
}
fn empty_status(out: &Output) -> Result<()> {
    if out.json {
        out.text(r#"{"meetings":[],"audio_bytes":0}"#)?;
    } else {
        out.text("No meeting audio")?;
    }
    Ok(())
}

fn delete_text(status: DeleteStatus) -> &'static str {
    match status {
        DeleteStatus::PhysicallyCleaned => "physically cleaned",
        DeleteStatus::PhysicalCleanupPending => "physical cleanup pending",
    }
}

fn read_capped(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|_| {
        Error::new(
            1,
            format!("Could not read {}", path.display()),
            "Pass the JSON transcript to import",
        )
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_IMPORT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(Error::new(
            1,
            "Import exceeds 8 MiB",
            "Split the transcript into smaller imports",
        ));
    }
    Ok(bytes)
}

fn reader() -> Result<Option<MeetingReader>> {
    let path = db_path()?;
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(MeetingReader::at(path)))
}

fn db_path() -> Result<std::path::PathBuf> {
    Ok(paths::data_dir()?.join("meetings.db"))
}

fn empty_list(out: &Output) -> Result<()> {
    if out.json {
        out.text(r#"{"meetings":[],"next":null}"#)?;
    } else {
        out.text("No meetings")?;
    }
    Ok(())
}

fn empty_search(out: &Output) -> Result<()> {
    if out.json {
        out.text(r#"{"segments":[],"next":null}"#)?;
    } else {
        out.text("No matches")?;
    }
    Ok(())
}

fn parse_id(value: &str) -> Result<MeetingId> {
    let (store, id) = value
        .split_once(':')
        .ok_or_else(|| usage("Meeting id is store_id:id"))?;
    if store.len() != 32 || !store.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(usage("Meeting store id is 32 hex characters"));
    }
    let id: i64 = id
        .parse()
        .map_err(|_| usage("Meeting id is not an integer"))?;
    if id <= 0 {
        return Err(usage("Meeting id must be positive"));
    }
    Ok(MeetingId {
        store_id: store.to_string(),
        id,
    })
}

fn bound(value: &str, timezone: Option<&str>) -> Result<i64> {
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return value
            .parse::<i64>()
            .map_err(|_| usage("Time bound is out of range"));
    }
    let offset =
        timezone.ok_or_else(|| usage("A YYYY-MM-DD filter needs --timezone Z or ±HH:MM"))?;
    let offset_ms = offset_ms(offset)?;
    let (year, month, day) = ymd(value)?;
    let days = days_from_civil(year, month, day)?;
    Ok(days * 86_400_000 - offset_ms)
}

fn offset_ms(value: &str) -> Result<i64> {
    if value == "Z" || value == "z" {
        return Ok(0);
    }
    let (sign, rest) = value
        .strip_prefix('+')
        .map(|rest| (1, rest))
        .or_else(|| value.strip_prefix('-').map(|rest| (-1, rest)))
        .ok_or_else(|| usage("Timezone is Z or ±HH:MM"))?;
    let (hours, minutes) = rest
        .split_once(':')
        .ok_or_else(|| usage("Timezone is Z or ±HH:MM"))?;
    let hours: i64 = hours
        .parse()
        .map_err(|_| usage("Timezone hour is not a number"))?;
    let minutes: i64 = minutes
        .parse()
        .map_err(|_| usage("Timezone minute is not a number"))?;
    if !(0..=14).contains(&hours) || !(0..60).contains(&minutes) {
        return Err(usage("Timezone is outside ±14:00"));
    }
    Ok(sign * (hours * 3_600_000 + minutes * 60_000))
}

fn ymd(value: &str) -> Result<(i32, u32, u32)> {
    let mut parts = value.split('-');
    let year: i32 = parts
        .next()
        .unwrap_or("")
        .parse()
        .map_err(|_| usage("Date is YYYY-MM-DD"))?;
    let month: u32 = parts
        .next()
        .unwrap_or("")
        .parse()
        .map_err(|_| usage("Date is YYYY-MM-DD"))?;
    let day: u32 = parts
        .next()
        .unwrap_or("")
        .parse()
        .map_err(|_| usage("Date is YYYY-MM-DD"))?;
    if parts.next().is_some() || !valid_day(year, month, day) {
        return Err(usage("Date is YYYY-MM-DD"));
    }
    Ok((year, month, day))
}

fn valid_day(year: i32, month: u32, day: u32) -> bool {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    (1..=max).contains(&day)
}

/// Days since 1970-01-01. Howard Hinnant's civil calendar.
fn days_from_civil(mut year: i32, month: u32, day: u32) -> Result<i64> {
    if month <= 2 {
        year -= 1;
    }
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = (year - era * 400) as u64;
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month_index + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u64;
    let days = era as i64 * 146097 + doe as i64 - 719468;
    if days < 0 {
        return Err(usage("Date is before 1970-01-01"));
    }
    Ok(days)
}

fn meeting_label(meeting: Option<&MeetingId>) -> String {
    match meeting {
        Some(meeting) => format!("{}:{}", meeting.store_id, meeting.id),
        None => "unknown".into(),
    }
}

fn usage(message: &str) -> Error {
    Error::new(2, message, "Run xflow meeting --help")
}

fn store_err(error: anyhow::Error) -> Error {
    if let Some(contract) = error.downcast_ref::<ContractError>() {
        Error::new(
            1,
            meeting::contract_message(contract),
            "Run xflow meeting --help",
        )
    } else {
        Error::new(1, error.to_string(), "Run xflow meeting --help")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_dates_use_the_given_offset() {
        assert_eq!(bound("0", None).unwrap(), 0);
        assert_eq!(bound("1970-01-01", Some("Z")).unwrap(), 0);
        assert_eq!(bound("1970-01-02", Some("Z")).unwrap(), 86_400_000);
        assert_eq!(
            bound("1970-01-02", Some("+05:30")).unwrap(),
            86_400_000 - (5 * 3_600_000 + 30 * 60_000)
        );
        assert!(bound("1970-01-02", None).is_err());
    }
}
