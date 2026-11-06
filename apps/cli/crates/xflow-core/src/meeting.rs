//! M1 imported text only. IDs are local to `store_id`; text is untrusted display data.
use serde::{Deserialize, Serialize};

pub const IMPORT_VERSION: u32 = 1;
pub const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SEGMENTS: usize = 1024;
pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_PAGE_SEGMENTS: usize = 20;
pub const MAX_PAGE_BYTES: usize = 24 * 1024;
/// Conservative token bound: at most one token per serialized UTF-8 byte.
pub const PAGE_BYTES: usize = 6000;
pub const MAX_WINDOW_MS: i64 = 600_000;
pub const MAX_SEGMENT_MS: i64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetingId {
    pub store_id: String,
    pub id: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    pub start_ms: i64,
    pub end_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Timing, source labels and attribution supplied by the importer, unverified by xflow.
    ImportedUnverified,
    /// Audio the user supplied, transcribed by xflow. Times are offsets in that
    /// audio; the start is when xflow received it, not when it was recorded.
    TranscribedAudio,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub label: Option<String>,
    pub epoch: u32,
    pub provider: Option<String>,
    pub model: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportSegment {
    pub source_id: String,
    pub sequence: i64,
    pub range: TimeRange,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gap {
    pub source_id: String,
    pub range: TimeRange,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetingImport {
    pub schema_version: u32,
    pub import_id: String,
    pub provenance: Provenance,
    pub title: Option<String>,
    /// Supplied Unix milliseconds, never a planned calendar time.
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    /// Explicit supplied timezone label or offset; the store does not interpret dates.
    pub timezone: String,
    pub sources: Vec<Source>,
    pub segments: Vec<ImportSegment>,
    pub gaps: Vec<Gap>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportResult {
    pub meeting: MeetingId,
    pub already_present: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub highwater: i64,
    pub edit_revision: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptCursor {
    pub meeting: MeetingId,
    pub range: TimeRange,
    pub snapshot: Snapshot,
    pub last_start_ms: i64,
    pub last_id: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptQuery {
    pub meeting: MeetingId,
    pub range: TimeRange,
    /// Reuse across export windows. Edits/deletion require restarting the export.
    pub snapshot: Option<Snapshot>,
    pub cursor: Option<TranscriptCursor>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub meeting: MeetingId,
    pub id: i64,
    pub source_id: String,
    pub sequence: i64,
    pub range: TimeRange,
    pub ingest_sequence: i64,
    pub revision: i64,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPage {
    pub snapshot: Snapshot,
    pub segments: Vec<Segment>,
    pub next: Option<TranscriptCursor>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotesStatus {
    Absent,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingSummary {
    pub meeting: MeetingId,
    pub title: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub timezone: String,
    pub provenance: Provenance,
    pub segment_count: i64,
    pub gap_count: i64,
    pub edit_revision: i64,
    pub notes: NotesStatus,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingStatus {
    pub summary: MeetingSummary,
    pub sources: Vec<Source>,
    pub gaps: Vec<Gap>,
    pub snapshot: Snapshot,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListFilter {
    /// Half-open Unix-ms start-date filter; CLI owns explicit timezone interpretation.
    pub since_ms: i64,
    pub until_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListCursor {
    pub store_id: String,
    pub filter: ListFilter,
    pub snapshot: Snapshot,
    pub last_id: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingPage {
    pub meetings: Vec<MeetingSummary>,
    pub next: Option<ListCursor>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCursor {
    pub store_id: String,
    pub query: String,
    pub snapshot: Snapshot,
    pub last_id: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPage {
    pub segments: Vec<Segment>,
    /// May be present on an empty page: short queries scan only 256 candidates/call.
    pub next: Option<SearchCursor>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteStatus {
    PhysicallyCleaned,
    PhysicalCleanupPending,
}
