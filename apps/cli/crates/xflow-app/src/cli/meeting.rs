use super::{args, output::Output, Error, Result};
use std::{io::Read, path::Path};
use xflow_app::{
    meeting::{self, ContractError, ExportFormat, MeetingReader, MeetingStore},
    paths,
};
use xflow_core::meeting::{
    DeleteStatus, ListCursor, ListFilter, MeetingId, SearchCursor, MAX_IMPORT_BYTES,
};

pub fn run(action: &args::MeetingCmd, out: &Output) -> Result<()> {
    match action {
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
        "{title}\nid: {}:{}\nprovenance: imported_unverified\nnotes: absent\ntimezone: {}\nstarted_ms: {}\nended_ms: {}\nsegments: {}\ngaps: {}",
        status.summary.meeting.store_id,
        status.summary.meeting.id,
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
