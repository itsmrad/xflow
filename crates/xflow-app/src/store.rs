use anyhow::{bail, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use xflow_core::{
    ipc::{HistoryEntry, Stats},
    Mode,
};

const SCHEMA_VERSION: i64 = 2;
/// Largest history page; the daemon additionally trims pages to one IPC frame.
pub const MAX_PAGE: usize = 200;
const COLUMNS: &str =
    "id, created_at, text, provider, model, raw_text, app_id, language, duration_ms, latency_ms, mode";

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    path: PathBuf,
    enabled: bool,
    limit: usize,
}
impl Store {
    pub fn open(path: &Path, enabled: bool, limit: usize) -> Result<Self> {
        let mut connection = if enabled {
            if let Some(parent) = path.parent() {
                crate::paths::private_dir(parent)?;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
            Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?
        } else {
            Connection::open_in_memory()?
        };
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA secure_delete=ON;")?;
        migrate(&mut connection)?;
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
            path: path.to_owned(),
            enabled,
            limit,
        };
        if enabled {
            prune(&store.connection.lock().unwrap(), limit)?;
        }
        Ok(store)
    }
    /// The same database file with new privacy settings (config reload).
    pub fn reopen(&self, enabled: bool, limit: usize) -> Result<Self> {
        Self::open(&self.path, enabled, limit)
    }
    pub fn settings(&self) -> (bool, usize) {
        (self.enabled, self.limit)
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || work(&mut connection.lock().unwrap())).await?
    }
    /// Stores a finished session and returns it with its id and timestamp, or
    /// `None` when history is disabled. `id`/`created_at` of the input are ignored.
    pub async fn append(&self, entry: HistoryEntry) -> Result<Option<HistoryEntry>> {
        self.append_guarded(entry, None).await
    }
    pub(crate) async fn append_current(
        &self,
        entry: HistoryEntry,
        epoch: Arc<AtomicU64>,
        generation: u64,
    ) -> Result<Option<HistoryEntry>> {
        self.append_guarded(entry, Some((epoch, generation))).await
    }
    async fn append_guarded(
        &self,
        entry: HistoryEntry,
        guard: Option<(Arc<AtomicU64>, u64)>,
    ) -> Result<Option<HistoryEntry>> {
        if !self.enabled {
            return Ok(None);
        }
        let limit = self.limit;
        self.run(move |conn| {
            if guard.is_some_and(|(epoch, generation)| epoch.load(Ordering::SeqCst) != generation) { return Ok(None); }
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO history (text, provider, model, raw_text, app_id, language, duration_ms, latency_ms, mode, words) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    entry.text,
                    entry.provider,
                    entry.model,
                    entry.raw_text,
                    entry.app_id,
                    entry.language,
                    entry.duration_ms.map(clamp_i64),
                    entry.latency_ms.map(clamp_i64),
                    mode_name(entry.mode),
                    words(&entry.text),
                ],
            )?;
            let id = tx.last_insert_rowid();
            let stored = tx.query_row(
                &format!("SELECT {COLUMNS} FROM history WHERE id = ?1"),
                [id],
                entry_from_row,
            )?;
            prune(&tx, limit)?;
            tx.commit()?;
            // A zero history_limit keeps nothing.
            Ok((limit > 0).then_some(stored))
        })
        .await
    }
    /// Newest first. `query` is a case-insensitive (ASCII) substring of the text.
    /// Returns the page and the number of matching rows.
    pub async fn history(
        &self,
        limit: usize,
        offset: usize,
        query: Option<String>,
    ) -> Result<(Vec<HistoryEntry>, u64)> {
        if !self.enabled {
            return Ok((vec![], 0));
        }
        // ponytail: SQLite LIKE folds ASCII case only; Unicode folding needs a
        // registered collation/function (rusqlite "functions" feature).
        let pattern = query
            .filter(|query| !query.is_empty())
            .map(|query| format!("%{}%", escape_like(&query)));
        self.run(move |conn| {
            let filter = if pattern.is_some() {
                "WHERE text LIKE ?1 ESCAPE '\\'"
            } else {
                "WHERE ?1 IS NULL"
            };
            let total: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM history {filter}"),
                [&pattern],
                |row| row.get(0),
            )?;
            let mut statement = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM history {filter} ORDER BY id DESC LIMIT ?2 OFFSET ?3"
            ))?;
            let rows = statement.query_map(
                params![
                    pattern,
                    limit.min(MAX_PAGE) as i64,
                    offset.min(i64::MAX as usize) as i64
                ],
                entry_from_row,
            )?;
            Ok((rows.collect::<rusqlite::Result<_>>()?, total as u64))
        })
        .await
    }
    pub async fn get(&self, id: i64) -> Result<Option<HistoryEntry>> {
        if !self.enabled {
            return Ok(None);
        }
        self.run(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM history WHERE id = ?1"),
                    [id],
                    entry_from_row,
                )
                .optional()?)
        })
        .await
    }
    /// Returns whether a row was deleted.
    pub async fn delete(&self, id: i64) -> Result<bool> {
        if !self.enabled {
            return Ok(false);
        }
        self.run(move |conn| Ok(conn.execute("DELETE FROM history WHERE id = ?1", [id])? > 0))
            .await
    }
    pub async fn set_latency(&self, id: i64, latency_ms: u64) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "UPDATE history SET latency_ms = ?2 WHERE id = ?1",
                params![id, clamp_i64(latency_ms)],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn clear(&self) -> Result<()> {
        self.run(|conn| {
            conn.execute("DELETE FROM history", [])?;
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .await
    }
    pub async fn stats(&self) -> Result<Stats> {
        if !self.enabled {
            return Ok(Stats::default());
        }
        self.run(|conn| {
            let mut stats = Stats::default();
            let timed_words: i64;
            (stats.sessions, stats.words, stats.audio_ms, timed_words) = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(words), 0), COALESCE(SUM(duration_ms), 0), \
                 COALESCE(SUM(CASE WHEN duration_ms > 0 THEN words END), 0) FROM history",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u64,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, i64>(2)? as u64,
                        row.get(3)?,
                    ))
                },
            )?;
            // Local calendar days (SQLite's 'localtime' honours TZ and /etc/localtime).
            (stats.sessions_today, stats.words_today) = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(words), 0) FROM history \
                 WHERE created_at >= CAST(strftime('%s', 'now', 'localtime', 'start of day', 'utc') AS INTEGER)",
                [],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
            )?;
            let today: i64 = conn.query_row(
                "SELECT CAST(julianday('now', 'localtime') + 0.5 AS INTEGER)",
                [],
                |row| row.get(0),
            )?;
            let mut days = conn.prepare(
                "SELECT DISTINCT CAST(julianday(created_at, 'unixepoch', 'localtime') + 0.5 AS INTEGER) AS day \
                 FROM history ORDER BY day DESC",
            )?;
            let mut days = days.query_map([], |row| row.get::<_, i64>(0))?;
            // The streak may end yesterday: today's first dictation has not happened yet.
            let mut expected = match days.next().transpose()? {
                Some(day) if day == today || day == today - 1 => day,
                _ => today + 2,
            };
            if expected <= today {
                stats.streak_days = 1;
                for day in days {
                    expected -= 1;
                    if day? != expected {
                        break;
                    }
                    stats.streak_days += 1;
                }
            }
            if stats.audio_ms > 0 {
                stats.wpm = Some(timed_words as f64 * 60_000.0 / stats.audio_ms as f64);
            }
            let timed: i64 = conn.query_row(
                "SELECT COUNT(*) FROM history WHERE latency_ms IS NOT NULL",
                [],
                |row| row.get(0),
            )?;
            // Nearest-rank percentiles.
            let percentile = |p: f64| -> Result<Option<u64>> {
                if timed == 0 {
                    return Ok(None);
                }
                let rank = ((p * timed as f64).ceil() as i64).clamp(1, timed);
                Ok(Some(conn.query_row(
                    "SELECT latency_ms FROM history WHERE latency_ms IS NOT NULL \
                     ORDER BY latency_ms LIMIT 1 OFFSET ?1",
                    [rank - 1],
                    |row| row.get::<_, i64>(0),
                )? as u64))
            };
            stats.latency_p50_ms = percentile(0.5)?;
            stats.latency_p95_ms = percentile(0.95)?;
            Ok(stats)
        })
        .await
    }
}

/// Schema v1 (MVP) → v2 in one transaction. Unknown newer schemas are refused
/// rather than pruned or rewritten by an older daemon.
fn migrate(connection: &mut Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("history database schema {version} is newer than this xflow; upgrade xflow or move the database aside");
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = connection.transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS history (id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL DEFAULT (unixepoch()), text TEXT NOT NULL, provider TEXT NOT NULL);
         ALTER TABLE history ADD COLUMN model TEXT;
         ALTER TABLE history ADD COLUMN raw_text TEXT;
         ALTER TABLE history ADD COLUMN app_id TEXT;
         ALTER TABLE history ADD COLUMN language TEXT;
         ALTER TABLE history ADD COLUMN duration_ms INTEGER;
         ALTER TABLE history ADD COLUMN latency_ms INTEGER;
         ALTER TABLE history ADD COLUMN mode TEXT NOT NULL DEFAULT 'dictation';
         ALTER TABLE history ADD COLUMN words INTEGER NOT NULL DEFAULT 0;
         CREATE INDEX IF NOT EXISTS history_created_at ON history (created_at);
         CREATE INDEX IF NOT EXISTS history_latency ON history (latency_ms);",
    )?;
    {
        let mut select = tx.prepare("SELECT id, text FROM history")?;
        let mut update = tx.prepare("UPDATE history SET words = ?2 WHERE id = ?1")?;
        let rows = select.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, text) = row?;
            update.execute(params![id, words(&text)])?;
        }
    }
    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    tx.commit()?;
    Ok(())
}

fn prune(connection: &Connection, limit: usize) -> Result<()> {
    connection.execute(
        "DELETE FROM history WHERE id <= (SELECT id FROM history ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [limit.min(i64::MAX as usize) as i64],
    )?;
    Ok(())
}

fn entry_from_row(row: &Row) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get(0)?,
        created_at: row.get(1)?,
        text: row.get(2)?,
        provider: row.get(3)?,
        model: row.get(4)?,
        raw_text: row.get(5)?,
        app_id: row.get(6)?,
        language: row.get(7)?,
        duration_ms: row.get::<_, Option<i64>>(8)?.map(|v| v.max(0) as u64),
        latency_ms: row.get::<_, Option<i64>>(9)?.map(|v| v.max(0) as u64),
        mode: match row.get_ref(10)?.as_str()? {
            "command" => Mode::Command,
            _ => Mode::Dictation,
        },
    })
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Dictation => "dictation",
        Mode::Command => "command",
    }
}

fn clamp_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

/// ponytail: whitespace-separated words; scripts without spaces (CJK) count
/// one word per run. A Unicode word segmenter is the upgrade path.
pub fn words(text: &str) -> i64 {
    text.split_whitespace().count() as i64
}

fn escape_like(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for c in query.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> HistoryEntry {
        HistoryEntry {
            text: text.into(),
            provider: "test".into(),
            ..HistoryEntry::default()
        }
    }

    #[tokio::test]
    async fn retention_and_disabled_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = Store::open(&path, true, 2).unwrap();
        for text in ["a", "b", "c"] {
            store.append(entry(text)).await.unwrap();
        }
        let (rows, total) = store.history(10, 0, None).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["c", "b"]
        );
        assert_eq!(total, 2);
        store.clear().await.unwrap();
        assert!(store.history(10, 0, None).await.unwrap().0.is_empty());
        let disabled_path = dir.path().join("disabled.db");
        let disabled = Store::open(&disabled_path, false, 2).unwrap();
        assert!(disabled.append(entry("secret")).await.unwrap().is_none());
        assert!(disabled.history(10, 0, None).await.unwrap().0.is_empty());
        assert_eq!(disabled.stats().await.unwrap(), Stats::default());
        assert!(!disabled_path.exists());
        let zero = Store::open(&dir.path().join("zero.db"), true, 0).unwrap();
        assert!(zero.append(entry("gone")).await.unwrap().is_none());
        assert_eq!(zero.history(10, 0, None).await.unwrap().1, 0);
    }

    #[tokio::test]
    async fn database_and_wal_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/history.db");
        let store = Store::open(&path, true, 5).unwrap();
        store.append(entry("private")).await.unwrap();
        for file in ["history.db", "history.db-wal"] {
            let mode = std::fs::metadata(dir.path().join("data").join(file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{file} must be owner-only");
        }
    }

    #[tokio::test]
    async fn round_trips_v2_fields_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        let full = HistoryEntry {
            text: "Fix the bug".into(),
            provider: "groq".into(),
            model: Some("whisper".into()),
            raw_text: Some("fix the bug um".into()),
            app_id: Some("code".into()),
            language: Some("en".into()),
            duration_ms: Some(1500),
            latency_ms: Some(420),
            mode: Mode::Command,
            ..HistoryEntry::default()
        };
        let stored = store.append(full.clone()).await.unwrap().unwrap();
        assert!(stored.id > 0 && stored.created_at > 0);
        assert_eq!(
            HistoryEntry {
                id: 0,
                created_at: 0,
                ..stored.clone()
            },
            full
        );
        assert_eq!(store.get(stored.id).await.unwrap(), Some(stored.clone()));
        let reopened = store.reopen(true, 10).unwrap();
        assert_eq!(reopened.get(stored.id).await.unwrap(), Some(stored.clone()));
        assert!(reopened.delete(stored.id).await.unwrap());
        assert!(!reopened.delete(stored.id).await.unwrap());
        assert_eq!(reopened.get(stored.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn search_escapes_wildcards_and_pages() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("h.db"), true, 100).unwrap();
        for text in [
            "100% done",
            "100 percent",
            "snake_case",
            "snakeXcase",
            "Hello World",
            "hello again",
            r"C:\path",
        ] {
            store.append(entry(text)).await.unwrap();
        }
        let texts = |rows: Vec<HistoryEntry>| rows.into_iter().map(|r| r.text).collect::<Vec<_>>();
        let search = |q: &str| store.history(10, 0, Some(q.into()));
        assert_eq!(texts(search("0%").await.unwrap().0), ["100% done"]);
        assert_eq!(texts(search("e_c").await.unwrap().0), ["snake_case"]);
        assert_eq!(texts(search(r"\p").await.unwrap().0), [r"C:\path"]);
        let (rows, total) = search("HELLO").await.unwrap();
        assert_eq!(
            (texts(rows), total),
            (vec!["hello again".into(), "Hello World".into()], 2)
        );
        assert_eq!(search("").await.unwrap().1, 7);
        let (page, total) = store.history(2, 2, None).await.unwrap();
        assert_eq!(
            (texts(page), total),
            (vec!["Hello World".into(), "snakeXcase".into()], 7)
        );
        let (page, total) = store.history(2, 1, Some("hello".into())).await.unwrap();
        assert_eq!((texts(page), total), (vec!["Hello World".into()], 2));
        assert!(store
            .history(5, usize::MAX, None)
            .await
            .unwrap()
            .0
            .is_empty());
    }

    #[tokio::test]
    async fn migrates_v1_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        {
            let v1 = Connection::open(&path).unwrap();
            v1.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS history (id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL DEFAULT (unixepoch()), text TEXT NOT NULL, provider TEXT NOT NULL); PRAGMA user_version=1;
                 INSERT INTO history (text, provider) VALUES ('old dictation here', 'groq'), ('second one', 'openai');",
            )
            .unwrap();
        }
        let store = Store::open(&path, true, 500).unwrap();
        let (rows, total) = store.history(10, 0, None).await.unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows[0].text, "second one");
        assert_eq!(rows[0].mode, Mode::Dictation);
        assert_eq!(rows[1].provider, "groq");
        assert!(rows[1].model.is_none() && rows[1].latency_ms.is_none());
        let stats = store.stats().await.unwrap();
        assert_eq!((stats.sessions, stats.words), (2, 5));
        store.append(entry("new")).await.unwrap();
        drop(store);
        let version: i64 = Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
        // Reopening an already-migrated database is a no-op.
        assert_eq!(
            Store::open(&path, true, 500)
                .unwrap()
                .history(10, 0, None)
                .await
                .unwrap()
                .1,
            3
        );
        // A database from a newer xflow is refused, not rewritten.
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version=3;")
            .unwrap();
        assert!(Store::open(&path, true, 500).is_err());
    }

    #[tokio::test]
    async fn stats_streak_wpm_and_latency_percentiles() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("h.db"), true, 1000).unwrap();
        assert_eq!(store.stats().await.unwrap(), Stats::default());
        // Local noon `days_ago` days back, so DST shifts cannot move a row across midnight.
        let insert = |days_ago: i64, text: &str, duration: Option<i64>, latency: Option<i64>| {
            store.connection.lock().unwrap().execute(
                "INSERT INTO history (created_at, text, provider, duration_ms, latency_ms, words) VALUES \
                 (CAST(strftime('%s', 'now', 'localtime', 'start of day', ?1, '+12 hours', 'utc') AS INTEGER), ?2, 'p', ?3, ?4, ?5)",
                params![format!("-{days_ago} days"), text, duration, latency, words(text)],
            ).unwrap();
        };
        // Yesterday, 2 and 3 days ago, then a gap: streak 3 ending yesterday.
        insert(1, "one two three four", Some(2_000), Some(100));
        insert(2, "five six", Some(1_000), Some(200));
        insert(3, "seven", None, Some(300));
        insert(5, "eight nine", Some(1_000), None);
        let stats = store.stats().await.unwrap();
        assert_eq!(stats.sessions, 4);
        assert_eq!(stats.words, 9);
        assert_eq!(stats.audio_ms, 4_000);
        assert_eq!((stats.sessions_today, stats.words_today), (0, 0));
        assert_eq!(stats.streak_days, 3);
        // 8 timed words over 4 s of audio.
        assert_eq!(stats.wpm, Some(120.0));
        assert_eq!(stats.latency_p50_ms, Some(200));
        assert_eq!(stats.latency_p95_ms, Some(300));
        insert(0, "today words", Some(1_000), Some(50));
        let stats = store.stats().await.unwrap();
        assert_eq!((stats.sessions_today, stats.words_today), (1, 2));
        assert_eq!(stats.streak_days, 4);
        assert_eq!(stats.latency_p50_ms, Some(100));
        // A streak that ended two days ago is broken.
        let old = Store::open(&dir.path().join("old.db"), true, 10).unwrap();
        old.connection.lock().unwrap().execute(
            "INSERT INTO history (created_at, text, provider) VALUES (unixepoch() - 3 * 86400, 'x', 'p')", [],
        ).unwrap();
        assert_eq!(old.stats().await.unwrap().streak_days, 0);
    }
}
