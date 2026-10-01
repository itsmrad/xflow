//! Comment-preserving, validated, atomic edits of config.toml shared by the CLI and TUI.
//! Every mutation is checked by deserializing the whole document into `Config`;
//! an invalid edit leaves the document untouched.
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table};
use xflow_core::config::Config;

const TEMPLATE: &str = include_str!("../../../config/default.toml");

pub struct ConfigFile {
    path: PathBuf,
    doc: DocumentMut,
}

impl ConfigFile {
    /// The user's file, or the documented template when it does not exist yet.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => TEMPLATE.to_owned(),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        let doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("{} is not valid TOML", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            doc,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The validated configuration this document describes.
    pub fn config(&self) -> Result<Config> {
        parse(&self.doc)
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

    /// Atomically replace the file (temp file + rename), creating a private
    /// config directory on first save. A symlinked config is written through.
    pub fn save(&self) -> Result<()> {
        let target = match std::fs::canonicalize(&self.path) {
            Ok(real) => real,
            Err(_) => {
                let parent = self.path.parent().context("config path has no parent")?;
                crate::paths::private_dir(parent)?;
                self.path.clone()
            }
        };
        let parent = target.parent().context("config path has no parent")?;
        let temp = parent.join(format!(".xflow-config-{}.tmp", std::process::id()));
        let result = (|| -> Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                let mode = std::fs::metadata(&target)
                    .map(|metadata| metadata.permissions().mode() & 0o777)
                    .unwrap_or(0o600);
                options.mode(mode);
            }
            let mut file = options.open(&temp)?;
            file.write_all(self.doc.to_string().as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temp, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result.with_context(|| format!("cannot write {}", target.display()))
    }

    fn try_edit(
        &mut self,
        key: &str,
        edit: impl FnOnce(&mut DocumentMut) -> Result<()>,
    ) -> Result<()> {
        let mut doc = self.doc.clone();
        edit(&mut doc)?;
        parse(&doc).with_context(|| format!("invalid value for {key}"))?;
        self.doc = doc;
        Ok(())
    }
}

impl std::fmt::Display for ConfigFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.doc, formatter)
    }
}

fn parse(doc: &DocumentMut) -> Result<Config> {
    let config: Config = toml::from_str(&doc.to_string())?;
    config.validate()?;
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
    fn edits_preserve_comments_validate_and_save_atomically() {
        let (_dir, path) = temp_config(TEMPLATE);
        let mut file = ConfigFile::load(&path).unwrap();
        file.set("stt.model", "whisper-large-v3").unwrap();
        file.set("recording.auto_stop_secs", "5").unwrap();
        file.set("sounds.enabled", "false").unwrap();
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
            Some("120")
        );
        assert!(file
            .list_effective()
            .unwrap()
            .iter()
            .any(|(key, value)| key == "stt.provider" && value == r#""groq""#));
    }

    #[test]
    fn missing_file_starts_from_template_and_new_tables_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xflow/config.toml");
        let mut file = ConfigFile::load(&path).unwrap();
        file.set("ui.themes.mine.bg", "#f7f6f3").unwrap();
        file.save().unwrap();
        let config = ConfigFile::load(&path).unwrap().config().unwrap();
        assert_eq!(config.ui.themes["mine"]["bg"], "#f7f6f3");
    }
}
