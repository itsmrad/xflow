use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};
use xflow_core::{config::InjectionConfig, AppContext, Desktop, InjectionOutcome};

pub struct LinuxDesktop {
    clipboard_only: bool,
    wayland: bool,
}

impl LinuxDesktop {
    pub fn new(config: &InjectionConfig) -> Self {
        Self {
            clipboard_only: config.clipboard_only,
            wayland: std::env::var_os("WAYLAND_DISPLAY").is_some(),
        }
    }
}

fn is_terminal(app: Option<&str>) -> bool {
    let app = app.unwrap_or_default().to_lowercase();
    [
        "terminal",
        "kgx",
        "konsole",
        "kitty",
        "alacritty",
        "wezterm",
        "foot",
        "xterm",
        "tilix",
        "terminator",
        "hyper",
        "warp",
        "ghostty",
        "rxvt",
    ]
    .iter()
    .any(|terminal| app.contains(terminal))
}

fn safe_target(text: &str, target: &AppContext, current: &AppContext) -> bool {
    // No attempt to refocus a window: typing must still target the original app.
    if target.window_id.is_none()
        || target.app_id.is_none()
        || target.window_id != current.window_id
        || target.app_id != current.app_id
    {
        return false;
    }
    // Pasting newlines into a terminal can execute commands. An unknown app is
    // equally unsuitable for multiline automatic paste; explicit manual paste works.
    !(text.contains(['\n', '\r'])
        && (is_terminal(current.app_id.as_deref()) || current.app_id.is_none()))
}

async fn output(program: &str, args: &[&str]) -> Result<String> {
    let result = timeout(
        Duration::from_secs(2),
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("desktop command timed out")?
    .with_context(|| format!("cannot run {program}"))?;
    if !result.status.success() {
        bail!("{program} failed");
    }
    if result.stdout.len() > 16_384 {
        bail!("desktop command returned too much output");
    }
    Ok(String::from_utf8(result.stdout)?.trim().to_owned())
}

async fn copy_with(program: &str, args: &[&str], text: &str) -> Result<()> {
    timeout(Duration::from_secs(3), async {
        // Clipboard helpers fork an owner process. Do not capture stdout/stderr:
        // inherited pipes held by that owner would make wait_with_output hang.
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("install {program} to enable clipboard support"))?;
        let mut input = child.stdin.take().context("missing clipboard stdin")?;
        input.write_all(text.as_bytes()).await?;
        input.shutdown().await?;
        drop(input);
        if !child.wait().await?.success() {
            bail!("{program} could not own the clipboard");
        }
        Ok(())
    })
    .await
    .context("clipboard operation timed out")?
}

async fn shell_context() -> Result<AppContext> {
    timeout(Duration::from_millis(500), async {
        let connection = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &connection,
            "org.xflow.Shell",
            "/org/xflow/Shell",
            "org.xflow.Shell",
        )
        .await?;
        let json: String = proxy.call("Context", &()).await?;
        Ok::<_, anyhow::Error>(serde_json::from_str(&json)?)
    })
    .await
    .context("GNOME context request timed out")?
}

#[cfg(unix)]
fn ydotool_socket() -> Option<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    let mut candidates: Vec<PathBuf> = std::env::var_os("YDOTOOL_SOCKET")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if candidates.is_empty() {
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
            candidates.push(PathBuf::from(runtime).join(".ydotool_socket"));
        }
        candidates.push(PathBuf::from("/tmp/.ydotool_socket"));
    }
    candidates
        .into_iter()
        .find(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()))
}

#[cfg(not(unix))]
fn ydotool_socket() -> Option<PathBuf> {
    None
}

async fn virtual_paste(terminal: bool) -> Result<()> {
    let socket = ydotool_socket().context("ydotool daemon unavailable")?;
    let keys = if terminal {
        ["42:1", "110:1", "110:0", "42:0"]
    } else {
        ["29:1", "47:1", "47:0", "29:0"]
    };
    let run = |args: Vec<&str>| {
        let mut command = Command::new("ydotool");
        command
            .env("YDOTOOL_SOCKET", &socket)
            .arg("key")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        async move {
            timeout(Duration::from_secs(2), command.status())
                .await
                .context("virtual keyboard timed out")?
                .context("cannot run ydotool")
        }
    };
    match run(keys.to_vec()).await {
        Ok(status) if status.success() => Ok(()),
        _ => {
            // Best effort release if a helper fails while modifiers are held.
            let _ = run(vec!["29:0", "42:0", "47:0", "110:0"]).await;
            bail!("virtual keyboard paste failed")
        }
    }
}

#[async_trait]
impl Desktop for LinuxDesktop {
    async fn context(&self) -> Result<AppContext> {
        if self.wayland {
            return Ok(shell_context().await.unwrap_or_default());
        }
        if std::env::var_os("DISPLAY").is_none() {
            return Ok(AppContext::default());
        }
        let Ok(window) = output("xdotool", &["getactivewindow"]).await else {
            return Ok(AppContext::default());
        };
        let app = output("xdotool", &["getwindowclassname", &window])
            .await
            .ok();
        Ok(AppContext {
            app_id: app,
            window_id: Some(window),
            selected_text: None,
        })
    }

    async fn copy(&self, text: &str) -> Result<()> {
        if self.wayland {
            copy_with("wl-copy", &["--type", "text/plain;charset=utf-8"], text).await
        } else if std::env::var_os("DISPLAY").is_some() {
            copy_with("xclip", &["-selection", "clipboard", "-in"], text).await
        } else {
            bail!("no desktop display; transcript remains available through xflow last")
        }
    }

    async fn inject(&self, text: &str, target: &AppContext) -> Result<InjectionOutcome> {
        self.copy(text).await?;
        if self.clipboard_only {
            return Ok(InjectionOutcome::ClipboardOnly);
        }
        let current = self.context().await?;
        if !safe_target(text, target, &current) {
            return Ok(InjectionOutcome::ClipboardOnly);
        }
        let terminal = is_terminal(current.app_id.as_deref());
        let paste = if self.wayland {
            virtual_paste(terminal).await
        } else {
            output(
                "xdotool",
                &[
                    "key",
                    "--clearmodifiers",
                    if terminal { "shift+Insert" } else { "ctrl+v" },
                ],
            )
            .await
            .map(|_| ())
        };
        Ok(if paste.is_ok() {
            InjectionOutcome::Pasted
        } else {
            InjectionOutcome::ClipboardOnly
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(app: Option<&str>, window: Option<&str>) -> AppContext {
        AppContext {
            app_id: app.map(str::to_owned),
            window_id: window.map(str::to_owned),
            selected_text: None,
        }
    }
    #[test]
    fn changed_or_undiscoverable_focus_never_pastes() {
        assert!(!safe_target(
            "text",
            &target(None, None),
            &target(None, None)
        ));
        assert!(!safe_target(
            "text",
            &target(None, Some("1")),
            &target(None, Some("1"))
        ));
        assert!(!safe_target(
            "text",
            &target(Some("editor"), Some("1")),
            &target(Some("editor"), Some("2"))
        ));
        assert!(safe_target(
            "text",
            &target(Some("editor"), Some("1")),
            &target(Some("editor"), Some("1"))
        ));
    }
    #[test]
    fn terminal_multiline_never_executes() {
        for app in [
            Some("org.gnome.Terminal"),
            Some("org.gnome.Console-kgx"),
            Some("foot"),
            None,
        ] {
            let context = target(app, Some("1"));
            assert!(!safe_target("echo hi\n", &context, &context));
            assert!(!safe_target("echo hi\r", &context, &context));
            assert_eq!(safe_target("echo hi", &context, &context), app.is_some());
        }
        let editor = target(Some("code"), Some("2"));
        assert!(safe_target("line one\nline two", &editor, &editor));
    }
}
