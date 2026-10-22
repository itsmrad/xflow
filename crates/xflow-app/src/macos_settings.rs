//! macOS appearance preferences, separate from the strict core TOML schema.
use crate::gsettings::{Kind, Setting};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::PathBuf,
};

pub struct GSettings {
    path: PathBuf,
}

fn defaults() -> Vec<Setting> {
    [
        ("enabled", "true", Kind::Bool, "Show the recording pill"),
        (
            "position",
            "'bottom-center'",
            Kind::Choice(
                [
                    "bottom-center",
                    "top-center",
                    "bottom-left",
                    "bottom-right",
                    "top-left",
                    "top-right",
                    "top",
                    "bottom",
                ]
                .map(str::to_owned)
                .to_vec(),
            ),
            "Pill position",
        ),
        (
            "opacity",
            "0.97",
            Kind::Double { min: 0.2, max: 1.0 },
            "Pill opacity",
        ),
        (
            "scale",
            "1.0",
            Kind::Double { min: 0.5, max: 3.0 },
            "Pill size",
        ),
    ]
    .into_iter()
    .map(|(key, value, kind, summary)| Setting {
        key: key.into(),
        value: value.into(),
        kind,
        summary: summary.into(),
    })
    .collect()
}

fn normalize(key: &str, value: &str) -> Result<String> {
    let setting = defaults()
        .into_iter()
        .find(|s| s.key == key)
        .with_context(|| format!("unsupported macOS overlay setting: {key}"))?;
    let value = value.trim();
    match setting.kind {
        Kind::Bool if matches!(value, "true" | "false") => Ok(value.into()),
        Kind::Double { min, max } => {
            let number: f64 = value.parse().context("expected a number")?;
            if !number.is_finite() || !(min..=max).contains(&number) {
                bail!("{key} must be between {min} and {max}");
            }
            Ok(number.to_string())
        }
        Kind::Choice(choices) => {
            let word = value
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .unwrap_or(value);
            if !choices.iter().any(|choice| choice == word) {
                bail!("{key} must be one of {}", choices.join(", "));
            }
            Ok(format!("'{word}'"))
        }
        _ => bail!("invalid value for {key}"),
    }
}

impl GSettings {
    pub fn detect() -> Result<Self> {
        Ok(Self {
            path: crate::paths::config_path()?.with_file_name("overlay.json"),
        })
    }

    fn read(&self) -> Result<BTreeMap<String, String>> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new())
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(8193).read_to_end(&mut bytes)?;
        if bytes.len() > 8192 {
            bail!("macOS overlay settings exceed 8 KiB");
        }
        let values: BTreeMap<String, String> =
            serde_json::from_slice(&bytes).context("invalid macOS overlay settings")?;
        for (key, value) in &values {
            normalize(key, value)?;
        }
        Ok(values)
    }

    pub fn list(&self) -> Result<Vec<Setting>> {
        let values = self.read()?;
        let mut settings = defaults();
        for setting in &mut settings {
            if let Some(value) = values.get(&setting.key) {
                setting.value = normalize(&setting.key, value)?;
            }
        }
        settings.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(settings)
    }

    pub fn get(&self, key: &str) -> Result<String> {
        self.list()?
            .into_iter()
            .find(|s| s.key == key)
            .map(|s| s.value)
            .with_context(|| format!("unsupported macOS overlay setting: {key}"))
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.write(key, Some(normalize(key, value)?))
    }

    pub fn reset(&self, key: &str) -> Result<()> {
        if !defaults().iter().any(|s| s.key == key) {
            bail!("unsupported macOS overlay setting: {key}");
        }
        self.write(key, None)
    }

    fn write(&self, key: &str, value: Option<String>) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("overlay settings path has no parent")?;
        crate::paths::private_dir(parent)?;
        use std::os::unix::fs::OpenOptionsExt;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.path.with_extension("lock"))?;
        lock.lock_exclusive()?;
        let mut values = self.read()?;
        if let Some(value) = value {
            values.insert(key.into(), value);
        } else {
            values.remove(key);
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(temp.as_file_mut(), &values)?;
        temp.as_file_mut().flush()?;
        temp.as_file().sync_all()?;
        temp.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_settings_round_trip_and_validate() {
        let dir = tempfile::tempdir().unwrap();
        let settings = GSettings {
            path: dir.path().join("overlay.json"),
        };
        assert_eq!(settings.get("position").unwrap(), "'bottom-center'");
        for (key, value) in [
            ("enabled", "false"),
            ("position", "top-right"),
            ("scale", "1.5"),
            ("opacity", "0.5"),
        ] {
            settings.set(key, value).unwrap();
            assert_eq!(settings.get(key).unwrap(), normalize(key, value).unwrap());
            settings.reset(key).unwrap();
        }
        for (key, value) in [
            ("enabled", "1"),
            ("position", "left"),
            ("scale", "NaN"),
            ("scale", "3.1"),
            ("opacity", "0.1"),
            ("toggle-shortcut", "x"),
        ] {
            assert!(settings.set(key, value).is_err(), "{key}={value}");
        }
        std::fs::write(&settings.path, br#"{"scale":"NaN"}"#).unwrap();
        assert!(settings.list().is_err());
    }
}
