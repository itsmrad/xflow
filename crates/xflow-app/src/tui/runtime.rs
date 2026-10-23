use super::{
    integrations, options,
    render::{self, HitMap},
    state::{Action, App, Effect, Modal, Page, Step, Tag},
    theme::ColorMode,
};
use crate::{
    config_edit::ConfigFile,
    gsettings::{GSettings, Setting},
    transport,
};
use anyhow::{bail, Context, Result};
use crossterm::{
    cursor::Show,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    io::IsTerminal,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{io::BufReader, net::UnixStream, sync::mpsc, task::JoinSet};
use xflow_core::{
    config::Config,
    ipc::{Request, Response},
};

enum Update {
    Loaded(Result<(ConfigFile, Option<String>)>),
    Saved(ConfigFile, Result<()>),
    Reply(Tag, Result<Response>),
    Event(Response),
    Connection(bool, String),
    Overlay(Result<Vec<Setting>>),
    KeyStatus(Box<Config>, (bool, bool, bool)),
    Devices(Result<Vec<String>>),
    Doctor(Vec<String>),
    Notice(Result<String>),
    MicTimeout,
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        Show
    );
}
type PanicHook = Arc<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>;
struct TerminalGuard {
    previous: PanicHook,
}
impl TerminalGuard {
    fn enter() -> Result<Self> {
        let previous: PanicHook = std::panic::take_hook().into();
        let hook = previous.clone();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            hook(info);
        }));
        let guard = Self { previous };
        enable_raw_mode()?;
        execute!(
            std::io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste
        )?;
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
        if !std::thread::panicking() {
            let hook = self.previous.clone();
            std::panic::set_hook(Box::new(move |info| hook(info)));
        }
    }
}

/// Frames are read in this dedicated task: select cancellation never discards a
/// partially read line. Reconnect only while disconnected, with bounded backoff.
async fn subscribe(tx: mpsc::Sender<Update>) {
    reconnect(tx, transport::connect).await;
}
async fn reconnect<F, Fut>(tx: mpsc::Sender<Update>, connect: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<UnixStream>>,
{
    let mut backoff = Duration::from_millis(250);
    loop {
        let result = async {
            let stream = connect().await?;
            stream_events(stream, &tx).await
        }
        .await;
        if tx.is_closed() {
            return;
        }
        if result.is_ok() {
            backoff = Duration::from_millis(250);
        }
        let message = result
            .err()
            .map(|e| format!("Daemon unavailable: {e:#}"))
            .unwrap_or_else(|| "Daemon disconnected".into());
        if tx.send(Update::Connection(false, message)).await.is_err() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(8));
    }
}
async fn stream_events(mut stream: UnixStream, tx: &mpsc::Sender<Update>) -> Result<()> {
    transport::write_frame(&mut stream, &Request::Subscribe).await?;
    let mut reader = BufReader::new(stream);
    let snapshot = tokio::time::timeout(
        Duration::from_secs(3),
        transport::read_frame::<Response, _>(&mut reader),
    )
    .await
    .context("Subscription snapshot timed out")??
    .context("Daemon closed subscription")?;
    if !snapshot.ok {
        bail!(
            "{}",
            snapshot
                .message
                .unwrap_or_else(|| "Subscription rejected".into())
        );
    }
    tx.send(Update::Connection(true, "Connected".into()))
        .await?;
    tx.send(Update::Event(snapshot)).await?;
    while let Some(response) = transport::read_frame::<Response, _>(&mut reader).await? {
        tx.send(Update::Event(response)).await?;
    }
    Ok(())
}

pub async fn run() -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("xflow tui needs an interactive terminal");
    }
    let mut app = App::new(crate::paths::config_path()?, ColorMode::detect());
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.clear()?;
    let (tx, mut rx) = mpsc::channel(64);
    let mut jobs = JoinSet::new();
    jobs.spawn(subscribe(tx.clone()));
    let generation = Arc::new(AtomicU64::new(0));
    dispatch(
        vec![Effect::Load(app.path.clone())],
        &tx,
        &mut jobs,
        &generation,
    );
    let mut input = EventStream::new();
    let mut hits = HitMap::default();
    let mut redraw = true;
    let mut mouse = false;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        if app.quit {
            break;
        }
        if mouse != app.config.ui.mouse {
            mouse = app.config.ui.mouse;
            if mouse {
                execute!(std::io::stdout(), EnableMouseCapture)?;
            } else {
                execute!(std::io::stdout(), DisableMouseCapture)?;
            }
        }
        if redraw {
            terminal.draw(|frame| hits = render::draw(frame, &app))?;
            redraw = false;
        }
        let mut effects = vec![];
        tokio::select! {
            update = rx.recv() => {
                let Some(update) = update else { break };
                redraw = true;
                effects = handle(&mut app, update, &mut redraw);
            },
            event = input.next() => {
                let Some(event) = event else { break };
                redraw = true;
                match event? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => effects = app.key(key),
                    Event::Paste(text) => {
                        let text = zeroize::Zeroizing::new(text);
                        app.paste(&text);
                    }
                    Event::Resize(_, _) => (),
                    Event::Mouse(event) if mouse => {
                        let point = ratatui::layout::Position::new(event.column, event.row);
                        let arrow = |code| KeyEvent::new(code, KeyModifiers::NONE);
                        match event.kind {
                            MouseEventKind::ScrollUp => effects = app.key(arrow(KeyCode::Up)),
                            MouseEventKind::ScrollDown => effects = app.key(arrow(KeyCode::Down)),
                            MouseEventKind::Down(MouseButton::Left) => {
                                if let Some((_, index)) = hits.options.iter().find(|(r, _)| r.contains(point)) {
                                    effects = app.pick(*index);
                                } else if let Some((_, page)) = hits.tabs.iter().find(|(r, _)| r.contains(point)) {
                                    effects = app.action(Action::Go(*page));
                                } else if let Some((_, action)) = hits.actions.iter().find(|(r, _)| r.contains(point)) {
                                    effects = app.action(*action);
                                } else if let Some((_, index)) = hits.rows.iter().find(|(r, _)| r.contains(point)) {
                                    effects = if *index == app.selected() {
                                        app.action(Action::Choose)
                                    } else {
                                        app.select(*index as isize - app.selected() as isize)
                                    };
                                }
                            }
                            _ => redraw = false,
                        }
                    }
                    _ => redraw = false,
                }
            },
            _ = term.recv() => {
                effects = app.action(Action::Quit);
                redraw = true;
            },
            Some(result) = jobs.join_next(), if !jobs.is_empty() => {
                match result {
                    // The panic hook already restored the terminal; drawing on would corrupt it.
                    Err(error) if error.is_panic() => break,
                    Err(error) => {
                        app.toast = Some(options::plain(&error.to_string()));
                        redraw = true;
                    }
                    Ok(()) => (),
                }
            },
        }
        // Every staged choice is saved right away; there is no separate save step.
        if app.dirty && !app.saving && app.file.is_some() {
            effects.extend(app.action(Action::Save));
        }
        dispatch(effects, &tx, &mut jobs, &generation);
    }
    jobs.abort_all();
    Ok(())
}

fn handle(app: &mut App, update: Update, redraw: &mut bool) -> Vec<Effect> {
    let mut effects = vec![];
    match update {
        Update::Loaded(result) => match result.and_then(|(file, disk)| app.load(file, disk)) {
            Ok(()) => {
                effects.push(Effect::KeyStatus(app.config.clone()));
                if app.disk.is_none() && !app.setup_offered && app.modal.is_none() {
                    effects.extend(app.open_setup(Step::Welcome));
                }
            }
            Err(e) => {
                app.toast = Some(format!(
                    "Your settings file has a problem, so defaults are shown: {}",
                    options::plain(&format!("{e:#}"))
                ))
            }
        },
        Update::Saved(file, result) => {
            app.saving = false;
            match result {
                Ok(()) => {
                    app.disk = Some(file.to_string());
                    app.dirty = false;
                    app.toast.get_or_insert_with(|| "Saved.".into());
                    effects.push(Effect::Request(Request::Reload, Tag::Reload));
                    effects.push(Effect::KeyStatus(app.config.clone()));
                    app.file = Some(file);
                }
                Err(e) => {
                    // Never leave an unsaved choice on screen: go back to what's on disk.
                    app.toast = Some(format!(
                        "That change wasn't saved. {}",
                        options::plain(&format!("{e:#}"))
                    ));
                    app.file = Some(file);
                    app.dirty = false;
                    effects.push(Effect::Load(app.path.clone()));
                }
            }
        }
        Update::Reply(tag, result) => {
            match result {
                Ok(response) => effects = app.response(response, tag),
                Err(_) if matches!(tag, Tag::Reload) => app.toast = Some(
                    "Saved. It takes effect when xflow's background helper starts (run xflowd)."
                        .into(),
                ),
                Err(error) => app.toast = Some(options::plain(&format!("{error:#}"))),
            }
        }
        Update::Event(response) => {
            *redraw = app.status.state != response.state
                || response.text.is_some()
                || response.state == xflow_core::State::Listening;
            let final_event = response.text.is_some();
            app.event(response);
            if app.reload_pending && !app.active() {
                app.reload_pending = false;
                effects.push(Effect::Request(Request::Reload, Tag::Reload));
            }
            if final_event && matches!(app.page, Page::History | Page::Stats) {
                effects = app.refresh();
            }
        }
        Update::Connection(connected, message) => {
            app.connected = connected;
            app.connection = message;
            if connected {
                effects = app.refresh();
            }
        }
        Update::Overlay(result) => match result {
            Ok(rows) => {
                app.overlay = rows;
                app.overlay_error = false;
            }
            Err(_) => app.overlay_error = true,
        },
        Update::KeyStatus(config, missing) => {
            app.key_status(&config, missing);
            if app.key_locked {
                app.toast = Some(xflow_providers::KEYCHAIN_WAITING.into());
            }
            if app.missing_key && !app.setup_offered && app.modal.is_none() {
                effects.extend(app.open_setup(Step::Welcome));
            }
        }
        Update::Devices(result) => match result {
            Ok(devices) => app.devices(devices),
            Err(e) => {
                app.toast = Some(options::plain(&format!("{e:#}")));
                if app.setup == Some(Step::Microphone) {
                    app.devices(vec![]);
                }
            }
        },
        Update::Doctor(rows) => app.diagnostics = rows,
        Update::MicTimeout => {
            if matches!(app.modal, Some(Modal::Setup(Step::MicCheck)))
                && app.status.state == xflow_core::State::Listening
            {
                effects = app.action(Action::Cancel);
            }
        }
        Update::Notice(result) => {
            app.toast = Some(match result {
                Ok(text) => text,
                Err(e) => options::plain(&format!("{e:#}")),
            });
            effects.push(Effect::KeyStatus(app.config.clone()));
        }
    }
    effects
}

fn dispatch(
    effects: Vec<Effect>,
    tx: &mpsc::Sender<Update>,
    jobs: &mut JoinSet<()>,
    generation: &Arc<AtomicU64>,
) {
    for effect in effects {
        let current = generation.clone();
        let history_generation = match &effect {
            Effect::Request(_, Tag::History(g)) => {
                current.store(*g, Ordering::Relaxed);
                Some(*g)
            }
            _ => None,
        };
        let tx = tx.clone();
        jobs.spawn(async move {
            // Debounce search without opening obsolete daemon connections.
            if let Some(g) = history_generation {
                tokio::time::sleep(Duration::from_millis(120)).await;
                if current.load(Ordering::Relaxed) != g {
                    return;
                }
            }
            let update = match effect {
                Effect::Request(request, tag) => {
                    Update::Reply(tag, transport::request(&request).await)
                }
                Effect::Load(path) => Update::Loaded(
                    blocking(move || {
                        let disk = read_optional(&path)?;
                        Ok((ConfigFile::load(&path)?, disk))
                    })
                    .await,
                ),
                Effect::Save(file, expected) => match tokio::task::spawn_blocking(move || {
                    let result = save(&file, expected.as_deref());
                    (file, result)
                })
                .await
                {
                    Ok((file, result)) => Update::Saved(file, result),
                    Err(e) => Update::Notice(Err(e.into())),
                },
                Effect::Overlay => Update::Overlay(blocking(|| GSettings::detect()?.list()).await),
                Effect::OverlaySet(key, value) => Update::Overlay(
                    blocking(move || {
                        let settings = GSettings::detect()?;
                        settings.set(&key, &value)?;
                        settings.list()
                    })
                    .await,
                ),
                Effect::KeyStatus(config) => {
                    let missing = integrations::missing_keys(&config).await;
                    Update::KeyStatus(Box::new(config), missing)
                }
                Effect::SaveKey(id, secret) => Update::Notice(
                    xflow_providers::save_key(&id, &secret)
                        .await
                        .map(|()| "Key saved in your system keychain.".into()),
                ),
                Effect::CheckProvider(config, cleanup) => {
                    Update::Notice(integrations::check(&config, cleanup).await)
                }
                Effect::MicTimeout(after) => {
                    tokio::time::sleep(after).await;
                    Update::MicTimeout
                }
                Effect::Devices => Update::Devices(blocking(integrations::devices).await),
                Effect::Doctor => {
                    let permissions = transport::request(&Request::Status)
                        .await
                        .ok()
                        .and_then(|r| r.permissions);
                    Update::Doctor(blocking(move || doctor(permissions)).await.unwrap_or_else(
                        |e| vec![format!("! {}", options::plain(&format!("{e:#}")))],
                    ))
                }
            };
            let _ = tx.send(update).await;
        });
    }
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await?
}
fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
/// Write only what the daemon would accept on reload: the same services are
/// built here (no network, no audio) so a bad combination is refused up front.
pub(super) fn save(file: &ConfigFile, expected: Option<&str>) -> Result<()> {
    if read_optional(file.path())?.as_deref() != expected {
        bail!("Configuration changed outside the TUI");
    }
    let config = file.config()?;
    xflow_providers::build_stt(&config.stt, config.privacy.offline)?;
    crate::daemon::build_transformer(&config)?;
    file.save()
}
fn doctor(_permissions: Option<xflow_core::ipc::Permissions>) -> Result<Vec<String>> {
    let (ok, warn) = (options::glyph("✓", "+"), "!");
    let mut rows = vec![];
    let path = crate::paths::config_path()?;
    rows.push(match ConfigFile::load(&path).and_then(|f| f.config()) {
        Ok(_) => format!("{ok} Your settings are valid"),
        Err(e) => format!(
            "{warn} Your settings file has a problem: {}",
            options::plain(&format!("{e:#}"))
        ),
    });
    #[cfg(target_os = "linux")]
    {
        rows.push(match GSettings::detect() {
            Ok(_) => format!("{ok} Recording pill is installed"),
            Err(_) => {
                format!("{warn} Recording pill isn't installed — run: xflow extension install")
            }
        });
        for (tool, what, fix) in [
            (
                "wl-copy",
                "Clipboard tool",
                "install the wl-clipboard package",
            ),
            (
                "ydotool",
                "Typing tool",
                "install ydotool, or install the recording pill",
            ),
        ] {
            let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .any(|p| p.join(tool).is_file());
            rows.push(if found {
                format!("{ok} {what} found")
            } else {
                format!("{warn} {what} missing — {fix}")
            });
        }
    }
    #[cfg(target_os = "macos")]
    rows.extend(permission_rows(_permissions.as_ref()));
    Ok(rows)
}

#[cfg(any(target_os = "macos", test))]
fn permission_rows(permissions: Option<&xflow_core::ipc::Permissions>) -> Vec<String> {
    use xflow_core::ipc::{ACCESSIBILITY_FIX, INPUT_MONITORING_FIX, UNVERIFIED_SHORTCUT};
    let Some(permissions) = permissions else {
        return vec![
            "! Cannot verify xflowd's permissions — start or update xflowd, then refresh".into(),
        ];
    };
    let ok = options::glyph("✓", "+");
    let mut rows = vec![
        if permissions.input_monitoring {
            format!("{ok} Input Monitoring is on for xflowd")
        } else {
            format!("! {INPUT_MONITORING_FIX}")
        },
        if permissions.accessibility {
            format!("{ok} Accessibility is on for xflowd")
        } else {
            format!("! {ACCESSIBILITY_FIX}")
        },
    ];
    if let Some(received) = permissions.keyboard_events {
        rows.push(if received {
            format!("{ok} xflowd has received keyboard events")
        } else {
            format!("! {UNVERIFIED_SHORTCUT}")
        });
    }
    rows.push("• Microphone: allow xflowd in System Settings → Privacy & Security → Microphone (the first recording asks)".into());
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use xflow_core::State;

    #[test]
    fn checkup_reports_daemon_grants_and_unverified_input() {
        use xflow_core::ipc::{
            Permissions, ACCESSIBILITY_FIX, INPUT_MONITORING_FIX, UNVERIFIED_SHORTCUT,
        };
        let mut permissions = Permissions {
            accessibility: false,
            input_monitoring: false,
            keyboard_events: Some(false),
        };
        let rows = permission_rows(Some(&permissions));
        assert!(rows.iter().any(|r| r.contains(INPUT_MONITORING_FIX)));
        assert!(rows.iter().any(|r| r.contains(ACCESSIBILITY_FIX)));
        assert!(rows.iter().all(|r| !r.contains("terminal")));
        // The selected row's Details pane must retain the fix at normal terminal sizes.
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().join("config.toml"), ColorMode::TrueColor);
        app.connected = true;
        app.page = Page::Doctor;
        app.diagnostics = rows;
        app.selected[Page::Doctor.index()] = 2;
        for width in [80, 100] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| {
                    render::draw(frame, &app);
                })
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("add it again"), "{text}");
        }
        permissions.accessibility = true;
        permissions.input_monitoring = true;
        assert!(permission_rows(Some(&permissions))
            .iter()
            .any(|r| r.contains(UNVERIFIED_SHORTCUT)));
        assert!(permission_rows(None)[0].starts_with("! Cannot verify"));
    }
    #[tokio::test]
    async fn mock_subscription_keeps_fragmented_frames_and_final_timings() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let reader = tokio::spawn(async move { stream_events(client, &tx).await.unwrap() });
        let mut server_reader = BufReader::new(&mut server);
        assert_eq!(
            transport::read_frame::<Request, _>(&mut server_reader)
                .await
                .unwrap(),
            Some(Request::Subscribe)
        );
        drop(server_reader);
        for response in [
            Response::status(State::Idle, 0.0),
            Response::status(State::Listening, 0.6),
            Response {
                text: Some("hello mock".into()),
                timings: Some(xflow_core::ipc::Timings {
                    total_ms: 27,
                    ..Default::default()
                }),
                ..Response::status(State::Success, 0.0)
            },
        ] {
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            for part in bytes.chunks(3) {
                server.write_all(part).await.unwrap();
                tokio::task::yield_now().await;
            }
        }
        server.shutdown().await.unwrap();
        reader.await.unwrap();
        assert!(matches!(rx.recv().await, Some(Update::Connection(true, _))));
        assert!(matches!(rx.recv().await,Some(Update::Event(r)) if r.state==State::Idle));
        assert!(matches!(rx.recv().await,Some(Update::Event(r)) if r.state==State::Listening));
        assert!(
            matches!(rx.recv().await,Some(Update::Event(r)) if r.text.as_deref()==Some("hello mock") && r.timings.as_ref().unwrap().total_ms==27)
        );
    }

    #[tokio::test]
    async fn isolated_reconnect_survives_daemon_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mock.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(&mut stream);
                assert_eq!(
                    transport::read_frame::<Request, _>(&mut reader)
                        .await
                        .unwrap(),
                    Some(Request::Subscribe)
                );
                drop(reader);
                transport::write_frame(&mut stream, &Response::status(State::Idle, 0.0))
                    .await
                    .unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        let (tx, mut rx) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            reconnect(tx, || {
                let path = path.clone();
                async move { Ok(UnixStream::connect(path).await?) }
            })
            .await
        });
        let mut connected = 0;
        tokio::time::timeout(Duration::from_secs(3), async {
            while connected < 2 {
                if matches!(rx.recv().await, Some(Update::Connection(true, _))) {
                    connected += 1;
                }
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn obsolete_search_does_not_open_any_daemon_connection() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut jobs = JoinSet::new();
        let generation = Arc::new(AtomicU64::new(0));
        dispatch(
            vec![Effect::Request(
                Request::History {
                    limit: 30,
                    offset: 0,
                    query: Some("obsolete".into()),
                },
                Tag::History(1),
            )],
            &tx,
            &mut jobs,
            &generation,
        );
        generation.store(2, Ordering::Relaxed);
        jobs.join_next().await.unwrap().unwrap();
        assert!(rx.try_recv().is_err());
    }
}
