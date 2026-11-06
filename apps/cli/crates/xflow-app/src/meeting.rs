//! Explicit M1 operations only; constructing a handle performs no I/O.
//! Process entry points must call `store::skip_bulk_page_cache` before ANY SQLite
//! connection, exactly as for history (M0). Never configure SQLite from a store opener.
pub mod export;
pub mod jobs;
pub mod journal;
pub mod query;
pub mod store;
pub mod transcribe;
pub use export::{display_text, export_meeting, export_meeting_checking, ExportFormat};
pub use query::MeetingReader;
pub use store::{ContractError, MeetingStore};

pub fn contract_message(error: &ContractError) -> String {
    match error {
        ContractError::InvalidImport(reason) => format!("Invalid meeting import: {reason}"),
        ContractError::InvalidQuery(reason) => format!("Invalid meeting query: {reason}"),
        ContractError::StaleCursor => {
            "The meeting changed or was deleted; repeat the command".into()
        }
        ContractError::NotFound => "Meeting not found".into(),
        ContractError::ImportConflict => {
            "Import identity already exists with different content".into()
        }
        ContractError::UnsupportedSchema(version) => {
            format!(
                "Meeting database schema {version} is newer than this xflow and was left unchanged"
            )
        }
        ContractError::MalformedStore => {
            "Meeting database is malformed and was left unchanged".into()
        }
    }
}

/// Test-support builds only: `XFLOW_MEETING_FAULT=point=N=kill|enospc|eio,...`
/// fails or SIGKILLs this process the Nth time it reaches `point`.
#[cfg(feature = "test-support")]
pub(crate) fn fault(point: &str) -> std::io::Result<()> {
    use std::{collections::HashMap, sync::Mutex};
    static SEEN: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
    let Ok(spec) = std::env::var("XFLOW_MEETING_FAULT") else {
        return Ok(());
    };
    let mut seen = SEEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let count = seen
        .get_or_insert_with(HashMap::new)
        .entry(point.to_owned())
        .or_insert(0);
    *count += 1;
    for rule in spec.split(',') {
        let mut parts = rule.split('=');
        let (Some(name), Some(n), Some(action)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if name != point || n.parse::<u32>().ok() != Some(*count) {
            continue;
        }
        match action {
            "kill" => unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            },
            "enospc" => return Err(std::io::Error::from_raw_os_error(libc::ENOSPC)),
            "eio" => return Err(std::io::Error::from_raw_os_error(libc::EIO)),
            _ => {}
        }
    }
    Ok(())
}
#[cfg(not(feature = "test-support"))]
#[inline(always)]
pub(crate) fn fault(_: &str) -> std::io::Result<()> {
    Ok(())
}
