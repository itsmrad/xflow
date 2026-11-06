//! Render one frozen transcript. Nothing is written until every page agrees.
use super::store::ContractError;
use super::MeetingReader;
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use xflow_core::meeting::{
    Gap, MeetingId, MeetingStatus, Provenance, Segment, Source, TimeRange, TranscriptQuery,
    MAX_WINDOW_MS,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Json,
    Markdown,
    Text,
}

/// Terminal controls become spaces. Stored text is not modified.
pub fn display_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Display text plus Markdown/HTML punctuation, so images and tables stay inert.
pub fn markdown_text(text: &str) -> String {
    let mut out = String::new();
    for c in display_text(text).chars() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '!' | '<' | '>' | '#' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

pub fn export_meeting(
    db: impl Into<PathBuf>,
    meeting: &MeetingId,
    format: ExportFormat,
    dest: &Path,
    force: bool,
) -> Result<()> {
    export_inner(db.into(), meeting, format, dest, force, |_| {})
}

/// `on_page` runs before each transcript read, so a test can edit between pages.
#[doc(hidden)]
pub fn export_meeting_checking(
    db: impl Into<PathBuf>,
    meeting: &MeetingId,
    format: ExportFormat,
    dest: &Path,
    force: bool,
    on_page: impl FnMut(u32),
) -> Result<()> {
    export_inner(db.into(), meeting, format, dest, force, on_page)
}

fn export_inner(
    db: PathBuf,
    meeting: &MeetingId,
    format: ExportFormat,
    dest: &Path,
    force: bool,
    mut on_page: impl FnMut(u32),
) -> Result<()> {
    ensure!(
        !replaces_store(dest)?,
        "export cannot replace the meeting or history database"
    );
    if let Ok(meta) = std::fs::symlink_metadata(dest) {
        ensure!(
            !meta.file_type().is_symlink(),
            "export path is a symlink and was not followed"
        );
        ensure!(force, "export already exists; pass --force to replace it");
        ensure!(meta.is_file(), "export path is not a file");
    }
    // A bare filename has an empty parent and means the current directory.
    let parent = match dest.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let parent_meta = std::fs::symlink_metadata(parent).context("export directory")?;
    ensure!(
        parent_meta.is_dir() && !parent_meta.file_type().is_symlink(),
        "export directory is missing or is a symlink"
    );

    let reader = MeetingReader::at(db);
    let status = reader.status(meeting)?;
    let snapshot = status.snapshot;
    let duration = status.summary.ended_at_ms - status.summary.started_at_ms;
    let mut segments = Vec::new();
    let mut seen = HashSet::new();
    let mut start = 0;
    let mut pages = 0u32;
    while start < duration {
        let end = start.saturating_add(MAX_WINDOW_MS).min(duration);
        if end <= start {
            break;
        }
        let mut query = TranscriptQuery {
            meeting: meeting.clone(),
            range: TimeRange {
                start_ms: start,
                end_ms: end,
            },
            snapshot: Some(snapshot),
            cursor: None,
        };
        loop {
            on_page(pages);
            pages += 1;
            let page = match reader.transcript(&query) {
                Ok(page) => page,
                Err(error)
                    if error.downcast_ref::<ContractError>()
                        == Some(&ContractError::StaleCursor) =>
                {
                    return Err(ContractError::StaleCursor.into());
                }
                Err(error) => return Err(error),
            };
            if page.snapshot != snapshot {
                return Err(ContractError::StaleCursor.into());
            }
            for segment in page.segments {
                // A segment that crosses a 10-minute edge overlaps both windows.
                if seen.insert(segment.id) {
                    segments.push(segment);
                }
            }
            match page.next {
                Some(cursor) => {
                    query.cursor = Some(cursor);
                    query.snapshot = Some(snapshot);
                }
                None => break,
            }
        }
        start = end;
    }
    let body = render(format, &status, &segments)?;
    write_private(parent, dest, force, body.as_bytes())
}

fn provenance(status: &MeetingStatus) -> &'static str {
    match status.summary.provenance {
        Provenance::ImportedUnverified => "imported_unverified",
        Provenance::TranscribedAudio => "transcribed_audio",
    }
}

fn render(format: ExportFormat, status: &MeetingStatus, segments: &[Segment]) -> Result<String> {
    match format {
        ExportFormat::Json => {
            // Serde escapes control bytes. The strings themselves stay original.
            let view = JsonExport {
                meeting: status.summary.meeting.clone(),
                title: status.summary.title.clone(),
                provenance: provenance(status),
                notes: "absent",
                timezone: status.summary.timezone.clone(),
                started_at_ms: status.summary.started_at_ms,
                ended_at_ms: status.summary.ended_at_ms,
                sources: status
                    .sources
                    .iter()
                    .map(|s| JsonSource {
                        id: s.id.clone(),
                        label: s.label.clone(),
                        epoch: s.epoch,
                        provider: s.provider.clone(),
                        model: s.model.clone(),
                    })
                    .collect(),
                gaps: status
                    .gaps
                    .iter()
                    .map(|g| JsonGap {
                        source_id: g.source_id.clone(),
                        start_ms: g.range.start_ms,
                        end_ms: g.range.end_ms,
                        reason: g.reason.clone(),
                    })
                    .collect(),
                segments: segments.iter().map(json_segment).collect(),
            };
            Ok(serde_json::to_string_pretty(&view)?)
        }
        ExportFormat::Markdown => Ok(markdown(status, segments)),
        ExportFormat::Text => Ok(text(status, segments)),
    }
}

fn markdown(status: &MeetingStatus, segments: &[Segment]) -> String {
    let title = status
        .summary
        .title
        .as_deref()
        .map(markdown_text)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Untitled meeting".into());
    let mut out = format!(
        "# {title}\n\nProvenance: {}\nNotes: absent\nTimezone: {}\nStarted (Unix ms): {}\nEnded (Unix ms): {}\n\n## Sources\n",
        provenance(status),
        markdown_text(&status.summary.timezone),
        status.summary.started_at_ms,
        status.summary.ended_at_ms
    );
    if status.sources.is_empty() {
        out.push_str("\n(none)\n");
    }
    for source in &status.sources {
        out.push_str(&format!("\n- {}\n", source_line(source, true)));
    }
    out.push_str("\n## Gaps\n");
    if status.gaps.is_empty() {
        out.push_str("\n(none)\n");
    }
    for gap in &status.gaps {
        out.push_str(&format!("\n- {}\n", gap_line(gap, true)));
    }
    out.push_str("\n## Transcript\n");
    for segment in segments {
        out.push_str(&format!(
            "\n### {}–{}\n\n{}\n\n{}\n",
            clock(segment.range.start_ms),
            clock(segment.range.end_ms),
            source_stamp(segment, true),
            markdown_text(&segment.text)
        ));
    }
    out
}

fn text(status: &MeetingStatus, segments: &[Segment]) -> String {
    let mut out = format!(
        "provenance: {}\nnotes: absent\ntimezone: {}\nstarted_ms: {}\nended_ms: {}\n",
        provenance(status),
        display_text(&status.summary.timezone),
        status.summary.started_at_ms,
        status.summary.ended_at_ms
    );
    for source in &status.sources {
        out.push_str(&format!("source: {}\n", source_line(source, false)));
    }
    for gap in &status.gaps {
        out.push_str(&format!("gap: {}\n", gap_line(gap, false)));
    }
    for segment in segments {
        out.push_str(&format!(
            "{} {} {} {}\n",
            clock(segment.range.start_ms),
            clock(segment.range.end_ms),
            source_stamp(segment, false),
            display_text(&segment.text)
        ));
    }
    out
}

fn source_line(source: &Source, markdown: bool) -> String {
    let text = |s: &str| {
        if markdown {
            markdown_text(s)
        } else {
            display_text(s)
        }
    };
    format!(
        "{} epoch {} provider {} model {} label {}",
        text(&source.id),
        source.epoch,
        source
            .provider
            .as_deref()
            .map(text)
            .unwrap_or_else(|| "absent".into()),
        source
            .model
            .as_deref()
            .map(text)
            .unwrap_or_else(|| "absent".into()),
        source
            .label
            .as_deref()
            .map(text)
            .unwrap_or_else(|| "absent".into())
    )
}

fn gap_line(gap: &Gap, markdown: bool) -> String {
    let text = |s: &str| {
        if markdown {
            markdown_text(s)
        } else {
            display_text(s)
        }
    };
    format!(
        "{}–{} {} {}",
        clock(gap.range.start_ms),
        clock(gap.range.end_ms),
        text(&gap.source_id),
        text(&gap.reason)
    )
}

fn source_stamp(segment: &Segment, markdown: bool) -> String {
    let id = if markdown {
        markdown_text(&segment.source_id)
    } else {
        display_text(&segment.source_id)
    };
    format!(
        "{id} sequence {} revision {}",
        segment.sequence, segment.revision
    )
}

fn json_segment(segment: &Segment) -> JsonSegment {
    JsonSegment {
        id: segment.id,
        source_id: segment.source_id.clone(),
        sequence: segment.sequence,
        start_ms: segment.range.start_ms,
        end_ms: segment.range.end_ms,
        revision: segment.revision,
        text: segment.text.clone(),
    }
}

fn clock(ms: i64) -> String {
    let ms = ms.max(0);
    let hours = ms / 3_600_000;
    let minutes = (ms % 3_600_000) / 60_000;
    let seconds = (ms % 60_000) / 1_000;
    let millis = ms % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

/// Meeting and history databases, including sidecars, by real path and inode.
fn replaces_store(dest: &Path) -> Result<bool> {
    let data = crate::paths::data_dir()?;
    for name in ["meetings.db", "history.db"] {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            if same_place(dest, &data.join(format!("{name}{suffix}"))) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn same_place(dest: &Path, protected: &Path) -> bool {
    if let (Some(dest), Some(protected)) = (logical_path(dest), logical_path(protected)) {
        if dest == protected {
            return true;
        }
    }
    match (file_id(dest), file_id(protected)) {
        (Some(dest), Some(protected)) => dest == protected,
        _ => false,
    }
}

fn logical_path(path: &Path) -> Option<PathBuf> {
    if let Ok(real) = std::fs::canonicalize(path) {
        return Some(real);
    }
    let name = path.file_name()?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    Some(parent.canonicalize().ok()?.join(name))
}

fn file_id(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn publish(temp: &Path, dest: &Path, force: bool) -> Result<()> {
    if force {
        return std::fs::rename(temp, dest).map_err(Into::into);
    }
    match std::fs::hard_link(temp, dest) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::bail!("export already exists; pass --force to replace it")
        }
        Err(error) if link_unsupported(&error) => anyhow::bail!(
            "this filesystem does not support hard links, so the export was not written ({error})"
        ),
        Err(error) => Err(error.into()),
    }
}

fn link_unsupported(error: &std::io::Error) -> bool {
    let Some(code) = error.raw_os_error() else {
        return false;
    };
    code == libc::EXDEV || code == libc::EOPNOTSUPP || code == libc::ENOTSUP
}

fn write_private(parent: &Path, dest: &Path, force: bool, body: &[u8]) -> Result<()> {
    let temp = parent.join(format!(
        ".meeting-export-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let write_temp = || -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temp)?;
            use std::io::Write;
            file.write_all(body)?;
        }
        #[cfg(not(unix))]
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(body)?;
        }
        Ok(())
    };
    if let Err(error) = write_temp() {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = publish(&temp, dest, force) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    let _ = std::fs::remove_file(&temp);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[derive(Serialize)]
struct JsonExport {
    meeting: MeetingId,
    title: Option<String>,
    provenance: &'static str,
    notes: &'static str,
    timezone: String,
    started_at_ms: i64,
    ended_at_ms: i64,
    sources: Vec<JsonSource>,
    gaps: Vec<JsonGap>,
    segments: Vec<JsonSegment>,
}
#[derive(Serialize)]
struct JsonSource {
    id: String,
    label: Option<String>,
    epoch: u32,
    provider: Option<String>,
    model: Option<String>,
}
#[derive(Serialize)]
struct JsonGap {
    source_id: String,
    start_ms: i64,
    end_ms: i64,
    reason: String,
}
#[derive(Serialize)]
struct JsonSegment {
    id: i64,
    source_id: String,
    sequence: i64,
    start_ms: i64,
    end_ms: i64,
    revision: i64,
    text: String,
}
