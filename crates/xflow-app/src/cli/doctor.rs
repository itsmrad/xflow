use super::{output::Output, Error, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct Check {
    name: String,
    ok: bool,
    required: bool,
    detail: String,
    hint: String,
}
fn check(name: &str, ok: bool, required: bool, detail: impl Into<String>, hint: &str) -> Check {
    Check {
        name: name.into(),
        ok,
        required,
        detail: detail.into(),
        hint: hint.into(),
    }
}
pub fn present(tool: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(dir.join(tool))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}
fn checks(
    path: &Path,
    desktop: &str,
    session: &str,
    has_tool: impl Fn(&str) -> bool,
    daemon: std::result::Result<String, String>,
) -> Vec<Check> {
    let config = super::config::load(path).and_then(|f| f.config().map_err(Error::config));
    let mut checks = vec![check(
        "config",
        config.is_ok(),
        true,
        config
            .as_ref()
            .map(|_| format!("{} (defaults apply when absent)", path.display()))
            .unwrap_or_else(|e| e.message.clone()),
        "Run xflow config validate; correct the reported setting",
    )];
    checks.push(check(
        "daemon",
        daemon.is_ok(),
        true,
        daemon.unwrap_or_else(|v| v),
        "Run xflow daemon start, or run xflowd in a separate terminal",
    ));
    let gnome = desktop.to_lowercase().contains("gnome");
    checks.push(check(
        "session",
        !session.is_empty(),
        false,
        format!("desktop={desktop}; session={session}"),
        "Desktop delivery requires a graphical login; file transcription works headlessly",
    ));
    checks.push(check(
        "systemd",
        has_tool("systemctl"),
        false,
        "Optional user service management",
        "Without systemd, run xflowd in the foreground",
    ));
    if gnome {
        checks.push(check(
            "GNOME tools",
            has_tool("gnome-extensions") && has_tool("glib-compile-schemas"),
            true,
            "Native extension shortcuts and delivery",
            "Install gnome-shell and libglib2.0-bin; run xflow extension install",
        ));
    } else if session == "wayland" {
        checks.push(check(
            "Wayland clipboard",
            has_tool("wl-copy"),
            true,
            "Clipboard fallback",
            "Install wl-clipboard",
        ));
        checks.push(check(
            "Wayland injection",
            has_tool("ydotool") || has_tool("wtype"),
            false,
            "Text insertion fallback",
            "Configure ydotool/uinput, or use injection.method=clipboard",
        ));
    } else if session == "x11" {
        checks.push(check(
            "X11 clipboard",
            has_tool("xclip"),
            true,
            "Clipboard fallback",
            "Install xclip",
        ));
        checks.push(check(
            "X11 injection",
            has_tool("xdotool"),
            false,
            "Text insertion fallback",
            "Install xdotool, or use injection.method=clipboard",
        ));
    }
    if let Ok(config) = config {
        let detail = if config.privacy.offline {
            "Offline endpoint guard enabled"
        } else {
            "Cloud transcription uploads audio to the configured provider"
        };
        checks.push(check("privacy", true, false, detail, "Review xflow config show; keys belong in environment variables or OS credential storage"));
    }
    checks
}
pub fn run(path: &Path, out: &Output) -> Result<()> {
    let daemon = super::ipc::request(&xflow_core::ipc::Request::Status)
        .map(|r| {
            format!(
                "{}; protocol {}",
                r.version.unwrap_or_default(),
                r.protocol.unwrap_or_default()
            )
        })
        .map_err(|e| e.message);
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let checks = checks(path, &desktop, &session, present, daemon);
    let ok = checks.iter().all(|c| c.ok || !c.required);
    if out.json {
        out.data(&serde_json::json!({"ok":ok,"checks":checks,"manual_checks":["Microphone quality","Desktop shortcuts and text delivery","Provider credentials: xflow key status"]}), "")?;
    } else {
        out.table(
            &["CHECK", "RESULT", "DETAIL / FIX"],
            &checks
                .iter()
                .map(|c| {
                    vec![
                        c.name.clone(),
                        if c.ok {
                            "ok"
                        } else if c.required {
                            "FAIL"
                        } else {
                            "optional"
                        }
                        .into(),
                        if c.ok {
                            c.detail.clone()
                        } else {
                            format!("{}; {}", c.detail, c.hint)
                        },
                    ]
                })
                .collect::<Vec<_>>(),
        )?;
        out.text("Microphone, desktop delivery and provider credentials need separate checks; doctor never records or contacts a provider.")?;
    }
    if ok {
        Ok(())
    } else {
        Err(Error::new(
            6,
            "Required diagnostic checks failed",
            "Follow the fixes in the diagnostic report",
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fake_doctor_checks_are_session_specific() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let result = checks(&path, "GNOME", "wayland", |_| true, Ok("v1".into()));
        assert!(result.iter().all(|c| c.ok || !c.required));
        assert!(!result.iter().any(|c| c.name == "Wayland injection"));
        std::fs::write(&path, "[recording]\nmax_seconds = 0").unwrap();
        let result = checks(
            &path,
            "sway",
            "wayland",
            |_| false,
            Err("unavailable".into()),
        );
        assert!(result.iter().any(|c| c.name == "config" && !c.ok));
        assert!(result
            .iter()
            .any(|c| c.name == "Wayland clipboard" && !c.ok));
        assert!(result
            .iter()
            .find(|c| c.name == "Wayland injection")
            .is_some_and(|c| !c.required));
    }
}
