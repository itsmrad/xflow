use super::{args, output::Output, Error, Result};
use anyhow::Context;
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::Command,
};
const UUID: &str = "xflow@xflow.local";
const UNIT: &str = include_str!("../../../../packaging/xflow.service");
const ASSETS: &[(&str, &[u8])] = &[
    ("logic.js", include_bytes!("../../../../packaging/gnome-extension/logic.js")),
    ("prefs.js", include_bytes!("../../../../packaging/gnome-extension/prefs.js")),
    ("extension.js", include_bytes!("../../../../packaging/gnome-extension/extension.js")),
    ("metadata.json", include_bytes!("../../../../packaging/gnome-extension/metadata.json")),
    ("stylesheet.css", include_bytes!("../../../../packaging/gnome-extension/stylesheet.css")),
    ("schemas/org.gnome.shell.extensions.xflow.gschema.xml", include_bytes!("../../../../packaging/gnome-extension/schemas/org.gnome.shell.extensions.xflow.gschema.xml")),
];
fn xdg(name: &str, fallback: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(fallback)))
        .context("HOME or an XDG path is required")
        .map_err(Into::into)
}
fn unit_path() -> Result<PathBuf> {
    Ok(xdg("XDG_CONFIG_HOME", ".config")?.join("systemd/user/xflow.service"))
}
fn extension_path() -> Result<PathBuf> {
    Ok(xdg("XDG_DATA_HOME", ".local/share")?
        .join("gnome-shell/extensions")
        .join(UUID))
}
pub fn tool(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output().map_err(|e| {
        Error::new(
            1,
            format!("Cannot run {program}: {e}"),
            "Install the tool, or run xflowd in the foreground when systemd is unavailable",
        )
    })?;
    if !output.status.success() {
        return Err(Error::new(
            1,
            format!(
                "{program} failed: {}",
                super::output::clean(String::from_utf8_lossy(&output.stderr).trim())
            ),
            "Run xflow doctor for desktop setup guidance",
        ));
    }
    Ok(String::from_utf8(output.stdout)?)
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("install path has no parent")?;
    if !parent.exists() {
        xflow_app::paths::private_dir(parent)?;
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
pub fn service(action: &args::Service, out: &Output) -> Result<()> {
    match action {
        args::Service::Install { enable } => {
            atomic(&unit_path()?, UNIT.as_bytes())?;
            tool("systemctl", &["--user", "daemon-reload"])?;
            if *enable {
                tool("systemctl", &["--user", "enable", "--now", "xflow.service"])?;
            }
            out.confirm("User service installed (graphical-session.target); enable with xflow daemon start or xflow service install --enable")?;
        }
        args::Service::Uninstall => {
            tool(
                "systemctl",
                &["--user", "disable", "--now", "xflow.service"],
            )?;
            let path = unit_path()?;
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            tool("systemctl", &["--user", "daemon-reload"])?;
            out.confirm("User service uninstalled")?;
        }
    }
    Ok(())
}
pub fn daemon(action: &args::Daemon, out: &Output) -> Result<()> {
    use args::Daemon::*;
    let verb = match action {
        Start => "start",
        Stop => "stop",
        Restart => "restart",
        Status => "status",
        Logs { follow, lines } => {
            let lines = lines.to_string();
            let mut args = vec!["--user", "--unit=xflow.service", "--no-pager", "-n", &lines];
            if *follow {
                args.push("--follow");
            }
            if *follow {
                let mut child = Command::new("journalctl")
                    .args(&args)
                    .stdout(std::process::Stdio::piped())
                    .spawn()?;
                for line in std::io::BufReader::new(
                    child.stdout.take().context("journal output unavailable")?,
                )
                .lines()
                {
                    let line = line?;
                    if let Err(error) = out.data(&serde_json::json!({"message":line}), &line) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error.into());
                    }
                }
                if !child.wait()?.success() {
                    return Err(Error::new(1, "journalctl failed", "Run xflow doctor"));
                }
            } else {
                let text = tool("journalctl", &args)?;
                out.data(&serde_json::json!({"logs":text}), text.trim_end())?;
            }
            return Ok(());
        }
    };
    if matches!(action, Status) {
        let result = Command::new("systemctl")
            .args([
                "--user",
                "show",
                "xflow.service",
                "--property=ActiveState,SubState,MainPID,UnitFileState",
            ])
            .output()
            .context("systemd unavailable; start xflowd in a separate terminal")?;
        let text = String::from_utf8(result.stdout)?;
        if !result.status.success() {
            return Err(Error::new(
                3,
                "User service unavailable",
                "Install it with xflow service install, or run xflowd",
            ));
        }
        out.data(
            &serde_json::json!({"service":"xflow.service","properties":text}),
            text.trim_end(),
        )?;
    } else {
        tool("systemctl", &["--user", verb, "xflow.service"])?;
        out.confirm(&format!("Daemon service: {verb}"))?;
    }
    Ok(())
}
pub fn extension(action: &args::Extension, out: &Output) -> Result<()> {
    use args::Extension::*;
    match action {
        Install { enable } => {
            let root = extension_path()?;
            for (name, bytes) in ASSETS {
                atomic(&root.join(name), bytes)?;
            }
            tool(
                "glib-compile-schemas",
                &[
                    "--strict",
                    root.join("schemas")
                        .to_str()
                        .context("non-UTF-8 extension path")?,
                ],
            )?;
            if *enable {
                tool("gnome-extensions", &["enable", UUID]).map_err(|_| {
                    Error::new(
                        1,
                        "Extension installed but GNOME could not enable it yet",
                        "Log out and back in, then run xflow extension enable",
                    )
                })?;
            }
            out.confirm("GNOME extension installed. On Wayland, log out and back in before enabling a new installation with xflow extension enable.")?;
        }
        Uninstall => {
            let root = extension_path()?;
            tool("gnome-extensions", &["disable", UUID])?;
            // Remove only the embedded assets, preserving unknown user files.
            for (name, _) in ASSETS {
                let path = root.join(name);
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
            }
            let compiled = root.join("schemas/gschemas.compiled");
            if compiled.exists() {
                std::fs::remove_file(compiled)?;
            }
            let _ = std::fs::remove_dir(root.join("schemas"));
            let _ = std::fs::remove_dir(root);
            out.confirm("GNOME extension uninstalled")?;
        }
        Enable | Disable => {
            let verb = if matches!(action, Enable) {
                "enable"
            } else {
                "disable"
            };
            tool("gnome-extensions", &[verb, UUID]).map_err(|e| Error::new(e.code, e.message, "For a new Wayland installation, log out and back in before xflow extension enable"))?;
            out.confirm(&format!("Extension {verb}d"))?;
        }
        Status => {
            let text = tool("gnome-extensions", &["info", UUID])?;
            out.data(
                &serde_json::json!({"uuid":UUID,"info":text}),
                text.trim_end(),
            )?;
        }
    }
    Ok(())
}
pub fn overlay(action: Option<&args::Overlay>, out: &Output) -> Result<()> {
    use args::Overlay::*;
    let settings = xflow_app::gsettings::GSettings::detect()?;
    match action.unwrap_or(&List) {
        List => {
            let rows = settings.list()?;
            if out.json {
                out.data(&rows.iter().map(|s| serde_json::json!({"key":s.key,"value":s.value,"kind":format!("{:?}",s.kind),"summary":s.summary})).collect::<Vec<_>>(), "")?;
            } else {
                out.table(
                    &["KEY", "VALUE", "DESCRIPTION"],
                    &rows
                        .iter()
                        .map(|s| vec![s.key.clone(), s.value.clone(), s.summary.clone()])
                        .collect::<Vec<_>>(),
                )?;
            }
        }
        Get { key } => {
            let value = settings.get(key)?;
            out.data(&serde_json::json!({"key":key,"value":value}), &value)?;
        }
        Set { key, value } => {
            settings.set(key, value)?;
            out.confirm("Overlay setting saved")?;
        }
        Reset { key } => {
            if let Some(key) = key {
                settings.reset(key)?;
            } else {
                for setting in settings.list()? {
                    settings.reset(&setting.key)?;
                }
            }
            out.confirm("Overlay settings reset")?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_follows_graphical_session_and_assets_are_complete() {
        assert!(UNIT.contains("PartOf=graphical-session.target"));
        assert!(UNIT.contains("WantedBy=graphical-session.target"));
        assert!(ASSETS.iter().any(|(p, _)| p.ends_with("gschema.xml")));
        assert_eq!(ASSETS.len(), 6);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("install/asset");
        atomic(&path, b"first").unwrap();
        atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"second");
    }
}
