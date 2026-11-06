//! Explicit M1 operations only; constructing a handle performs no I/O.
//! Process entry points must call `store::skip_bulk_page_cache` before ANY SQLite
//! connection, exactly as for history (M0). Never configure SQLite from a store opener.
pub mod query;
pub mod store;
pub use query::MeetingReader;
pub use store::{ContractError, MeetingStore};
