use super::{args, output::Output, Error, Result};
use anyhow::Context;
use std::{
    io::{self, BufRead, IsTerminal, Read, Write},
    path::Path,
};
use xflow_app::config_edit::ConfigFile;
use xflow_core::{
    config::{Replacement, Snippet, Style},
    CleanupMode,
};

pub const TEMPLATE: &str = include_str!("../../../../config/default.toml");
pub fn load(path: &Path) -> Result<ConfigFile> {
    ConfigFile::load(path).map_err(Error::config)
}
pub fn commit(file: &ConfigFile, out: &Output) -> Result<()> {
    file.save().map_err(Error::config)?;
    super::ipc::reload_if_running(file.path(), out)?;
    out.confirm("Configuration saved")?;
    Ok(())
}
pub fn run(path: &Path, action: Option<&args::Config>, out: &Output) -> Result<()> {
    use args::Config::*;
    match action.unwrap_or(&Show) {
        Path => {
            out.data(
                &serde_json::json!({"path":path}),
                &path.display().to_string(),
            )?;
        }
        Show => {
            let file = load(path)?;
            out.data(&file.config().map_err(Error::config)?, &file.to_string())?;
        }
        Get { key } => {
            let file = load(path)?;
            let json = serde_json::to_value(file.config().map_err(Error::config)?)?;
            let mut typed = &json;
            for part in key.split('.') {
                typed = typed
                    .get(part)
                    .ok_or_else(|| Error::config(format!("Unknown configuration key: {key}")))?;
            }
            let value = file
                .effective(key)
                .map_err(Error::config)?
                .unwrap_or_else(|| "unset".into());
            out.data(&serde_json::json!({"key":key,"value":typed}), &value)?;
        }
        Set { key, value } => {
            let mut file = load(path)?;
            file.set(key, value).map_err(Error::config)?;
            commit(&file, out)?;
        }
        Unset { key } => {
            let mut file = load(path)?;
            file.unset(key).map_err(Error::config)?;
            commit(&file, out)?;
        }
        Validate => {
            load(path)?.config().map_err(Error::config)?;
            out.confirm("Configuration is valid")?;
        }
        Reset { yes } => {
            confirm(*yes, "Reset configuration to defaults?")?;
            replace(path, TEMPLATE.as_bytes())?;
            super::ipc::reload_if_running(path, out)?;
            out.confirm("Configuration reset to defaults")?;
        }
        Edit => edit(path, out)?,
    }
    Ok(())
}
/// Validate before replacing, write privately, and follow an existing config symlink.
pub fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let target = if path.is_symlink() {
        std::fs::canonicalize(path).map_err(Error::config)?
    } else {
        path.to_owned()
    };
    let parent = target.parent().context("configuration has no parent")?;
    xflow_app::paths::private_dir(parent).map_err(Error::config)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(Error::config)?;
    temp.write_all(bytes).map_err(Error::config)?;
    temp.as_file().sync_all().map_err(Error::config)?;
    load(temp.path())?.config().map_err(Error::config)?;
    temp.persist(&target).map_err(Error::config)?;
    Ok(())
}
fn edit(path: &Path, out: &Output) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| {
            Error::new(
                4,
                "No editor configured",
                "Set VISUAL or EDITOR, for example EDITOR=nano",
            )
        })?;
    // ponytail: editor commands support whitespace-separated arguments; shell expansion is deliberately absent.
    let mut parts = editor.split_whitespace();
    let program = parts.next().context("editor command is empty")?;
    let mut temp = tempfile::NamedTempFile::new()?;
    temp.write_all(load(path)?.to_string().as_bytes())?;
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(temp.path())
        .status()
        .context("cannot start editor")?;
    if !status.success() {
        return Err(Error::new(
            4,
            "Editor failed; original configuration preserved",
            "Check your editor command",
        ));
    }
    let content = bounded_read(temp.path(), 1024 * 1024)?;
    load(temp.path())?.config().map_err(Error::config)?;
    replace(path, &content)?;
    super::ipc::reload_if_running(path, out)?;
    out.confirm("Configuration saved")?;
    Ok(())
}
pub fn confirm(yes: bool, prompt: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err(Error::new(
            2,
            "Confirmation required",
            "Pass --yes in non-interactive use",
        ));
    }
    if ask(&format!("{prompt} [y/N] "), "")?.eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        Err(Error::new(1, "Operation cancelled", "No changes were made"))
    }
}
pub fn ask(prompt: &str, default: &str) -> Result<String> {
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut value = String::new();
    io::stdin().lock().take(16 * 1024).read_line(&mut value)?;
    let value = value.trim();
    Ok(if value.is_empty() {
        default.into()
    } else {
        value.into()
    })
}
pub fn bounded_read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::new(
            4,
            "Input file is too large",
            "Keep configuration/import files below 1 MiB",
        ));
    }
    Ok(bytes)
}
pub fn dictionary(path: &Path, action: Option<&args::Dictionary>, out: &Output) -> Result<()> {
    use args::Dictionary::*;
    let mut file = load(path)?;
    let mut config = file.config().map_err(Error::config)?;
    match action.unwrap_or(&List) {
        List => {
            let mut rows: Vec<_> = config
                .dictionary
                .words
                .iter()
                .map(|w| vec!["word".into(), w.clone(), "".into()])
                .collect();
            rows.extend(
                config
                    .dictionary
                    .replacements
                    .iter()
                    .map(|r| vec!["replace".into(), r.from.clone(), r.to.clone()]),
            );
            if out.json {
                out.data(&config.dictionary, "")?;
            } else {
                out.table(&["TYPE", "FROM / WORD", "TO"], &rows)?;
            }
            return Ok(());
        }
        Export => {
            out.data(&config.dictionary, &config.dictionary.words.join("\n"))?;
            return Ok(());
        }
        Add { words } => {
            for word in words {
                if !config.dictionary.words.contains(word) {
                    config.dictionary.words.push(word.clone());
                }
            }
        }
        Remove { words } => {
            config.dictionary.words.retain(|word| !words.contains(word));
        }
        Replace { from, to } => {
            config
                .dictionary
                .replacements
                .retain(|r| !r.from.eq_ignore_ascii_case(from));
            config.dictionary.replacements.push(Replacement {
                from: from.clone(),
                to: to.clone(),
            });
        }
        Unreplace { from } => {
            config
                .dictionary
                .replacements
                .retain(|r| !r.from.eq_ignore_ascii_case(from));
        }
        Import { file: source } => {
            let text = String::from_utf8(bounded_read(source, 1024 * 1024)?)?;
            for word in text.lines().map(str::trim).filter(|v| !v.is_empty()) {
                if !config.dictionary.words.iter().any(|v| v == word) {
                    config.dictionary.words.push(word.into());
                }
            }
        }
    }
    file.set_typed("dictionary.words", &config.dictionary.words)
        .map_err(Error::config)?;
    file.set_typed("dictionary.replacements", &config.dictionary.replacements)
        .map_err(Error::config)?;
    commit(&file, out)
}
pub fn snippets(path: &Path, action: Option<&args::Snippets>, out: &Output) -> Result<()> {
    use args::Snippets::*;
    let mut file = load(path)?;
    let mut config = file.config().map_err(Error::config)?;
    match action.unwrap_or(&List) {
        List => {
            if out.json {
                out.data(&config.snippets, "")?;
            } else {
                out.table(
                    &["TRIGGER", "TEXT"],
                    &config
                        .snippets
                        .iter()
                        .map(|s| vec![s.trigger.clone(), s.text.clone()])
                        .collect::<Vec<_>>(),
                )?;
            }
            return Ok(());
        }
        Add { trigger, text } => {
            config
                .snippets
                .retain(|s| !s.trigger.eq_ignore_ascii_case(trigger));
            config.snippets.push(Snippet {
                trigger: trigger.clone(),
                text: text.clone(),
            });
        }
        Remove { trigger } => {
            config
                .snippets
                .retain(|s| !s.trigger.eq_ignore_ascii_case(trigger));
        }
    }
    file.set_typed("snippets", &config.snippets)
        .map_err(Error::config)?;
    commit(&file, out)
}
pub fn styles(path: &Path, action: Option<&args::Styles>, out: &Output) -> Result<()> {
    use args::Styles::*;
    let mut file = load(path)?;
    let mut config = file.config().map_err(Error::config)?;
    match action.unwrap_or(&List) {
        List => {
            if out.json {
                out.data(&config.styles, "")?;
            } else {
                out.table(
                    &["NAME", "APPS", "MODE", "PROMPT"],
                    &config
                        .styles
                        .iter()
                        .map(|s| {
                            vec![
                                s.name.clone(),
                                s.apps.join(", "),
                                s.mode
                                    .map(|v| format!("{v:?}").to_lowercase())
                                    .unwrap_or_else(|| "inherit".into()),
                                s.prompt.clone().unwrap_or_default(),
                            ]
                        })
                        .collect::<Vec<_>>(),
                )?;
            }
            return Ok(());
        }
        Add {
            name,
            apps,
            mode,
            prompt,
        } => {
            config.styles.retain(|s| !s.name.eq_ignore_ascii_case(name));
            let mode = mode.map(|v| match v {
                args::Cleanup::Raw => CleanupMode::Raw,
                args::Cleanup::Light => CleanupMode::Light,
                args::Cleanup::Polished => CleanupMode::Polished,
                args::Cleanup::Custom => CleanupMode::Custom,
            });
            config.styles.push(Style {
                name: name.clone(),
                apps: apps.clone(),
                mode,
                prompt: prompt.clone(),
            });
        }
        Remove { name } => {
            config.styles.retain(|s| !s.name.eq_ignore_ascii_case(name));
        }
    }
    file.set_typed("styles", &config.styles)
        .map_err(Error::config)?;
    commit(&file, out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_roundtrip_invalid_editor_and_private_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        replace(&path, TEMPLATE.as_bytes()).unwrap();
        let mut file = load(&path).unwrap();
        file.set("stt.model", "whisper-large-v3").unwrap();
        file.save().unwrap();
        assert_eq!(
            load(&path).unwrap().config().unwrap().stt.model.as_deref(),
            Some("whisper-large-v3")
        );
        let before = std::fs::read(&path).unwrap();
        assert!(replace(&path, b"[stt]\nunknown = true\n").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        replace(&path, TEMPLATE.as_bytes()).unwrap();
        assert!(load(&path).unwrap().config().unwrap().stt.model.is_none());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
