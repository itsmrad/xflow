//! Explicit M1 operations only; constructing a handle performs no I/O.
//! Process entry points must call `store::skip_bulk_page_cache` before ANY SQLite
//! connection, exactly as for history (M0). Never configure SQLite from a store opener.
pub mod export;
pub mod query;
pub mod store;
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
