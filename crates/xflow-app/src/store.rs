use anyhow::Result;
use rusqlite::{params, Connection, OpenFlags};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use xflow_core::ipc::HistoryEntry;

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    enabled: bool,
    limit: usize,
}
impl Store {
    pub fn open(path: &Path, enabled: bool, limit: usize) -> Result<Self> {
        let connection = if enabled {
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
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS history (id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL DEFAULT (unixepoch()), text TEXT NOT NULL, provider TEXT NOT NULL); PRAGMA user_version=1;")?;
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
            enabled,
            limit,
        };
        if enabled {
            store.prune_sync()?;
        }
        Ok(store)
    }
    fn prune_sync(&self) -> Result<()> {
        self.connection.lock().unwrap().execute("DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY id DESC LIMIT ?1)", [self.limit as i64])?;
        Ok(())
    }
    pub async fn append(&self, text: &str, provider: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let this = self.clone();
        let text = text.to_string();
        let provider = provider.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut conn = this.connection.lock().unwrap();
            let tx = conn.transaction()?;
            tx.execute("INSERT INTO history (text, provider) VALUES (?1, ?2)", params![text, provider])?;
            tx.execute("DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY id DESC LIMIT ?1)", [this.limit as i64])?;
            tx.commit()?; Ok(())
        }).await?
    }
    pub async fn history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        if !self.enabled {
            return Ok(vec![]);
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<HistoryEntry>> {
            let conn = this.connection.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT id, created_at, text, provider FROM history ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit.min(50) as i64], |row| {
                Ok(HistoryEntry {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    text: row.get(2)?,
                    provider: row.get(3)?,
                    ..HistoryEntry::default()
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?
    }
    pub async fn clear(&self) -> Result<()> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = this.connection.lock().unwrap();
            conn.execute("DELETE FROM history", [])?;
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn retention_and_disabled_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = Store::open(&path, true, 2).unwrap();
        for text in ["a", "b", "c"] {
            store.append(text, "test").await.unwrap();
        }
        let rows = store.history(10).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["c", "b"]
        );
        store.clear().await.unwrap();
        assert!(store.history(10).await.unwrap().is_empty());
        let disabled_path = dir.path().join("disabled.db");
        let disabled = Store::open(&disabled_path, false, 2).unwrap();
        disabled.append("secret", "test").await.unwrap();
        assert!(disabled.history(10).await.unwrap().is_empty());
        assert!(!disabled_path.exists());
    }
}
