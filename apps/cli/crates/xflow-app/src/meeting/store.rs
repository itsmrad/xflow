use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use xflow_core::meeting::*;

#[derive(Debug, PartialEq, Eq)]
pub enum ContractError {
    InvalidImport(&'static str),
    InvalidQuery(&'static str),
    StaleCursor,
    NotFound,
    ImportConflict,
    UnsupportedSchema(i64),
    MalformedStore,
}
impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "meeting storage: {self:?}")
    }
}
impl std::error::Error for ContractError {}

// Exact DDL is checked before any writer PRAGMA, including triggers and indexes.
const SCHEMA: &[&str] = &[
    "CREATE TABLE metadata (singleton INTEGER PRIMARY KEY CHECK(singleton=1), store_id TEXT NOT NULL, edit_revision INTEGER NOT NULL DEFAULT 0, cleanup_pending INTEGER NOT NULL DEFAULT 0)",
    "CREATE TABLE meetings (id INTEGER PRIMARY KEY AUTOINCREMENT, import_id TEXT NOT NULL UNIQUE, import_hash TEXT NOT NULL, import_json BLOB NOT NULL, title TEXT, started_at_ms INTEGER NOT NULL, ended_at_ms INTEGER NOT NULL, timezone TEXT NOT NULL, edit_revision INTEGER NOT NULL DEFAULT 0)",
    "CREATE TABLE sources (meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE, id TEXT NOT NULL, json TEXT NOT NULL, PRIMARY KEY(meeting_id,id))",
    "CREATE TABLE gaps (id INTEGER PRIMARY KEY AUTOINCREMENT, meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE, source_id TEXT NOT NULL, json TEXT NOT NULL, FOREIGN KEY(meeting_id,source_id) REFERENCES sources(meeting_id,id))",
    "CREATE TABLE segments (id INTEGER PRIMARY KEY AUTOINCREMENT, meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE, source_id TEXT NOT NULL, sequence INTEGER NOT NULL, start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL, revision INTEGER NOT NULL DEFAULT 1, text TEXT NOT NULL, UNIQUE(meeting_id,source_id,sequence), FOREIGN KEY(meeting_id,source_id) REFERENCES sources(meeting_id,id))",
    "CREATE INDEX segment_time ON segments(meeting_id,start_ms,id)",
    "CREATE TABLE revisions (segment_id INTEGER NOT NULL REFERENCES segments(id) ON DELETE CASCADE, revision INTEGER NOT NULL, text TEXT NOT NULL, PRIMARY KEY(segment_id,revision))",
    "CREATE VIRTUAL TABLE segment_fts USING fts5(text, content='segments', content_rowid='id', tokenize='trigram case_sensitive 1')",
    "CREATE TRIGGER segment_insert AFTER INSERT ON segments BEGIN INSERT INTO segment_fts(rowid,text) VALUES (new.id,new.text); END",
    "CREATE TRIGGER segment_delete AFTER DELETE ON segments BEGIN INSERT INTO segment_fts(segment_fts,rowid,text) VALUES ('delete',old.id,old.text); END",
    "CREATE TRIGGER segment_update AFTER UPDATE OF text ON segments BEGIN INSERT INTO segment_fts(segment_fts,rowid,text) VALUES ('delete',old.id,old.text); INSERT INTO segment_fts(rowid,text) VALUES (new.id,new.text); END",
];

#[derive(Clone, Debug)]
pub struct MeetingStore {
    pub(crate) path: PathBuf,
}
impl MeetingStore {
    /// No I/O. Only explicit operations open the separate meeting database.
    pub fn new() -> Result<Self> {
        Ok(Self::at(crate::paths::data_dir()?.join("meetings.db")))
    }
    /// Owned private data directory only. Intended also for isolated scratch tests.
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn reader(&self) -> super::MeetingReader {
        super::MeetingReader::at(self.path.clone())
    }

    /// Read at most MAX_IMPORT_BYTES+1 before calling this method. Validation
    /// happens before opening/creating the database, including on identity replay.
    pub fn import_json(&self, bytes: &[u8]) -> Result<ImportResult> {
        let input = validate_import(bytes)?;
        let canonical = serde_json::to_vec(&input)?;
        let hash = format!("fnv1a64:{:016x}", xflow_core::stable_hash(&canonical));
        let mut db = open_writer(&self.path)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let store_id = store_id(&tx)?;
        let old: Option<(i64, String, Vec<u8>)> = tx
            .query_row(
                "SELECT id,import_hash,import_json FROM meetings WHERE import_id=?1",
                [&input.import_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((id, old_hash, old_input)) = old {
            // Exact equality closes the noncryptographic hash's collision boundary.
            ensure!(
                old_hash == hash && old_input == canonical,
                ContractError::ImportConflict
            );
            return Ok(ImportResult {
                meeting: MeetingId { store_id, id },
                already_present: true,
            });
        }
        tx.execute("INSERT INTO meetings(import_id,import_hash,import_json,title,started_at_ms,ended_at_ms,timezone) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![input.import_id,hash,canonical,input.title,input.started_at_ms,input.ended_at_ms,input.timezone])?;
        let id = tx.last_insert_rowid();
        for source in &input.sources {
            tx.execute(
                "INSERT INTO sources(meeting_id,id,json) VALUES (?1,?2,?3)",
                params![id, source.id, serde_json::to_string(source)?],
            )?;
        }
        for gap in &input.gaps {
            tx.execute(
                "INSERT INTO gaps(meeting_id,source_id,json) VALUES (?1,?2,?3)",
                params![id, gap.source_id, serde_json::to_string(gap)?],
            )?;
        }
        for segment in &input.segments {
            insert_segment(&tx, id, segment)?;
        }
        tx.commit()?;
        Ok(ImportResult {
            meeting: MeetingId { store_id, id },
            already_present: false,
        })
    }

    /// Existing evidence changes invalidate every in-progress read/export. This
    /// storage seam does not expose a CLI correction command in M1.
    pub fn correct_segment(
        &self,
        meeting: &MeetingId,
        id: i64,
        revision: i64,
        text: &str,
    ) -> Result<()> {
        valid_text(text)?;
        let mut db = open_writer(&self.path)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_meeting(&tx, meeting)?;
        let changed = tx.execute("UPDATE segments SET text=?1,revision=revision+1 WHERE meeting_id=?2 AND id=?3 AND revision=?4", params![text,meeting.id,id,revision])?;
        ensure!(changed == 1, ContractError::StaleCursor);
        tx.execute(
            "INSERT INTO revisions SELECT id,revision,text FROM segments WHERE id=?1",
            [id],
        )?;
        invalidate(&tx, meeting.id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete(&self, meeting: &MeetingId) -> Result<DeleteStatus> {
        let mut db = open_writer(&self.path)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_meeting(&tx, meeting)?;
        tx.execute("DELETE FROM meetings WHERE id=?1", [meeting.id])?;
        tx.execute(
            "UPDATE metadata SET edit_revision=edit_revision+1,cleanup_pending=1",
            [],
        )?;
        tx.commit()?;
        checkpoint(&db)
    }

    /// One bounded attempt; caller schedules any further retry. No sleeping loop.
    pub fn retry_cleanup(&self) -> Result<DeleteStatus> {
        checkpoint(&open_writer(&self.path)?)
    }
}

fn checkpoint(db: &Connection) -> Result<DeleteStatus> {
    let pending: bool = db.query_row("SELECT cleanup_pending FROM metadata", [], |r| r.get(0))?;
    if !pending {
        return Ok(DeleteStatus::PhysicallyCleaned);
    }
    let revision: i64 = db.query_row("SELECT edit_revision FROM metadata", [], |r| r.get(0))?;
    let busy: i64 = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    if busy != 0 {
        return Ok(DeleteStatus::PhysicalCleanupPending);
    }
    // Never clear a concurrent delete's cleanup request. This write is noncontent.
    let cleared = db.execute(
        "UPDATE metadata SET cleanup_pending=0 WHERE edit_revision=?1",
        [revision],
    )?;
    Ok(if cleared == 1 {
        DeleteStatus::PhysicallyCleaned
    } else {
        DeleteStatus::PhysicalCleanupPending
    })
}

fn invalidate(db: &Connection, meeting: i64) -> Result<()> {
    db.execute(
        "UPDATE meetings SET edit_revision=edit_revision+1 WHERE id=?1",
        [meeting],
    )?;
    db.execute("UPDATE metadata SET edit_revision=edit_revision+1", [])?;
    Ok(())
}
fn insert_segment(db: &Connection, meeting: i64, segment: &ImportSegment) -> Result<()> {
    db.execute("INSERT INTO segments(meeting_id,source_id,sequence,start_ms,end_ms,text) VALUES (?1,?2,?3,?4,?5,?6)",
        params![meeting,segment.source_id,segment.sequence,segment.range.start_ms,segment.range.end_ms,segment.text])?;
    db.execute(
        "INSERT INTO revisions(segment_id,revision,text) VALUES (?1,1,?2)",
        params![db.last_insert_rowid(), segment.text],
    )?;
    Ok(())
}

pub(crate) fn store_id(db: &Connection) -> Result<String> {
    Ok(
        db.query_row("SELECT store_id FROM metadata WHERE singleton=1", [], |r| {
            r.get(0)
        })?,
    )
}
pub(crate) fn check_meeting(db: &Connection, meeting: &MeetingId) -> Result<()> {
    ensure!(
        meeting.id > 0 && store_id(db)? == meeting.store_id,
        ContractError::NotFound
    );
    ensure!(
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM meetings WHERE id=?1)",
            [meeting.id],
            |r| r.get::<_, bool>(0)
        )?,
        ContractError::NotFound
    );
    Ok(())
}
pub(crate) fn highwater(db: &Connection, table: &str) -> Result<i64> {
    Ok(db.query_row(
        "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name=?1),0)",
        [table],
        |r| r.get(0),
    )?)
}

pub(crate) fn deadline(db: &Connection) {
    let end = Instant::now() + Duration::from_millis(200);
    db.progress_handler(1000, Some(move || Instant::now() >= end));
}

pub(crate) fn open_reader(path: &Path) -> Result<Connection> {
    let path = private_path(path, false)?;
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(50))?;
    db.execute_batch("PRAGMA query_only=ON;")?;
    deadline(&db);
    validate_schema(&db, false)?;
    Ok(db)
}
fn open_writer(path: &Path) -> Result<Connection> {
    let path = private_path(path, true)?;
    let mut db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(50))?;
    deadline(&db);
    let fresh = validate_schema(&db, true)?;
    ensure!(
        rusqlite::version_number() >= 3_051_003,
        "meeting storage requires SQLite >=3.51.3"
    );
    // No writer PRAGMA precedes schema refusal.
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON; PRAGMA synchronous=FULL; PRAGMA journal_mode=WAL;")?;
    if fresh {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A competing initializer may have committed since our preflight.
        if validate_schema(&tx, true)? {
            for ddl in SCHEMA {
                tx.execute_batch(ddl)?;
            }
            tx.execute(
                "INSERT INTO metadata(singleton,store_id) VALUES (1,lower(hex(randomblob(16))))",
                [],
            )?;
            tx.execute(
                "INSERT INTO segment_fts(segment_fts,rank) VALUES ('secure-delete',1)",
                [],
            )?;
            tx.execute_batch("PRAGMA user_version=1;")?;
        }
        tx.commit()?;
    }
    // Persistent FTS protection is required, never silently repaired on open.
    let secure: i64 = db.query_row(
        "SELECT v FROM segment_fts_config WHERE k='secure-delete'",
        [],
        |r| r.get(0),
    )?;
    ensure!(secure == 1, ContractError::MalformedStore);
    private_sidecars(&path)?;
    // Import validation/transaction work can take longer than a read's budget.
    db.progress_handler(0, None::<fn() -> bool>);
    Ok(db)
}

fn validate_schema(db: &Connection, allow_empty: bool) -> Result<bool> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(version <= 1, ContractError::UnsupportedSchema(version));
    if version == 0 && allow_empty {
        let count: i64 = db.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
        ensure!(count == 0, ContractError::MalformedStore);
        return Ok(true);
    }
    ensure!(version == 1, ContractError::MalformedStore);
    for ddl in SCHEMA {
        let actual: Option<String> = db
            .query_row("SELECT sql FROM sqlite_schema WHERE sql=?1", [*ddl], |r| {
                r.get(0)
            })
            .optional()?;
        ensure!(actual.is_some(), ContractError::MalformedStore);
    }
    let unexpected: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type IN ('trigger','view')",
        [],
        |r| r.get(0),
    )?;
    ensure!(unexpected == 3, ContractError::MalformedStore);
    let (identity, revision, pending): (String, i64, i64) = db.query_row(
        "SELECT store_id,edit_revision,cleanup_pending FROM metadata WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        identity.len() == 32
            && identity.bytes().all(|b| b.is_ascii_hexdigit())
            && revision >= 0
            && (0..=1).contains(&pending),
        ContractError::MalformedStore
    );
    let secure: Option<i64> = db
        .query_row(
            "SELECT v FROM segment_fts_config WHERE k='secure-delete'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(secure == Some(1), ContractError::MalformedStore);
    Ok(false)
}

fn private_path(path: &Path, create: bool) -> Result<PathBuf> {
    let parent = path
        .parent()
        .context("meeting database needs an owned parent")?;
    ensure!(
        path.file_name() == Some(std::ffi::OsStr::new("meetings.db")),
        "meeting database must be named meetings.db"
    );
    if create {
        crate::paths::private_dir(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::symlink_metadata(parent)?;
        ensure!(
            meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.permissions().mode() & 0o077 == 0,
            "unsafe meeting directory"
        );
    }
    let parent = parent.canonicalize()?;
    let path = parent.join("meetings.db");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(create)
            .create(create)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.nlink() == 1
                && meta.permissions().mode() & 0o077 == 0,
            "unsafe meeting database file"
        );
    }
    #[cfg(not(unix))]
    if create {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
    }
    private_sidecars(&path)?;
    Ok(path)
}
fn private_sidecars(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let side = PathBuf::from(format!("{}{suffix}", path.display()));
        match std::fs::symlink_metadata(&side) {
            Ok(meta) => {
                ensure!(
                    meta.is_file() && !meta.file_type().is_symlink(),
                    "unsafe meeting sidecar"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{MetadataExt, PermissionsExt};
                    ensure!(
                        meta.uid() == unsafe { libc::geteuid() }
                            && meta.nlink() == 1
                            && meta.permissions().mode() & 0o077 == 0,
                        "unsafe meeting sidecar permissions"
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max
}
fn optional(value: &Option<String>, max: usize) -> bool {
    value.as_ref().is_none_or(|s| bounded(s, max))
}
pub(crate) fn valid_text(text: &str) -> Result<()> {
    ensure!(
        bounded(text, MAX_TEXT_BYTES) && serde_json::to_vec(text)?.len() <= 5000,
        ContractError::InvalidImport("text exceeds 4096 UTF-8 bytes / 5000 JSON bytes or is empty")
    );
    Ok(())
}
fn validate_import(bytes: &[u8]) -> Result<MeetingImport> {
    ensure!(
        bytes.len() <= MAX_IMPORT_BYTES,
        ContractError::InvalidImport("import exceeds 8 MiB")
    );
    let input: MeetingImport = serde_json::from_slice(bytes)
        .map_err(|_| ContractError::InvalidImport("invalid versioned JSON contract"))?;
    ensure!(
        input.schema_version == IMPORT_VERSION,
        ContractError::InvalidImport("unsupported import schema")
    );
    ensure!(
        bounded(&input.import_id, 128)
            && optional(&input.title, 256)
            && bounded(&input.timezone, 64),
        ContractError::InvalidImport("invalid identity/title/timezone")
    );
    ensure!(
        input.started_at_ms >= 0 && input.ended_at_ms > input.started_at_ms,
        ContractError::InvalidImport("invalid meeting times")
    );
    ensure!(
        !input.sources.is_empty()
            && input.sources.len() <= 8
            && !input.segments.is_empty()
            && input.segments.len() <= MAX_SEGMENTS
            && input.gaps.len() <= 32,
        ContractError::InvalidImport("source/segment/gap count exceeds bounds")
    );
    let mut sources = HashSet::new();
    for s in &input.sources {
        ensure!(
            bounded(&s.id, 64)
                && optional(&s.label, 128)
                && optional(&s.provider, 128)
                && optional(&s.model, 128)
                && sources.insert(s.id.as_str()),
            ContractError::InvalidImport("invalid or duplicate source")
        );
    }
    ensure!(
        serde_json::to_vec(&(&input.title, &input.timezone, &input.sources, &input.gaps))?.len()
            <= 4500,
        ContractError::InvalidImport("source/gap metadata exceeds 4500 JSON bytes")
    );
    let duration = input.ended_at_ms - input.started_at_ms;
    let range_ok = |r: TimeRange| r.start_ms >= 0 && r.end_ms > r.start_ms && r.end_ms <= duration;
    let mut identities = HashSet::new();
    for s in &input.segments {
        ensure!(
            sources.contains(s.source_id.as_str())
                && s.sequence >= 0
                && identities.insert((&s.source_id, s.sequence))
                && range_ok(s.range)
                && s.range.end_ms - s.range.start_ms <= MAX_SEGMENT_MS,
            ContractError::InvalidImport(
                "invalid source/sequence/range or span exceeds 60 seconds"
            )
        );
        valid_text(&s.text)?;
    }
    for gap in &input.gaps {
        ensure!(
            sources.contains(gap.source_id.as_str())
                && range_ok(gap.range)
                && bounded(&gap.reason, 128),
            ContractError::InvalidImport("invalid gap")
        );
    }
    Ok(input)
}
