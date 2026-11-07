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
/// v2: audio journal, job ledger and segment provenance. Every row cascades
/// with its meeting, so a deleted meeting leaves nothing to resurrect.
const SCHEMA_V2: &[&str] = &[
    "CREATE TABLE captures (meeting_id INTEGER PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE, state TEXT NOT NULL, created_ms INTEGER NOT NULL, ended_ms INTEGER, expected_ms INTEGER, reason TEXT, queued INTEGER NOT NULL DEFAULT 0)",
    "CREATE TABLE capture_sources (meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE, idx INTEGER NOT NULL, source_id TEXT NOT NULL, next_seq INTEGER NOT NULL DEFAULT 0, next_sample INTEGER NOT NULL DEFAULT 0, finished INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(meeting_id,idx))",
    "CREATE TABLE pieces (id INTEGER PRIMARY KEY AUTOINCREMENT, meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE, source_idx INTEGER NOT NULL, seq INTEGER NOT NULL, start_sample INTEGER NOT NULL, samples INTEGER NOT NULL, bytes INTEGER NOT NULL, hash TEXT NOT NULL, state TEXT NOT NULL, created_ms INTEGER NOT NULL, UNIQUE(meeting_id,source_idx,seq))",
    "CREATE TABLE jobs (id INTEGER PRIMARY KEY AUTOINCREMENT, meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE, source_idx INTEGER NOT NULL, win INTEGER NOT NULL, overlap_sample INTEGER NOT NULL, start_sample INTEGER NOT NULL, end_sample INTEGER NOT NULL, state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, budget INTEGER NOT NULL DEFAULT 3, not_before_ms INTEGER NOT NULL DEFAULT 0, error TEXT, UNIQUE(meeting_id,source_idx,win))",
    "CREATE TABLE attempts (id INTEGER PRIMARY KEY AUTOINCREMENT, job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE, provider TEXT NOT NULL, state TEXT NOT NULL, started_ms INTEGER NOT NULL, error TEXT)",
    "CREATE TABLE segment_provenance (segment_id INTEGER PRIMARY KEY REFERENCES segments(id) ON DELETE CASCADE, job_id INTEGER NOT NULL, attempt_id INTEGER NOT NULL, timing TEXT NOT NULL, dedup TEXT NOT NULL, trimmed TEXT, language TEXT)",
    "CREATE TABLE worker (singleton INTEGER PRIMARY KEY CHECK(singleton=1), pid INTEGER NOT NULL, started_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL, state TEXT NOT NULL, error TEXT)",
    "CREATE INDEX job_state ON jobs(meeting_id,state,start_sample)",
    "CREATE INDEX attempt_job ON attempts(job_id)",
    "CREATE INDEX provenance_job ON segment_provenance(job_id)",
];
const SCHEMA_V3: &[&str] = &[
    "CREATE TABLE notes (meeting_id INTEGER PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE, snapshot TEXT NOT NULL, json TEXT NOT NULL)",
    "CREATE TABLE meeting_terms (term TEXT PRIMARY KEY, replacement TEXT NOT NULL)",
];
pub(crate) const SCHEMA_VERSION: i64 = 3;
/// v1 indexed case-sensitively. v2 folds case one character at a time
/// (SQLite's Unicode table) and keeps accents, so `oauth` finds `OAuth` and
/// `nino` does not find `niño`.
const FTS_V1: &str = "CREATE VIRTUAL TABLE segment_fts USING fts5(text, content='segments', content_rowid='id', tokenize='trigram case_sensitive 1')";
const FTS_V2: &str = "CREATE VIRTUAL TABLE segment_fts USING fts5(text, content='segments', content_rowid='id', tokenize='trigram case_sensitive 0')";

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
        self.import_json_in(bytes).map_err(explain_busy)
    }
    fn import_json_in(&self, bytes: &[u8]) -> Result<ImportResult> {
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
        self.correct_segment_in(meeting, id, revision, text)
            .map_err(explain_busy)
    }
    fn correct_segment_in(
        &self,
        meeting: &MeetingId,
        id: i64,
        revision: i64,
        text: &str,
    ) -> Result<()> {
        if !self.path.exists() {
            return Err(ContractError::NotFound.into());
        }
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
        self.delete_in(meeting).map_err(explain_busy)
    }
    fn delete_in(&self, meeting: &MeetingId) -> Result<DeleteStatus> {
        if !self.path.exists() {
            return Err(ContractError::NotFound.into());
        }
        let mut db = open_writer(&self.path)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_meeting(&tx, meeting)?;
        // Jobs, attempts and pieces cascade, so no worker or recovery can
        // commit text or re-import audio for this meeting afterwards.
        tx.execute("DELETE FROM meetings WHERE id=?1", [meeting.id])?;
        tx.execute(
            "UPDATE metadata SET edit_revision=edit_revision+1,cleanup_pending=1",
            [],
        )?;
        tx.commit()?;
        let audio = super::journal::remove_deleted_audio(&self.path, &meeting.store_id, &db);
        let status = checkpoint(&db)?;
        Ok(if audio.is_err() {
            DeleteStatus::PhysicalCleanupPending
        } else {
            status
        })
    }

    /// One bounded attempt; caller schedules any further retry. No sleeping loop.
    pub fn retry_cleanup(&self) -> Result<DeleteStatus> {
        self.retry_cleanup_in().map_err(explain_busy)
    }
    fn retry_cleanup_in(&self) -> Result<DeleteStatus> {
        if !self.path.exists() {
            return Ok(DeleteStatus::PhysicallyCleaned);
        }
        let db = open_writer(&self.path)?;
        let audio = super::journal::remove_deleted_audio(&self.path, &store_id(&db)?, &db);
        let status = checkpoint(&db)?;
        Ok(if audio.is_err() {
            DeleteStatus::PhysicalCleanupPending
        } else {
            status
        })
    }

    /// File modes while a connection is open. Sidecars may disappear after it closes.
    #[doc(hidden)]
    pub fn private_modes(&self, write: bool) -> Result<Vec<(String, u32)>> {
        let _db = if write {
            open_writer(&self.path)?
        } else {
            open_reader(&self.path)?
        };
        let mut found = Vec::new();
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let side = PathBuf::from(format!("{}{suffix}", self.path.display()));
            if let Ok(meta) = std::fs::symlink_metadata(&side) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    found.push((suffix.to_string(), meta.permissions().mode() & 0o777));
                }
            }
        }
        Ok(found)
    }

    /// Core `secure_delete` on this writer connection, and the persisted FTS flag.
    #[doc(hidden)]
    pub fn deletion_flags(&self) -> Result<(i64, i64)> {
        let db = open_writer(&self.path)?;
        let core: i64 = db.query_row("PRAGMA secure_delete", [], |r| r.get(0))?;
        let fts: i64 = db.query_row(
            "SELECT v FROM segment_fts_config WHERE k='secure-delete'",
            [],
            |r| r.get(0),
        )?;
        Ok((core, fts))
    }
}

fn checkpoint(db: &Connection) -> Result<DeleteStatus> {
    let pending: bool = db.query_row("SELECT cleanup_pending FROM metadata", [], |r| r.get(0))?;
    if !pending {
        return Ok(DeleteStatus::PhysicallyCleaned);
    }
    let revision: i64 = db.query_row("SELECT edit_revision FROM metadata", [], |r| r.get(0))?;
    // A reader blocks the checkpoint, not the next writer. Do not spend the
    // writer's busy timeout waiting for that reader.
    db.busy_timeout(Duration::ZERO)?;
    let busy = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        r.get::<_, i64>(0)
    });
    db.busy_timeout(WRITER_BUSY)?;
    let busy = busy?;
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

pub(crate) fn invalidate(db: &Connection, meeting: i64) -> Result<()> {
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

pub(crate) fn deadline(db: &mut Connection) -> Result<()> {
    let end = Instant::now() + Duration::from_millis(200);
    db.progress_handler(1000, Some(move || Instant::now() >= end))?;
    Ok(())
}

pub(crate) fn open_reader(path: &Path) -> Result<Connection> {
    let path = private_path(path, false)?;
    let mut db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // WAL readers only wait on locks while another connection recovers the
    // WAL after a crash or rebuilds the shared index; give that the writers'
    // wait, not a fixed failure for status, the TUI and MCP.
    db.busy_timeout(WRITER_BUSY)?;
    db.execute_batch("PRAGMA query_only=ON;")?;
    validate_schema(&db, false)?;
    // A read can create a group-readable SHM under a loose umask.
    private_sidecars(&path, true)?;
    // The query budget starts once the connection is ready, so a lock wait
    // above never spends it.
    deadline(&mut db)?;
    Ok(db)
}
/// Wait this long for another writer, then fail. Schema creation holds the same lock.
const WRITER_BUSY: Duration = Duration::from_secs(1);

fn explain_busy(error: anyhow::Error) -> anyhow::Error {
    let busy = error.chain().any(|cause| {
        cause.downcast_ref::<rusqlite::Error>().is_some_and(|err| {
            matches!(
                err,
                rusqlite::Error::SqliteFailure(code, _)
                    if matches!(
                        code.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                    )
            )
        })
    });
    if busy {
        anyhow::anyhow!("Meeting database is busy; retry the command")
    } else {
        error
    }
}

pub(crate) fn open_writer(path: &Path) -> Result<Connection> {
    let path = private_path(path, true)?;
    let mut db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(WRITER_BUSY)?;
    ensure!(
        rusqlite::version_number() >= 3_051_003,
        "meeting storage requires SQLite >=3.51.3"
    );
    {
        // One immediate transaction creates the schema or refuses a bad file
        // before any writer PRAGMA can rewrite it.
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version = validate_schema(&tx, true)?;
        if version == 0 {
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
        }
        // v1 -> v2 in the same transaction: a crash or error leaves v1 as it was.
        if version < 2 {
            // Rebuild the index from the stored text, which is never touched.
            // Secure-delete is on, so the old index pages are zeroed.
            tx.execute_batch("DROP TABLE segment_fts;")?;
            tx.execute_batch(FTS_V2)?;
            tx.execute(
                "INSERT INTO segment_fts(segment_fts,rank) VALUES ('secure-delete',1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO segment_fts(segment_fts) VALUES ('rebuild')",
                [],
            )?;
            for ddl in SCHEMA_V2 {
                tx.execute_batch(ddl)?;
            }
            tx.execute_batch("PRAGMA user_version=2;")?;
        }
        if version < 3 {
            for ddl in SCHEMA_V3 {
                tx.execute_batch(ddl)?;
            }
            tx.execute_batch("PRAGMA user_version=3;")?;
        }
        tx.commit()?;
    }
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON; PRAGMA synchronous=FULL; PRAGMA journal_mode=WAL;")?;
    // Persistent FTS protection is required, never silently repaired on open.
    let secure: i64 = db.query_row(
        "SELECT v FROM segment_fts_config WHERE k='secure-delete'",
        [],
        |r| r.get(0),
    )?;
    ensure!(secure == 1, ContractError::MalformedStore);
    // SQLite creates WAL/SHM with the process umask; the writer tightens them.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    private_sidecars(&path, true)?;
    // Import validation/transaction work can take longer than a read's budget.
    db.progress_handler(0, None::<fn() -> bool>)?;
    Ok(db)
}

/// Returns the verified version; 0 is an empty file the writer may initialize.
fn validate_schema(db: &Connection, allow_empty: bool) -> Result<i64> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(
        version <= SCHEMA_VERSION,
        ContractError::UnsupportedSchema(version)
    );
    if version == 0 && allow_empty {
        let count: i64 = db.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
        ensure!(count == 0, ContractError::MalformedStore);
        return Ok(0);
    }
    ensure!(version >= 1, ContractError::MalformedStore);
    let (fts, v2): (&str, &[&str]) = if version >= 2 {
        (FTS_V2, SCHEMA_V2)
    } else {
        (FTS_V1, &[])
    };
    let common = SCHEMA.iter().filter(|ddl| **ddl != FTS_V1);
    for ddl in common
        .chain([&fts])
        .chain(v2)
        .chain(if version >= 3 { SCHEMA_V3 } else { &[] })
    {
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
    Ok(version)
}
/// Pipeline tables exist (v2). A v1 file stays readable without them.
pub(crate) fn has_pipeline(db: &Connection) -> Result<bool> {
    Ok(db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? >= 2)
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
    private_sidecars(&path, false)?;
    Ok(path)
}
fn private_sidecars(path: &Path, tighten: bool) -> Result<()> {
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
                    // APFS can report nlink 0 while another process's SQLite
                    // unlinks its journal: the name is already gone.
                    if meta.nlink() == 0 {
                        continue;
                    }
                    ensure!(
                        meta.uid() == unsafe { libc::geteuid() } && meta.nlink() == 1,
                        "unsafe meeting sidecar permissions"
                    );
                    if meta.permissions().mode() & 0o077 != 0 {
                        ensure!(tighten, "unsafe meeting sidecar permissions");
                        std::fs::set_permissions(&side, std::fs::Permissions::from_mode(0o600))?;
                    }
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
/// shortcut: 4600 JSON bytes so one max segment plus a search cursor fits in
/// PAGE_BYTES; raise only together with the worst-case page test.
const MAX_TEXT_JSON_BYTES: usize = 4600;

pub(crate) fn valid_text(text: &str) -> Result<()> {
    ensure!(
        bounded(text, MAX_TEXT_BYTES) && serde_json::to_vec(text)?.len() <= MAX_TEXT_JSON_BYTES,
        ContractError::InvalidImport("text exceeds 4096 UTF-8 bytes / 4600 JSON bytes or is empty")
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

#[cfg(test)]
mod migration_tests {
    use super::*;

    /// An M1 (v1) store exactly as that release created it.
    fn v1(dir: &Path, blocker: bool) -> PathBuf {
        let path = dir.join("meetings.db");
        let db = Connection::open(&path).unwrap();
        for ddl in SCHEMA {
            db.execute_batch(ddl).unwrap();
        }
        db.execute_batch("INSERT INTO metadata(singleton,store_id) VALUES (1,'00112233445566778899aabbccddeeff');
            INSERT INTO segment_fts(segment_fts,rank) VALUES ('secure-delete',1);
            INSERT INTO meetings(import_id,import_hash,import_json,started_at_ms,ended_at_ms,timezone) VALUES ('m1','h',x'7b7d',0,10,'Z');
            INSERT INTO sources VALUES (1,'mic','{\"id\":\"mic\",\"label\":null,\"epoch\":0,\"provider\":null,\"model\":null}');
            INSERT INTO segments(meeting_id,source_id,sequence,start_ms,end_ms,text) VALUES (1,'mic',0,0,5,'Sign in with OAuth, niño');
            INSERT INTO revisions VALUES (1,1,'Sign in with OAuth, niño');
            PRAGMA user_version=1;").unwrap();
        if blocker {
            // Makes the v2 step fail halfway, after the FTS rebuild.
            db.execute_batch("CREATE TABLE jobs(x);").unwrap();
        }
        drop(db);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn state(path: &Path) -> (i64, String, String, i64) {
        let db = Connection::open(path).unwrap();
        (
            db.query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            db.query_row(
                "SELECT sql FROM sqlite_schema WHERE name='segment_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
            db.query_row("SELECT group_concat(text,'|') FROM segments", [], |r| {
                r.get(0)
            })
            .unwrap(),
            db.query_row(
                "SELECT count(*) FROM segment_fts WHERE segment_fts MATCH '\"oauth\"'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        )
    }

    #[test]
    fn v1_rebuilds_its_index_case_insensitively_and_keeps_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = v1(dir.path(), false);
        assert_eq!(state(&path).3, 0, "v1 is case-sensitive");
        drop(open_writer(&path).unwrap());
        let (version, fts, text, hits) = state(&path);
        assert_eq!((version, fts.as_str(), hits), (3, FTS_V2, 1));
        assert_eq!(text, "Sign in with OAuth, niño");
        let db = Connection::open(&path).unwrap();
        let secure: i64 = db
            .query_row(
                "SELECT v FROM segment_fts_config WHERE k='secure-delete'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(secure, 1);
        let revisions: String = db
            .query_row("SELECT text FROM revisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(revisions, "Sign in with OAuth, niño");
        // Reopening is a no-op.
        drop(open_writer(&path).unwrap());
        assert_eq!(state(&path), (3, FTS_V2.into(), text, 1));
    }

    #[test]
    fn failed_migration_leaves_v1_and_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let path = v1(dir.path(), true);
        let before = state(&path);
        assert!(open_writer(&path).is_err());
        assert_eq!(
            state(&path),
            before,
            "rolled back: v1 index and text intact"
        );
        Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TABLE jobs;")
            .unwrap();
        drop(open_writer(&path).unwrap());
        assert_eq!(state(&path).0, 3);
        assert_eq!(state(&path).3, 1);
    }
}
