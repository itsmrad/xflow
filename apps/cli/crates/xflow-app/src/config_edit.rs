//! Comment-preserving, validated, atomic edits of config.toml shared by the CLI and TUI.
//! Every mutation is checked by deserializing the whole document into `Config`;
//! an invalid edit leaves the document untouched.
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use toml_edit::{DocumentMut, Item, Table};
use xflow_core::config::Config;

const TEMPLATE: &str = include_str!("../../../../../config/default.toml");

pub struct ConfigFile {
    path: PathBuf,
    doc: DocumentMut,
}

/// The file a config path names: a symlink's target, or, before the file
/// exists, the path under its resolved directory.
pub fn target(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_owned()),
        _ => path.to_owned(),
    }
}

/// Exclusive advisory lock on `<config target>.lock`. Every cooperative writer
/// (CLI, TUI, the daemon's learning and undo) holds it from load to save, so no
/// edit overwrites another; it is named after the symlink-resolved file, so a
/// symlinked path and its target share it. Release it before asking the daemon
/// to reload.
pub fn lock(path: &Path) -> Result<std::fs::File> {
    let file = lock_file(path)?;
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}

/// `lock` that gives up after `wait`: the daemon never blocks behind a stopped
/// CLI or editor, it fails closed instead.
pub fn lock_within(path: &Path, wait: Duration) -> Result<std::fs::File> {
    let file = lock_file(path)?;
    let deadline = Instant::now() + wait;
    while fs2::FileExt::try_lock_exclusive(&file).is_err() {
        if Instant::now() >= deadline {
            bail!("config.toml is locked by another xflow command; nothing was changed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(file)
}

fn lock_file(path: &Path) -> Result<std::fs::File> {
    let parent = path.parent().context("config path has no parent")?;
    if !parent.exists() {
        crate::paths::private_dir(parent)?;
    }
    let mut name = target(path).into_os_string();
    name.push(".lock");
    Ok(std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(name)?)
}

/// The most config.toml bytes kept in memory to undo one automatic edit; a
/// larger file is not edited automatically.
pub const MAX_UNDO_BYTES: usize = 256 * 1024;

/// Which file a save wrote and what it held: device, inode, size, modification
/// time and a stable hash of the bytes, all from one open descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision {
    dev: u64,
    ino: u64,
    len: u64,
    mtime_ns: i128,
    hash: u64,
}
impl Revision {
    fn of(file: &std::fs::File, bytes: &[u8]) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata()?;
        Ok(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
            hash: xflow_core::stable_hash(bytes),
        })
    }
    /// The file at `path` now, bytes and metadata through one descriptor (the
    /// metadata after the read, so an in-place write during it shows); None
    /// when it is missing.
    fn read(path: &Path) -> Result<Option<Self>> {
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        (&mut file)
            .take(2 * MAX_UNDO_BYTES as u64)
            .read_to_end(&mut bytes)?;
        Self::of(&file, &bytes).map(Some)
    }
}

/// One automatic edit of config.toml that can be undone once: the exact bytes
/// before it (None: there was no file) and what it wrote. Kept in daemon memory
/// only.
#[derive(Clone, Debug)]
pub struct Undo {
    target: PathBuf,
    before: Option<Vec<u8>>,
    after: Revision,
}
impl Undo {
    pub fn new(path: &Path, before: Option<Vec<u8>>, after: Revision) -> Self {
        Self {
            target: target(path),
            before,
            after,
        }
    }
    /// Put config.toml back exactly as it was before the edit, atomically. The
    /// caller holds `lock`. Refuses, writing nothing, unless `path` still names
    /// the same file and it is still byte for byte the same file the edit wrote:
    /// any later save, even of identical bytes (a manual `dictionary add` of the
    /// same word), replaces the file, and an in-place edit changes its time.
    /// Returns the configuration now on disk.
    pub fn restore(&self, path: &Path) -> Result<Config> {
        if target(path) != self.target || Revision::read(&self.target)? != Some(self.after) {
            bail!("config.toml changed after that automatic edit; nothing was undone");
        }
        let Some(before) = &self.before else {
            std::fs::remove_file(&self.target)?;
            return ConfigFile::load(path)?.config();
        };
        let text = std::str::from_utf8(before).context("config.toml was not UTF-8")?;
        let config = parse(&text.parse::<DocumentMut>()?)?;
        let parent = self.target.parent().context("config path has no parent")?;
        written(parent, before)?
            .persist(&self.target)
            .map_err(|error| error.error)
            .with_context(|| format!("cannot replace {}", self.target.display()))?;
        Ok(config)
    }
}

/// A private, synced sibling temp file holding `bytes`, ready to rename.
fn written(parent: &Path, bytes: &[u8]) -> Result<tempfile::NamedTempFile> {
    let mut temp = tempfile::Builder::new()
        .prefix(".xflow-config-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("cannot create a temporary file in {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temp.as_file_mut().write_all(bytes)?;
    temp.as_file().sync_all()?;
    Ok(temp)
}

impl ConfigFile {
    /// The user's file, or the documented template when it does not exist yet.
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_exact(path).map(|(file, _)| file)
    }

    /// `load`, plus the file's exact bytes (None when it does not exist).
    pub fn load_exact(path: &Path) -> Result<(Self, Option<Vec<u8>>)> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        let text = match &bytes {
            Some(bytes) => std::str::from_utf8(bytes)
                .with_context(|| format!("cannot read {}", path.display()))?,
            None => TEMPLATE,
        };
        let doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("{} is not valid TOML", path.display()))?;
        Ok((
            Self {
                path: path.to_owned(),
                doc,
            },
            bytes,
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Validate routing and fields without requiring backup credentials for reads.
    pub fn config(&self) -> Result<Config> {
        parse(&self.doc)
    }

    /// Explicit validation and saves also require the backup key.
    pub fn validate(&self) -> Result<Config> {
        let config = self.config()?;
        xflow_providers::validate_fallback(&config.stt, config.privacy.offline)?;
        Ok(config)
    }

    /// The value written in the file at a dotted key (`stt.model`), as TOML text.
    pub fn get(&self, key: &str) -> Option<String> {
        let mut item = self.doc.as_item();
        for part in key.split('.') {
            item = item.get(part)?;
        }
        Some(match item {
            Item::Value(value) => value.clone().decorated("", "").to_string(),
            other => other.to_string().trim().to_owned(),
        })
    }

    /// The effective value at a dotted key, defaults included; `None` when unset.
    pub fn effective(&self, key: &str) -> Result<Option<String>> {
        let mut value = toml::Value::try_from(self.config()?)?;
        for part in key.split('.') {
            match value.get(part) {
                Some(next) => value = next.clone(),
                None => return Ok(None),
            }
        }
        Ok(Some(render(&value)))
    }

    /// Every effective `key = value` (defaults included), sorted by key.
    pub fn list_effective(&self) -> Result<Vec<(String, String)>> {
        let mut rows = Vec::new();
        flatten("", &toml::Value::try_from(self.config()?)?, &mut rows);
        rows.sort();
        Ok(rows)
    }

    /// Set a dotted key. `raw` is read as a TOML value (`true`, `42`, `0.5`,
    /// `["a"]`, `"quoted"`), falling back to a plain string when that is what the
    /// schema accepts.
    pub fn set(&mut self, key: &str, raw: &str) -> Result<()> {
        let mut candidates = Vec::new();
        if let Ok(value) = raw.parse::<toml_edit::Value>() {
            candidates.push(value);
        }
        candidates.push(toml_edit::Value::from(raw));
        let mut first_error = None;
        for value in candidates {
            match self.try_edit(key, |doc| put(doc, key, Item::Value(value))) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or_else(|| anyhow!("invalid value")))
    }

    /// Replace a dotted key with any serializable value (lists, snippet tables, …).
    pub fn set_typed<T: Serialize>(&mut self, key: &str, value: &T) -> Result<()> {
        #[derive(Serialize)]
        struct Wrapper<'a, T> {
            v: &'a T,
        }
        let text = toml::to_string(&Wrapper { v: value })?;
        let mut parsed = text.parse::<DocumentMut>()?;
        let item = parsed.remove("v").unwrap_or(Item::None);
        self.try_edit(key, |doc| put(doc, key, item))
    }

    /// Remove a dotted key so its default applies. Returns whether it was present.
    pub fn unset(&mut self, key: &str) -> Result<bool> {
        let (parent, leaf) = split(key)?;
        let mut removed = false;
        self.try_edit(key, |doc| {
            let mut item = doc.as_item_mut();
            for part in parent {
                match item.get_mut(part) {
                    Some(next) => item = next,
                    None => return Ok(()),
                }
            }
            if let Some(table) = item.as_table_like_mut() {
                removed = table.remove(leaf).is_some();
            }
            Ok(())
        })?;
        Ok(removed)
    }

    /// Atomically replace the file (unique sibling temp file + rename),
    /// creating a private config directory on first save. A symlinked config
    /// is written through.
    pub fn save(&self) -> Result<()> {
        self.save_tracked().map(drop)
    }

    /// `save`, returning what it wrote (see `Undo`).
    pub fn save_tracked(&self) -> Result<Revision> {
        self.validate()?;
        let target = match std::fs::canonicalize(&self.path) {
            Ok(real) => real,
            Err(_) => {
                let parent = self.path.parent().context("config path has no parent")?;
                match std::fs::symlink_metadata(parent) {
                    Ok(_) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        crate::paths::private_dir(parent)?;
                    }
                    Err(error) => return Err(error.into()),
                }
                self.path.clone()
            }
        };
        let parent = target.parent().context("config path has no parent")?;
        let bytes = self.doc.to_string();
        let temp = written(parent, bytes.as_bytes())?;
        // Before the rename, which keeps the inode and modification time.
        let revision = Revision::of(temp.as_file(), bytes.as_bytes())?;
        temp.persist(&target)
            .map_err(|error| error.error)
            .with_context(|| format!("cannot replace {}", target.display()))?;
        Ok(revision)
    }

    fn try_edit(
        &mut self,
        key: &str,
        edit: impl FnOnce(&mut DocumentMut) -> Result<()>,
    ) -> Result<()> {
        let mut doc = self.doc.clone();
        edit(&mut doc)?;
        // Setting either control replaces the old selection in one validated edit;
        // clearing the set keeps a saved fixed language.
        if let Some(stt) = doc.get_mut("stt").and_then(Item::as_table_like_mut) {
            if key == "stt.languages"
                && stt
                    .get("languages")
                    .and_then(Item::as_array)
                    .is_some_and(|a| !a.is_empty())
            {
                stt.remove("language");
            } else if key == "stt.language" && stt.get("language").is_some() {
                stt.remove("languages");
            }
        }
        parse(&doc).with_context(|| format!("invalid value for {key}"))?;
        self.doc = doc;
        Ok(())
    }
}

/// Templates before the 20-minute limit wrote `recording.max_seconds = 120`, which
/// stopped every dictation after two minutes. Unpin that value once so the current
/// default applies; `marker` records the run so a 120 chosen later is kept.
/// Returns whether the file changed.
pub fn migrate_recording_limit(path: &Path, marker: &Path) -> Result<bool> {
    if marker.exists() {
        return Ok(false);
    }
    let mut changed = false;
    if path.exists() {
        let _lock = lock(path)?;
        let mut file = ConfigFile::load(path)?;
        if file.get("recording.max_seconds").as_deref() == Some("120") {
            changed = file.unset("recording.max_seconds")?;
            file.save()?;
        }
    }
    if let Some(parent) = marker.parent() {
        crate::paths::private_dir(parent)?;
    }
    std::fs::write(marker, b"")?;
    Ok(changed)
}

impl std::fmt::Display for ConfigFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.doc, formatter)
    }
}

fn parse(doc: &DocumentMut) -> Result<Config> {
    let config: Config = toml::from_str(&doc.to_string())?;
    config.validate()?;
    xflow_providers::validate_fallback_route(&config.stt, config.privacy.offline)?;
    Ok(config)
}

fn split(key: &str) -> Result<(Vec<&str>, &str)> {
    let mut parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        bail!("invalid key {key:?}; use dotted names like stt.model");
    }
    let leaf = parts.pop().context("empty key")?;
    Ok((parts, leaf))
}

fn put(doc: &mut DocumentMut, key: &str, item: Item) -> Result<()> {
    let (parent, leaf) = split(key)?;
    let mut table: &mut Table = doc.as_table_mut();
    for part in parent {
        let entry = table.entry(part).or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        });
        if let Some(inline) = entry.as_inline_table() {
            *entry = Item::Table(inline.clone().into_table());
        }
        table = entry
            .as_table_mut()
            .ok_or_else(|| anyhow!("{part} is not a table in {key}"))?;
    }
    match (table.get_mut(leaf), item) {
        // Keep the existing trailing comment and spacing when replacing a scalar.
        (Some(Item::Value(old)), Item::Value(mut new)) => {
            *new.decor_mut() = old.decor().clone();
            *old = new;
        }
        (_, item) => {
            table.insert(leaf, item);
        }
    }
    Ok(())
}

fn flatten(prefix: &str, value: &toml::Value, rows: &mut Vec<(String, String)>) {
    match value {
        toml::Value::Table(table) if !table.is_empty() => {
            for (key, value) in table {
                let key = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&key, value, rows);
            }
        }
        other => rows.push((prefix.to_owned(), render(other))),
    }
}

/// Config floats are f32; print them the way they were written (0.005, not 0.004999…).
fn render(value: &toml::Value) -> String {
    match value {
        toml::Value::Float(float) => (*float as f32).to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xflow_core::config::Snippet;

    fn temp_config(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn legacy_two_minute_limit_is_unpinned_once() {
        let (dir, path) = temp_config(
            "[recording]\nmax_seconds = 120\nmin_ms = 300 # keep\n[stt]\nprovider = \"groq\"\n",
        );
        let marker = dir.path().join("data/migrated");
        assert!(migrate_recording_limit(&path, &marker).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("max_seconds"), "{text}");
        assert!(text.contains("min_ms = 300 # keep"));
        assert_eq!(
            Config::load(&path).unwrap().recording.max_seconds,
            xflow_core::config::MAX_RECORDING_SECS
        );
        // A 120 chosen after the migration is the user's and stays.
        std::fs::write(&path, "[recording]\nmax_seconds = 120\n").unwrap();
        assert!(!migrate_recording_limit(&path, &marker).unwrap());
        assert!(std::fs::read_to_string(&path).unwrap().contains("120"));
        // Other values are never touched; a missing config still records the run.
        let (dir, path) = temp_config("[recording]\nmax_seconds = 300\n");
        assert!(!migrate_recording_limit(&path, &dir.path().join("m")).unwrap());
        assert!(std::fs::read_to_string(&path).unwrap().contains("300"));
        let missing = dir.path().join("none.toml");
        assert!(!migrate_recording_limit(&missing, &dir.path().join("m2")).unwrap());
        assert!(!missing.exists() && dir.path().join("m2").exists());
    }

    #[test]
    fn backup_validation_rejects_remote_offline_and_unknown_routes() {
        for text in [
            "[stt]\nfallback = [\"openai\"]\n[privacy]\noffline = true\n",
            "[stt]\nfallback = [\"missing-provider\"]\n",
            "[stt]\nfallback = [\"groq\"]\n",
            "[stt]\nfallback = [\"openai\", \"deepgram\"]\n",
        ] {
            let (_dir, path) = temp_config(text);
            let error = ConfigFile::load(&path).unwrap().config().unwrap_err();
            assert!(format!("{error:#}").contains("fallback"), "{error:#}");
        }
    }

    #[test]
    fn edits_preserve_comments_validate_and_save_atomically() {
        let (_dir, path) = temp_config(TEMPLATE);
        let mut file = ConfigFile::load(&path).unwrap();
        file.set("stt.model", "whisper-large-v3").unwrap();
        file.set("recording.auto_stop_secs", "5").unwrap();
        file.set("sounds.enabled", "false").unwrap();
        file.set("hotkey.key", "right_option").unwrap();
        assert!(file.set("hotkey.key", "not_a_key").is_err());
        file.set("dictionary.words", r#"["Postgres", "xflow"]"#)
            .unwrap();
        assert!(file.set("recording.auto_stop_secs", "999").is_err());
        assert!(file.set("stt.nonexistent", "1").is_err());
        file.set_typed(
            "snippets",
            &vec![Snippet {
                trigger: "my email".into(),
                text: "me@example.com".into(),
            }],
        )
        .unwrap();
        file.save().unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# `xflow providers` lists every provider"));
        let config = ConfigFile::load(&path).unwrap().config().unwrap();
        assert_eq!(config.stt.model.as_deref(), Some("whisper-large-v3"));
        assert_eq!(config.recording.auto_stop_secs, 5);
        assert!(!config.sounds.enabled);
        assert_eq!(config.hotkey.key, "right_option");
        assert_eq!(config.dictionary.words, ["Postgres", "xflow"]);
        assert_eq!(config.snippets[0].text, "me@example.com");

        let mut file = ConfigFile::load(&path).unwrap();
        assert_eq!(
            file.get("stt.model").as_deref(),
            Some(r#""whisper-large-v3""#)
        );
        assert!(file.unset("stt.model").unwrap());
        assert_eq!(file.get("stt.model"), None);
        assert_eq!(
            file.effective("recording.max_seconds").unwrap().as_deref(),
            Some("1200")
        );
        assert!(file
            .list_effective()
            .unwrap()
            .iter()
            .any(|(key, value)| key == "stt.provider" && value == r#""groq""#));
    }

    #[test]
    fn missing_file_starts_from_template_and_new_tables_are_created() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xflow/config.toml");
        let mut file = ConfigFile::load(&path).unwrap();
        file.set("ui.themes.mine.bg", "#f7f6f3").unwrap();
        file.save().unwrap();
        let config = ConfigFile::load(&path).unwrap().config().unwrap();
        assert_eq!(config.ui.themes["mine"]["bg"], "#f7f6f3");
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn saving_in_existing_directory_preserves_directory_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("user-selected");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = parent.join("config.toml");
        let mut file = ConfigFile::load(&path).unwrap();
        file.set("ui.theme", "midnight").unwrap();

        file.save().unwrap();

        assert_eq!(
            std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_replaces_permissive_config_with_private_file() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, path) = temp_config("[ui]\ntheme = 'paper'\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut file = ConfigFile::load(&path).unwrap();
        file.set("ui.theme", "midnight").unwrap();
        file.save().unwrap();

        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn save_does_not_remove_an_unowned_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let sentinel = dir
            .path()
            .join(format!(".xflow-config-{}.tmp", std::process::id()));
        std::fs::write(&path, "[ui]\ntheme = 'paper'\n").unwrap();
        std::fs::write(&sentinel, "owned by another save").unwrap();
        let mut file = ConfigFile::load(&path).unwrap();
        file.set("ui.theme", "midnight").unwrap();

        file.save().unwrap();

        assert_eq!(
            std::fs::read_to_string(sentinel).unwrap(),
            "owned by another save"
        );
        assert_eq!(
            ConfigFile::load(&path).unwrap().config().unwrap().ui.theme,
            "midnight"
        );
    }

    #[test]
    fn concurrent_saves_to_distinct_files_in_one_directory_succeed() {
        const SAVES: usize = 8;
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(SAVES);

        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..SAVES)
                .map(|index| {
                    let path = dir.path().join(format!("config-{index}.toml"));
                    std::fs::write(&path, "[ui]\ntheme = 'paper'\n").unwrap();
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let mut file = ConfigFile::load(&path).unwrap();
                        file.set("ui.theme", "midnight").unwrap();
                        barrier.wait();
                        file.save().unwrap();
                        assert_eq!(
                            ConfigFile::load(&path).unwrap().config().unwrap().ui.theme,
                            "midnight"
                        );
                    })
                })
                .collect();
            for worker in workers {
                worker.join().unwrap();
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_through_symlink_and_secures_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("actual.toml");
        let link = dir.path().join("config.toml");
        std::fs::write(&target, "[ui]\ntheme = 'paper'\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&target, &link).unwrap();
        let mut file = ConfigFile::load(&link).unwrap();
        file.set("ui.theme", "midnight").unwrap();

        file.save().unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            ConfigFile::load(&link).unwrap().config().unwrap().ui.theme,
            "midnight"
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(test)]
mod undo_tests {
    use super::*;

    const START: &str = "# mine\n[dictionary]\nwords = [\"Postgres\"]\n[ui]\ntheme = 'paper'\n";

    /// A daemon learn: locked load, one word added, tracked save.
    fn learn(path: &Path, word: &str) -> Undo {
        let _lock = lock(path).unwrap();
        let (mut file, before) = ConfigFile::load_exact(path).unwrap();
        let mut words = file.config().unwrap().dictionary.words;
        words.insert(0, word.into());
        file.set_typed("dictionary.words", &words).unwrap();
        let after = file.save_tracked().unwrap();
        Undo::new(path, before, after)
    }
    fn restore(undo: &Undo, path: &Path) -> Result<Config> {
        let _lock = lock(path).unwrap();
        undo.restore(path)
    }
    fn words(path: &Path) -> Vec<String> {
        Config::load(path).unwrap().dictionary.words
    }

    #[test]
    fn undo_restores_the_exact_bytes_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, START).unwrap();
        let undo = learn(&path, "claud");
        assert_eq!(words(&path), ["claud", "Postgres"]);
        let restored = restore(&undo, &path).unwrap();
        assert_eq!(restored.dictionary.words, ["Postgres"]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), START);
        // A replay finds a file the edit did not write.
        assert!(restore(&undo, &path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), START);
    }

    #[test]
    fn undo_refuses_after_any_later_save_and_writes_nothing() {
        type Later = fn(&Path);
        let later: [(&str, Later); 5] = [
            // `dictionary add claud` of a word already there saves identical bytes.
            ("identical bytes", |path| ConfigFile::load(path).unwrap().save().unwrap()),
            ("unrelated setting", |path| {
                let mut file = ConfigFile::load(path).unwrap();
                file.set("ui.theme", "midnight").unwrap();
                file.save().unwrap();
            }),
            ("remove then add again", |path| {
                let mut file = ConfigFile::load(path).unwrap();
                let bytes = file.to_string();
                file.set("dictionary.words", r#"["Postgres"]"#).unwrap();
                file.save().unwrap();
                std::fs::write(path.with_extension("next"), bytes).unwrap();
                std::fs::rename(path.with_extension("next"), path).unwrap();
            }),
            ("hand edit in place, same bytes", |path| {
                let bytes = std::fs::read(path).unwrap();
                let modified = std::fs::metadata(path).unwrap().modified().unwrap();
                std::fs::write(path, bytes).unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_modified(modified + std::time::Duration::from_secs(2))
                    .unwrap();
            }),
            ("another learn", |path| drop(learn(path, "Kubernetes"))),
        ];
        for (case, later) in later {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            std::fs::write(&path, START).unwrap();
            let undo = learn(&path, "claud");
            later(&path);
            let before = std::fs::read(&path).unwrap();
            assert!(restore(&undo, &path).is_err(), "{case}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{case}");
            assert!(words(&path).contains(&"claud".to_owned()), "{case}");
        }
    }

    #[test]
    fn undo_of_a_first_save_removes_the_file_it_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xflow/config.toml");
        let undo = learn(&path, "claud");
        assert!(path.exists());
        assert_eq!(restore(&undo, &path).unwrap().dictionary.words, Vec::<String>::new());
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn undo_and_lock_follow_a_symlinked_config_to_its_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("actual.toml");
        let link = dir.path().join("config.toml");
        std::fs::write(&target, START).unwrap();
        symlink(&target, &link).unwrap();
        // One lock for both names.
        let held = lock(&link).unwrap();
        assert!(lock_within(&target, Duration::from_millis(50)).is_err());
        drop(held);
        drop(lock_within(&target, Duration::from_millis(50)).unwrap());
        let undo = learn(&link, "claud");
        restore(&undo, &link).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), START);
        // The link now names another file: not the file the edit wrote.
        let undo = learn(&link, "claud");
        let other = dir.path().join("other.toml");
        std::fs::copy(&target, &other).unwrap();
        std::fs::remove_file(&link).unwrap();
        symlink(&other, &link).unwrap();
        assert!(restore(&undo, &link).is_err());
    }

    #[test]
    fn a_held_lock_makes_a_bounded_wait_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let held = lock(&path).unwrap();
        let started = Instant::now();
        assert!(lock_within(&path, Duration::from_millis(100)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(held);
        assert!(lock_within(&path, Duration::from_millis(100)).is_ok());
    }
}

#[cfg(test)]
mod preferred_language_tests {
    use super::*;
    #[test]
    fn preferred_languages_save_is_atomic_and_preserves_comments_and_legacy_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# keep me\n[stt]\nlanguage = 'fr'\n").unwrap();
        let mut file = ConfigFile::load(&path).unwrap();
        file.set("stt.languages", r#"["hi", "en"]"#).unwrap();
        assert!(file.config().unwrap().stt.language.is_none());
        file.save().unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("# keep me"));
        let old = file.to_string();
        assert!(file.set("stt.provider", "mistral").is_err());
        assert!(file.set("stt.languages", r#"["hi", "hi-IN"]"#).is_err());
        assert_eq!(file.to_string(), old);
        file.set("stt.language", "fr").unwrap();
        assert!(file.config().unwrap().stt.languages.is_empty());
        file.set("stt.languages", "[]").unwrap();
        assert_eq!(file.config().unwrap().stt.language.as_deref(), Some("fr"));
        file.save().unwrap();
        assert_eq!(
            Config::load(&path).unwrap().stt.language.as_deref(),
            Some("fr")
        );
    }
}
