use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::OnceCell,
    time::timeout,
};
use xflow_core::{
    config::{InjectionConfig, InjectionMethod},
    AppContext, Desktop, InjectionOutcome,
};

const MAX_TEXT: usize = 64 * 1024;
const BUS_TIMEOUT: Duration = Duration::from_millis(300);

pub struct LinuxDesktop {
    config: InjectionConfig,
    wayland: bool,
    connection: OnceCell<zbus::Connection>,
}

impl LinuxDesktop {
    pub fn new(config: &InjectionConfig) -> Self {
        Self {
            config: config.clone(),
            wayland: std::env::var_os("WAYLAND_DISPLAY").is_some(),
            connection: OnceCell::new(),
        }
    }

    async fn connection(&self) -> Result<&zbus::Connection> {
        timeout(
            BUS_TIMEOUT,
            self.connection.get_or_try_init(zbus::Connection::session),
        )
        .await
        .context("session bus connection timed out")?
        .map_err(Into::into)
    }

    async fn shell(&self) -> Result<Option<zbus::Proxy<'_>>> {
        timeout(BUS_TIMEOUT, async {
            let connection = self.connection().await?;
            let bus = zbus::fdo::DBusProxy::new(connection).await?;
            // Pin the unique owner so replacing an extension cannot redirect this call.
            let name = zbus::names::BusName::try_from("org.xflow.Shell")?;
            let owner = match bus.get_name_owner(name).await {
                Ok(owner) => owner,
                Err(zbus::fdo::Error::NameHasNoOwner(_)) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            Ok(Some(
                zbus::Proxy::new(connection, owner, "/org/xflow/Shell", "org.xflow.Shell").await?,
            ))
        })
        .await
        .context("Shell owner request timed out")?
    }

    async fn clipboard(&self, primary: bool) -> Result<String> {
        let value = if self.wayland {
            output(
                "wl-paste",
                if primary {
                    &["--primary", "--no-newline"]
                } else {
                    &["--no-newline"]
                },
            )
            .await?
        } else {
            output(
                "xclip",
                &[
                    "-o",
                    "-selection",
                    if primary { "primary" } else { "clipboard" },
                ],
            )
            .await?
        };
        parse_selection(value)
    }

    fn restore_later(&self, previous: String, injected: &str, primary: bool) {
        let wayland = self.wayland;
        let delay = self.config.restore_delay_ms;
        let injected = injected.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(u64::from(delay))).await;
            let desktop = LinuxDesktop {
                config: InjectionConfig::default(),
                wayland,
                connection: OnceCell::new(),
            };
            if desktop
                .clipboard(primary)
                .await
                .is_ok_and(|current| current == injected)
            {
                let _ = if primary {
                    if wayland {
                        copy_with("wl-copy", &["--primary"], &previous).await
                    } else {
                        copy_with("xclip", &["-selection", "primary", "-in"], &previous).await
                    }
                } else {
                    desktop.copy(&previous).await
                };
            }
        });
    }
}

fn is_terminal(app: Option<&str>) -> bool {
    let app = app.unwrap_or_default().to_ascii_lowercase();
    [
        "terminal",
        "console",
        "ptyxis",
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
        "st-256color",
    ]
    .iter()
    .any(|terminal| app.contains(terminal))
        || app == "st"
}

fn paste_shortcut(app: Option<&str>) -> &'static str {
    let app_lower = app.unwrap_or_default().to_ascii_lowercase();
    // xterm/rxvt traditionally paste PRIMARY with Shift+Insert. xclip fills
    // PRIMARY too on this path; modern terminals use the CLIPBOARD shortcut.
    if app_lower.contains("xterm")
        || app_lower.contains("rxvt")
        || app_lower == "st"
        || app_lower == "st-256color"
    {
        "shift+Insert"
    } else if is_terminal(app) {
        "ctrl+shift+v"
    } else {
        "ctrl+v"
    }
}

fn safe_target(text: &str, target: &AppContext, current: &AppContext) -> bool {
    if target.window_id.as_deref().is_none_or(str::is_empty)
        || target.app_id.as_deref().is_none_or(str::is_empty)
        || target.window_id != current.window_id
        || target.app_id != current.app_id
    {
        return false;
    }
    // Terminal control characters can submit commands even without a newline.
    !(is_terminal(current.app_id.as_deref()) && text.chars().any(char::is_control))
}

fn injection_options(config: &InjectionConfig, target: &AppContext) -> serde_json::Value {
    serde_json::json!({
        "method": if config.effective_method() == InjectionMethod::Type { "type" } else { "paste" },
        "terminal": is_terminal(target.app_id.as_deref()),
        "restore_clipboard": config.restore_clipboard,
        "restore_delay_ms": config.restore_delay_ms,
        "target": {"app_id": target.app_id, "window_id": target.window_id},
    })
}

fn injection_result(json: &str) -> Result<InjectionOutcome> {
    if json.len() > MAX_TEXT {
        bail!("Shell injection result exceeds limit");
    }
    let value: serde_json::Value = serde_json::from_str(json)?;
    match value["outcome"].as_str() {
        Some("pasted") => Ok(InjectionOutcome::Pasted),
        Some("typed") => Ok(InjectionOutcome::Typed),
        Some("clipboard_only") => Ok(InjectionOutcome::ClipboardOnly),
        _ => bail!("invalid Shell injection outcome"),
    }
}

fn parse_selection(text: String) -> Result<String> {
    if text.len() > MAX_TEXT {
        bail!("selection exceeds 64 KiB");
    }
    Ok(text) // Preserve leading/trailing spaces and newlines exactly.
}

async fn output(program: &str, args: &[&str]) -> Result<String> {
    timeout(Duration::from_secs(2), async {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("cannot run {program}"))?;
        let mut data = Vec::new();
        child
            .stdout
            .take()
            .context("missing command stdout")?
            .take((MAX_TEXT + 1) as u64)
            .read_to_end(&mut data)
            .await?;
        if data.len() > MAX_TEXT {
            bail!("desktop command returned too much output");
        }
        if !child.wait().await?.success() {
            bail!("{program} failed");
        }
        Ok(String::from_utf8(data)?)
    })
    .await
    .context("desktop command timed out")?
}

async fn input_command(mut command: Command, text: &str, duration: Duration) -> Result<()> {
    timeout(duration, async {
        // Clipboard owners inherit no pipes, so their parent can exit normally.
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child.stdin.take().context("missing command stdin")?;
        input.write_all(text.as_bytes()).await?;
        input.shutdown().await?;
        drop(input);
        if !child.wait().await?.success() {
            bail!("desktop helper failed");
        }
        Ok(())
    })
    .await
    .context("desktop helper timed out")?
}

async fn copy_with(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut command = Command::new(program);
    command.args(args);
    input_command(command, text, Duration::from_secs(3))
        .await
        .with_context(|| format!("cannot own clipboard with {program}"))
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
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.file_type().is_socket()))
}
#[cfg(not(unix))]
fn ydotool_socket() -> Option<PathBuf> {
    None
}

async fn virtual_paste(shortcut: &str) -> Result<()> {
    let socket = ydotool_socket().context("ydotool daemon unavailable")?;
    let keys: &[&str] = match shortcut {
        "ctrl+shift+v" => &["29:1", "42:1", "47:1", "47:0", "42:0", "29:0"],
        "shift+Insert" => &["42:1", "110:1", "110:0", "42:0"],
        _ => &["29:1", "47:1", "47:0", "29:0"],
    };
    let run = |keys: &[&str]| {
        let mut command = Command::new("ydotool");
        command.env("YDOTOOL_SOCKET", &socket).arg("key").args(keys);
        input_command(command, "", Duration::from_secs(2))
    };
    if run(keys).await.is_err() {
        let _ = run(&["29:0", "42:0", "47:0", "110:0"]).await;
        bail!("virtual keyboard paste failed");
    }
    Ok(())
}

async fn virtual_type(text: &str, wayland: bool) -> Result<()> {
    let mut command = if wayland {
        if let Some(socket) = ydotool_socket() {
            let mut command = Command::new("ydotool");
            command.env("YDOTOOL_SOCKET", socket).args([
                "type",
                "--key-delay",
                "0",
                "--key-hold",
                "0",
                "--file",
                "-",
            ]);
            command
        } else {
            let mut command = Command::new("wtype");
            command.arg("-");
            command
        }
    } else {
        let mut command = Command::new("xdotool");
        command.args(["type", "--clearmodifiers", "--delay", "0", "--file", "-"]);
        command
    };
    // No transcript appears in argv or shell syntax. Preserve it on helper failure.
    command.stdin(Stdio::piped());
    input_command(command, text, Duration::from_secs(10)).await
}

fn sway_focus(value: &serde_json::Value) -> Option<AppContext> {
    if value["focused"].as_bool() == Some(true) {
        return Some(AppContext {
            app_id: value["app_id"]
                .as_str()
                .or_else(|| value["window_properties"]["class"].as_str())
                .map(str::to_owned),
            window_id: value["id"].as_u64().map(|id| id.to_string()),
            selected_text: None,
        });
    }
    ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|key| value[*key].as_array())
        .flat_map(|nodes| nodes.iter())
        .find_map(sway_focus)
}

async fn wayland_context() -> Result<AppContext> {
    // ponytail: Sway's focus adapter makes the requested wlroots/wtype path
    // usable; other compositors safely copy until they have an identity adapter.
    if std::env::var_os("SWAYSOCK").is_some() {
        let json = output("swaymsg", &["-t", "get_tree", "-r"]).await?;
        return Ok(sway_focus(&serde_json::from_str(&json)?).unwrap_or_default());
    }
    Ok(AppContext::default())
}

#[async_trait]
impl Desktop for LinuxDesktop {
    async fn context(&self) -> Result<AppContext> {
        if let Ok(Some(shell)) = self.shell().await {
            if let Ok(Ok(json)) =
                timeout(BUS_TIMEOUT, shell.call::<_, _, String>("Context", &())).await
            {
                if json.len() <= MAX_TEXT {
                    return Ok(serde_json::from_str(&json)?);
                }
            }
        }
        if self.wayland {
            return Ok(wayland_context().await.unwrap_or_default());
        }
        if std::env::var_os("DISPLAY").is_none() {
            return Ok(AppContext::default());
        }
        let Ok(window) = output("xdotool", &["getactivewindow"]).await else {
            return Ok(AppContext::default());
        };
        let window = window.trim().to_owned();
        let app = output("xdotool", &["getwindowclassname", &window])
            .await
            .ok()
            .map(|s| s.trim().to_owned());
        Ok(AppContext {
            app_id: app,
            window_id: Some(window),
            selected_text: None,
        })
    }

    async fn selection(&self) -> Result<Option<String>> {
        if let Ok(Some(shell)) = self.shell().await {
            if let Ok(Ok(json)) =
                timeout(BUS_TIMEOUT, shell.call::<_, _, String>("Selection", &())).await
            {
                if json.len() > MAX_TEXT * 6 + 32 {
                    bail!("Shell selection result exceeds limit");
                }
                let value: serde_json::Value = serde_json::from_str(&json)?;
                return match &value["text"] {
                    serde_json::Value::Null => Ok(None),
                    serde_json::Value::String(s) => Ok(Some(parse_selection(s.clone())?)),
                    _ => bail!("invalid Shell selection result"),
                };
            }
        }
        Ok(self.clipboard(true).await.ok().filter(|s| !s.is_empty()))
    }

    async fn copy(&self, text: &str) -> Result<()> {
        if text.len() > MAX_TEXT {
            bail!("injection text exceeds 64 KiB");
        }
        if self.wayland {
            copy_with("wl-copy", &["--type", "text/plain;charset=utf-8"], text).await
        } else if std::env::var_os("DISPLAY").is_some() {
            copy_with("xclip", &["-selection", "clipboard", "-in"], text).await
        } else {
            bail!("no desktop display; transcript remains available through xflow last")
        }
    }

    async fn inject(&self, text: &str, target: &AppContext) -> Result<InjectionOutcome> {
        if text.len() > MAX_TEXT {
            bail!("injection text exceeds 64 KiB");
        }
        let method = self.config.effective_method();
        if method == InjectionMethod::Clipboard || !safe_target(text, target, target) {
            self.copy(text).await?;
            return Ok(InjectionOutcome::ClipboardOnly);
        }
        if let Ok(Some(shell)) = self.shell().await {
            let options = injection_options(&self.config, target).to_string();
            // An uncertain reply may follow successful delivery. Never repeat keys
            // via fallback after a dispatched call; propagate the error instead.
            let json: String = timeout(
                Duration::from_secs(2),
                shell.call("Inject", &(text, options)),
            )
            .await
            .context("Shell injection timed out; delivery is uncertain")??;
            return injection_result(&json);
        }
        let previous = if method == InjectionMethod::Paste && self.config.restore_clipboard {
            self.clipboard(false).await.ok()
        } else {
            None
        };
        if method == InjectionMethod::Paste {
            self.copy(text).await?;
        }
        let current = self.context().await?;
        if !safe_target(text, target, &current) {
            if method == InjectionMethod::Type {
                self.copy(text).await?;
            }
            return Ok(InjectionOutcome::ClipboardOnly);
        }
        let mut previous_primary = None;
        let result = if method == InjectionMethod::Type {
            virtual_type(text, self.wayland).await
        } else {
            let shortcut = paste_shortcut(current.app_id.as_deref());
            // PRIMARY is required by the traditional X terminal binding.
            if shortcut == "shift+Insert" {
                if self.config.restore_clipboard {
                    previous_primary = self.clipboard(true).await.ok();
                }
                if self.wayland {
                    copy_with("wl-copy", &["--primary"], text).await?;
                } else {
                    copy_with("xclip", &["-selection", "primary", "-in"], text).await?;
                }
            }
            if self.wayland {
                if ydotool_socket().is_some() {
                    virtual_paste(shortcut).await
                } else {
                    let args: &[&str] = match shortcut {
                        "ctrl+shift+v" => &[
                            "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
                        ],
                        "shift+Insert" => &["-M", "shift", "-k", "Insert", "-m", "shift"],
                        _ => &["-M", "ctrl", "-k", "v", "-m", "ctrl"],
                    };
                    output("wtype", args).await.map(|_| ())
                }
            } else {
                output("xdotool", &["key", "--clearmodifiers", shortcut])
                    .await
                    .map(|_| ())
            }
        };
        if result.is_err() {
            if method == InjectionMethod::Type {
                self.copy(text).await?;
            }
            return Ok(InjectionOutcome::ClipboardOnly);
        }
        if let Some(previous) = previous {
            self.restore_later(previous, text, false);
        }
        if let Some(previous) = previous_primary {
            self.restore_later(previous, text, true);
        }
        Ok(if method == InjectionMethod::Type {
            InjectionOutcome::Typed
        } else {
            InjectionOutcome::Pasted
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
        assert!(!safe_target(
            "text",
            &target(Some("editor"), Some("1")),
            &target(Some("other"), Some("1"))
        ));
        assert!(!safe_target(
            "text",
            &target(Some(""), Some("1")),
            &target(Some(""), Some("1"))
        ));
        assert!(safe_target(
            "text",
            &target(Some("editor"), Some("1")),
            &target(Some("editor"), Some("1"))
        ));
    }
    #[test]
    fn terminal_shortcuts_and_multiline_guard() {
        for app in [
            "org.gnome.Terminal",
            "org.gnome.Console",
            "org.gnome.Ptyxis",
            "kgx",
            "kitty",
            "foot",
            "konsole",
            "alacritty",
            "org.wezfurlong.wezterm",
            "ghostty",
        ] {
            let context = target(Some(app), Some("1"));
            assert_eq!(paste_shortcut(Some(app)), "ctrl+shift+v", "{app}");
            assert!(safe_target("echo hi", &context, &context));
            for text in ["echo hi\n", "echo hi\r", "echo\u{1b}[13~"] {
                assert!(!safe_target(text, &context, &context));
            }
        }
        for app in ["xterm", "urxvt", "st"] {
            assert_eq!(paste_shortcut(Some(app)), "shift+Insert");
        }
        let editor = target(Some("code"), Some("2"));
        assert!(safe_target("line one\nline two", &editor, &editor));
        assert_eq!(paste_shortcut(Some("code")), "ctrl+v");
    }
    #[test]
    fn injection_options_and_outcomes_follow_contract() {
        let config = InjectionConfig {
            method: InjectionMethod::Type,
            restore_clipboard: false,
            restore_delay_ms: 123,
            ..Default::default()
        };
        let context = target(Some("org.gnome.Ptyxis"), Some("42"));
        let options = injection_options(&config, &context);
        assert_eq!(
            options,
            serde_json::json!({"method":"type", "terminal":true, "restore_clipboard":false, "restore_delay_ms":123, "target":{"app_id":"org.gnome.Ptyxis", "window_id":"42"}})
        );
        for (name, expected) in [
            ("pasted", InjectionOutcome::Pasted),
            ("typed", InjectionOutcome::Typed),
            ("clipboard_only", InjectionOutcome::ClipboardOnly),
        ] {
            assert_eq!(
                injection_result(&format!("{{\"outcome\":\"{name}\",\"message\":null}}")).unwrap(),
                expected
            );
        }
        assert!(injection_result(r#"{"outcome":"unknown"}"#).is_err());
        assert_eq!(
            InjectionConfig {
                clipboard_only: true,
                ..config
            }
            .effective_method(),
            InjectionMethod::Clipboard
        );
    }
    #[test]
    fn selection_preserves_whitespace_and_is_bounded() {
        assert_eq!(
            parse_selection("  selection\n\n".into()).unwrap(),
            "  selection\n\n"
        );
        assert!(parse_selection("x".repeat(MAX_TEXT)).is_ok());
        assert!(parse_selection("x".repeat(MAX_TEXT + 1)).is_err());
    }
    #[test]
    fn sway_adapter_handles_nested_and_floating_focus() {
        let value = serde_json::json!({"nodes":[{"nodes":[], "floating_nodes":[{"focused":true,"id":42,"app_id":"editor"}]}]});
        assert_eq!(sway_focus(&value), Some(target(Some("editor"), Some("42"))));
        assert_eq!(sway_focus(&serde_json::json!({"nodes":[]})), None);
    }

    struct StubShell {
        calls: std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>,
    }
    #[zbus::interface(name = "org.xflow.Shell")]
    impl StubShell {
        fn context(&self) -> String {
            serde_json::to_string(&target(Some("org.gnome.Ptyxis"), Some("42"))).unwrap()
        }
        fn selection(&self) -> String {
            r#"{"text":"  selected\n"}"#.into()
        }
        fn inject(&self, text: &str, options: &str) -> String {
            let options: serde_json::Value = serde_json::from_str(options).unwrap();
            let outcome = if options["target"]["window_id"] != "42" {
                "clipboard_only"
            } else if options["method"] == "type" {
                "typed"
            } else {
                "pasted"
            };
            self.calls.lock().unwrap().push((text.to_owned(), options));
            serde_json::json!({"outcome":outcome,"message":null}).to_string()
        }
    }
    #[tokio::test]
    #[ignore = "requires isolated bus: dbus-run-session -- cargo test -p xflow-platform -- --ignored"]
    async fn shell_client_against_isolated_stub() {
        assert!(
            std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none(),
            "unset real displays before this test"
        );
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let service = zbus::ConnectionBuilder::session()
            .unwrap()
            .name("org.xflow.Shell")
            .unwrap()
            .serve_at(
                "/org/xflow/Shell",
                StubShell {
                    calls: calls.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let desktop = LinuxDesktop::new(&InjectionConfig::default());
        let context = desktop.context().await.unwrap();
        assert_eq!(context.window_id.as_deref(), Some("42"));
        assert_eq!(
            desktop.selection().await.unwrap().as_deref(),
            Some("  selected\n")
        );
        let now = std::time::Instant::now();
        assert_eq!(
            desktop.inject("hello", &context).await.unwrap(),
            InjectionOutcome::Pasted
        );
        eprintln!(
            "isolated cached Shell Inject round trip: {:?}",
            now.elapsed()
        );
        assert_eq!(
            desktop
                .inject("hello", &target(Some("editor"), Some("other")))
                .await
                .unwrap(),
            InjectionOutcome::ClipboardOnly
        );
        let typed = LinuxDesktop::new(&InjectionConfig {
            method: InjectionMethod::Type,
            ..Default::default()
        });
        assert_eq!(
            typed.inject("héllo", &context).await.unwrap(),
            InjectionOutcome::Typed
        );
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[0].1["terminal"], true);
            assert_eq!(calls[0].1["restore_delay_ms"], 300);
        }
        service.close().await.unwrap();
    }
}
