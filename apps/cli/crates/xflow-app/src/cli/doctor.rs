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
        "Run xflow daemon start",
    ));
    let gnome = desktop.to_lowercase().contains("gnome");
    checks.push(check(
        "session",
        !session.is_empty(),
        false,
        format!("desktop={desktop}; session={session}"),
        "Desktop delivery requires a graphical login; file transcription works headlessly",
    ));
    if session != "macos" {
        checks.push(check(
            "systemd",
            has_tool("systemctl"),
            false,
            "Optional user service management",
            "Without systemd, run xflowd in the foreground",
        ));
    } else {
        checks.push(check(
            "launchd",
            has_tool("launchctl"),
            false,
            "LaunchAgent user service management",
            "Use xflow service install --enable in a graphical login",
        ));
    }
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
        checks.push(check(
            "privacy",
            true,
            false,
            detail,
            "Review xflow config show; keys belong in environment variables or xflow key set",
        ));
        // Over-budget dictionary words are optional; an unbuildable provider fails every dictation.
        let hints = super::config::hint_warning(&config);
        checks.push(check(
            "dictionary hints",
            hints == Ok(None),
            hints.is_err(),
            match hints {
                Ok(None) => "All dictionary words fit the provider hint limit".into(),
                Ok(Some(text)) | Err(text) => text,
            },
            "Shorten stt.vocabulary or pick a supported provider; remove rarely used dictionary words",
        ));
    }
    checks
}
#[cfg(any(target_os = "macos", test))]
fn permission_checks(permissions: Option<&xflow_core::ipc::Permissions>) -> Vec<Check> {
    use xflow_core::ipc::{ACCESSIBILITY_FIX, INPUT_MONITORING_FIX, UNVERIFIED_SHORTCUT};
    let Some(permissions) = permissions else {
        return vec![check(
            "xflowd permissions",
            false,
            true,
            "Cannot verify xflowd's permissions",
            "Start or update xflowd, then run xflow doctor again",
        )];
    };
    let mut checks = vec![
        check(
            "Accessibility",
            permissions.accessibility,
            true,
            "xflowd text delivery",
            ACCESSIBILITY_FIX,
        ),
        check(
            "Input Monitoring",
            permissions.events(),
            true,
            if permissions.input_monitoring {
                "xflowd keyboard access"
            } else {
                "covered by Accessibility"
            },
            INPUT_MONITORING_FIX,
        ),
    ];
    if let Some(received) = permissions.keyboard_events {
        checks.push(check(
            "Shortcut events",
            received,
            false,
            if received {
                "xflowd has received keyboard events"
            } else {
                "xflowd shortcut is not verified"
            },
            UNVERIFIED_SHORTCUT,
        ));
    }
    checks
}
// PipeWire and PulseAudio both answer pactl. A monitor source records speakers.
#[cfg(any(target_os = "linux", test))]
fn echo_check(device: Option<&str>, default: &str, sources: &str) -> Check {
    if device.is_some() {
        return check(
            "Ignore speakers",
            false,
            false,
            "Cannot verify echo cancelling for a selected microphone",
            "Select the system default microphone in xflow, then make an echo-cancel source the default in Sound settings",
        );
    }
    let echo = |name: &str| {
        let name = name.to_lowercase();
        name.contains("echo") && name.contains("cancel") && !name.ends_with(".monitor")
    };
    let found = sources
        .lines()
        .filter_map(|l| l.split('\t').nth(1))
        .find(|&n| echo(n));
    let hint = match found {
        Some(name) => format!("Make it your microphone: pactl set-default-source '{name}'"),
        None => "Turn on echo cancelling. PipeWire: save `context.modules = [ { name = libpipewire-module-echo-cancel args = { monitor.mode = true } } ]` as ~/.config/pipewire/pipewire.conf.d/echo-cancel.conf, run systemctl --user restart pipewire, then pick the echo-cancel source as your microphone in Sound settings. PulseAudio: pactl load-module module-echo-cancel aec_method=webrtc, then select its echo-cancel input and output in Sound settings".into(),
    };
    check(
        "Ignore speakers",
        echo(default),
        false,
        if echo(default) {
            format!(
                "Echo-cancel input selected ({default}); verify playback routing in Sound settings"
            )
        } else {
            "Sound from your speakers can reach the microphone".into()
        },
        &hint,
    )
}
pub fn run(path: &Path, out: &Output) -> Result<()> {
    let status = super::ipc::request(&xflow_core::ipc::Request::Status);
    let daemon = status
        .as_ref()
        .map(|r| {
            format!(
                "{}; protocol {}",
                r.version.as_deref().unwrap_or_default(),
                r.protocol.unwrap_or_default()
            )
        })
        .map_err(|e| e.message.clone());
    #[cfg(target_os = "linux")]
    let (desktop, session) = (
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
    );
    #[cfg(target_os = "macos")]
    let (desktop, session) = ("macOS".to_string(), "macos".to_string());
    let mut checks = checks(path, &desktop, &session, present, daemon);
    #[cfg(target_os = "macos")]
    {
        checks.extend(permission_checks(
            status.as_ref().ok().and_then(|r| r.permissions.as_ref()),
        ));
    }
    if let Ok(cfg) = super::config::load(path).and_then(|f| f.config().map_err(Error::config)) {
        let known = super::provider::info(&cfg.stt.provider);
        checks.push(check(
            "provider",
            known.is_ok(),
            true,
            cfg.stt.provider.clone(),
            "Run xflow providers list and xflow providers use <provider>",
        ));
        let id = cfg.stt.provider.clone();
        let env = cfg.stt.api_key_env.clone();
        let source = super::ipc::block_on(async {
            Ok(xflow_providers::key_status(&id, env.as_deref()).await)
        })?;
        let (kind, note) = super::provider::describe(&id, &source);
        checks.push(check(
            "key",
            matches!(kind, "environment" | "keystore" | "not_required"),
            false,
            format!("{id}: {note}"),
            "Run xflow key status",
        ));
        #[cfg(target_os = "linux")]
        if cfg.recording.ignore_speakers {
            let default = super::system::tool("pactl", &["get-default-source"]).unwrap_or_default();
            let sources =
                super::system::tool("pactl", &["list", "short", "sources"]).unwrap_or_default();
            checks.push(echo_check(
                cfg.recording.device.as_deref(),
                default.trim(),
                &sources,
            ));
        }
    }
    // Refuses a damaged keystore, or one other users can read; the detail is the fix.
    let keystore = xflow_providers::check_keystore();
    checks.push(check(
        "keystore",
        keystore.is_ok(),
        true,
        match keystore {
            Ok(true) => "Provider keys are encrypted in xflow's own store, not config.toml".into(),
            Ok(false) => {
                "No keys stored yet; xflow key set <provider> creates the encrypted store".into()
            }
            Err(e) => format!("{e:#}"),
        },
        "Apply the fix above, or re-enter keys with xflow key set <provider>",
    ));
    if desktop.to_lowercase().contains("gnome") && present("gnome-extensions") {
        let extension = super::system::tool("gnome-extensions", &["info", "xflow@xflow.local"]);
        checks.push(check(
            "GNOME extension",
            extension.is_ok(),
            true,
            extension.unwrap_or_else(|e| e.message),
            "Run xflow extension install; on Wayland log out/back in, then xflow extension enable",
        ));
    }
    #[cfg(target_os = "macos")]
    {
        // 0 = Do Nothing; absent means the default "Show Emoji & Symbols".
        let usage = std::process::Command::new("defaults")
            .args(["read", "com.apple.HIToolbox", "AppleFnUsageType"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        checks.push(check(
            "Globe key",
            usage.as_deref() == Some("0"),
            false,
            format!("AppleFnUsageType={}", usage.as_deref().unwrap_or("unset")),
            "For the fn hotkey: System Settings → Keyboard → Press 🌐 key to: Do Nothing",
        ));
    }
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
    fn echo_diagnostic_checks_the_input_and_never_recommends_a_monitor() {
        let sources = "1\techo_cancel.monitor\tdriver\n2\techo_cancel.source\tdriver";
        assert!(echo_check(None, "echo_cancel.source", sources).ok);
        let selected = echo_check(Some("USB Microphone"), "echo_cancel.source", sources);
        assert!(!selected.ok && !selected.required);
        assert!(selected.detail.contains("Cannot verify"));
        let plain = echo_check(None, "alsa_input", sources);
        assert!(!plain.ok);
        assert!(plain.hint.ends_with("'echo_cancel.source'"));
        let monitor = echo_check(
            None,
            "echo_cancel.monitor",
            "1\techo_cancel.monitor\tdriver",
        );
        assert!(!monitor.ok);
        assert!(!monitor.hint.contains("set-default-source"));
        assert!(monitor.hint.contains("monitor.mode = true"));
        assert!(!echo_check(None, "", "").ok);
    }
    #[test]
    fn permissions_come_from_daemon_even_when_a_tap_exists() {
        use xflow_core::ipc::Permissions;
        for granted in [false, true] {
            let checks = permission_checks(Some(&Permissions {
                accessibility: granted,
                input_monitoring: granted,
                keyboard_events: Some(false),
                microphone: None,
            }));
            assert_eq!(checks.iter().all(|c| c.ok || !c.required), granted);
            let input = checks
                .iter()
                .find(|c| c.name == "Input Monitoring")
                .unwrap();
            assert_eq!(input.ok, granted);
            assert_eq!(input.hint, xflow_core::ipc::INPUT_MONITORING_FIX);
            assert!(checks.iter().any(|c| c.name == "Shortcut events" && !c.ok));
        }
        assert!(permission_checks(None).iter().any(|c| !c.ok && c.required));
    }
    #[test]
    fn fake_doctor_checks_are_session_specific() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let result = checks(&path, "GNOME", "wayland", |_| true, Ok("v1".into()));
        assert!(result.iter().all(|c| c.ok || !c.required));
        assert!(!result.iter().any(|c| c.name == "Wayland injection"));
        let mac = checks(&path, "macOS", "macos", |_| true, Ok("v1".into()));
        assert!(mac.iter().any(|c| c.name == "launchd"));
        assert!(!mac
            .iter()
            .any(|c| c.name == "systemd" || c.name.contains("GNOME")));
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
        let words: Vec<_> = (0..90).map(|i| format!("\"word{i:02}\"")).collect();
        std::fs::write(
            &path,
            format!("[dictionary]\nwords = [{}]", words.join(",")),
        )
        .unwrap();
        let result = checks(&path, "sway", "wayland", |_| true, Ok("v1".into()));
        let hints = result
            .iter()
            .find(|c| c.name == "dictionary hints")
            .unwrap();
        assert!(!hints.ok && !hints.required);
        assert!(hints
            .detail
            .starts_with("90 dictionary words exceed the groq hint limit"));
        let long = "Q".repeat(120);
        std::fs::write(
            &path,
            format!("[stt]\nvocabulary = [\"{long}\", \"b{long}\"]"),
        )
        .unwrap();
        let result = checks(&path, "sway", "wayland", |_| true, Ok("v1".into()));
        let hints = result
            .iter()
            .find(|c| c.name == "dictionary hints")
            .unwrap();
        assert!(!hints.ok && hints.required);
        assert!(hints.detail.contains("200 estimated tokens"));
    }
}
