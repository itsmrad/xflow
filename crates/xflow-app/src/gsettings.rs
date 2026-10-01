//! The GNOME extension's settings (overlay look, global shortcuts) for the CLI
//! and TUI, through the stock `gsettings` tool so every write applies live.
use anyhow::{bail, Context, Result};
use std::{path::PathBuf, process::Command};

pub const SCHEMA: &str = "org.gnome.shell.extensions.xflow";
const EXTENSION_UUID: &str = "xflow@xflow.local";

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Double {
        min: f64,
        max: f64,
    },
    Choice(Vec<String>),
    /// Any other GVariant type, e.g. "s" or "as" (shortcut lists).
    Type(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub key: String,
    /// GVariant text, e.g. `true`, `200`, `'bottom'`, `['<Super>space']`.
    pub value: String,
    pub kind: Kind,
    pub summary: String,
}

/// A handle on the extension schema: its private schema directory when the
/// extension is installed per-user, otherwise the system schema path.
pub struct GSettings {
    schema_dir: Option<PathBuf>,
    memory_backend: bool,
}

impl GSettings {
    pub fn detect() -> Result<Self> {
        let mut roots: Vec<PathBuf> = std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .into_iter()
            .collect();
        roots.push("/usr/share".into());
        roots.push("/usr/local/share".into());
        let schema_dir = roots
            .into_iter()
            .map(|root| {
                root.join("gnome-shell/extensions")
                    .join(EXTENSION_UUID)
                    .join("schemas")
            })
            .find(|dir| dir.join("gschemas.compiled").is_file());
        let settings = Self {
            schema_dir,
            memory_backend: false,
        };
        settings.run(&["list-keys", SCHEMA]).context(
            "the xflow GNOME extension settings are not installed; run `xflow extension install`",
        )?;
        Ok(settings)
    }

    /// For tests: a compiled schema directory and a throwaway in-memory backend.
    pub fn with_schema_dir(dir: PathBuf, memory_backend: bool) -> Self {
        Self {
            schema_dir: Some(dir),
            memory_backend,
        }
    }

    pub fn list(&self) -> Result<Vec<Setting>> {
        let listing = self.run(&["list-recursively", SCHEMA])?;
        let mut settings = Vec::new();
        for line in listing.lines() {
            // "<schema> <key> <value>"
            let mut parts = line.splitn(3, ' ');
            let (Some(_), Some(key), Some(value)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            settings.push(Setting {
                key: key.to_owned(),
                value: value.to_owned(),
                kind: parse_range(&self.run(&["range", SCHEMA, key])?),
                summary: self.run(&["describe", SCHEMA, key])?.trim().to_owned(),
            });
        }
        settings.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(settings)
    }

    pub fn get(&self, key: &str) -> Result<String> {
        Ok(self.run(&["get", SCHEMA, key])?.trim().to_owned())
    }

    /// `value` is GVariant text; bare words are accepted for string keys.
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.run(&["set", SCHEMA, key, value]).map(drop)
    }

    pub fn reset(&self, key: &str) -> Result<()> {
        self.run(&["reset", SCHEMA, key]).map(drop)
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let mut command = Command::new("gsettings");
        if let Some(dir) = &self.schema_dir {
            command.arg("--schemadir").arg(dir);
        }
        if self.memory_backend {
            command.env("GSETTINGS_BACKEND", "memory");
        }
        let output = command
            .args(args)
            .output()
            .context("cannot run gsettings (install libglib2.0-bin)")?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            bail!("gsettings {}: {}", args[0], error.trim());
        }
        Ok(String::from_utf8(output.stdout)?)
    }
}

fn parse_range(text: &str) -> Kind {
    let mut lines = text.lines();
    let head = lines.next().unwrap_or_default();
    let words: Vec<&str> = head.split_whitespace().collect();
    match words.as_slice() {
        ["type", "b"] => Kind::Bool,
        ["range", kind, min, max] if matches!(*kind, "i" | "u" | "x" | "t" | "n" | "q" | "y") => {
            Kind::Int {
                min: min.parse().unwrap_or(i64::MIN),
                max: max.parse().unwrap_or(i64::MAX),
            }
        }
        ["range", "d", min, max] => Kind::Double {
            min: min.parse().unwrap_or(f64::MIN),
            max: max.parse().unwrap_or(f64::MAX),
        },
        ["enum"] => Kind::Choice(
            lines
                .map(|line| line.trim().trim_matches('\'').to_owned())
                .filter(|choice| !choice.is_empty())
                .collect(),
        ),
        ["type", kind] => Kind::Type((*kind).to_owned()),
        _ => Kind::Type(head.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_output_is_classified() {
        assert_eq!(parse_range("type b\n"), Kind::Bool);
        assert_eq!(
            parse_range("range i 120 480\n"),
            Kind::Int { min: 120, max: 480 }
        );
        assert_eq!(
            parse_range("range d 0.2 1\n"),
            Kind::Double { min: 0.2, max: 1.0 }
        );
        assert_eq!(
            parse_range("enum\n'top'\n'bottom'\n"),
            Kind::Choice(vec!["top".into(), "bottom".into()])
        );
        assert_eq!(parse_range("type as\n"), Kind::Type("as".into()));
    }

    #[test]
    fn reads_and_writes_a_compiled_schema() {
        let tools = ["gsettings", "glib-compile-schemas"];
        if tools
            .iter()
            .any(|tool| Command::new(tool).arg("--help").output().is_err())
        {
            eprintln!("skipping: GLib tools unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../packaging/gnome-extension/schemas/org.gnome.shell.extensions.xflow.gschema.xml"
            ),
            dir.path().join("org.gnome.shell.extensions.xflow.gschema.xml"),
        )
        .unwrap();
        assert!(Command::new("glib-compile-schemas")
            .arg(dir.path())
            .status()
            .unwrap()
            .success());
        let settings = GSettings::with_schema_dir(dir.path().to_owned(), true);
        let all = settings.list().unwrap();
        assert!(all.iter().any(|setting| setting.kind == Kind::Bool));
        let key = &all[0];
        settings.set(&key.key, &key.value).unwrap();
        assert_eq!(settings.get(&key.key).unwrap(), key.value);
        assert!(settings.set("no-such-key", "1").is_err());
        settings.reset(&key.key).unwrap();
    }
}
