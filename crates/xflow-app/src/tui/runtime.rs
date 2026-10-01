use super::{
    integrations,
    render::{self, HitMap},
    state::{Action, App, Effect, Page, Tag},
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
        Event, EventStream, KeyEventKind, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    io::{IsTerminal, Write},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{io::BufReader, net::UnixStream, sync::mpsc, task::JoinSet};
use xflow_core::ipc::{HistoryEntry, Request, Response};

enum Update {
    Loaded(Result<(ConfigFile, Option<String>)>),
    Saved(ConfigFile, Result<()>),
    Reply(Tag, Result<Response>),
    Event(Response),
    Connection(bool, String),
    Overlay(Result<Vec<Setting>>),
    Providers(Vec<integrations::Provider>),
    Devices(Result<Vec<String>>),
    Doctor(Vec<String>),
    Notice(Result<String>),
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
    let mut ticks = tokio::time::interval(Duration::from_millis(80));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
            update=rx.recv()=>{
                let Some(update)=update else{break;};redraw=true;
                match update {
                    Update::Loaded(result)=>match result {Ok((file,disk))=>if let Err(e)=app.load(file,disk){app.toast=Some(format!("Invalid configuration: {e:#}"));},Err(e)=>app.toast=Some(format!("Cannot load configuration: {e:#}"))},
                    Update::Saved(file,result)=>{
                        app.saving=false;
                        match result {Ok(())=>{app.disk=Some(file.to_string());app.dirty=false;app.toast=Some("Saved; reloading daemon…".into());effects.push(Effect::Request(Request::Reload,Tag::Reload));},Err(e)=>app.toast=Some(format!("Save failed; edits retained: {e:#}"))}
                        app.file=Some(file);
                    },
                    Update::Reply(tag,result)=>match result{Ok(response)=>effects=app.response(response,tag),Err(error)=>app.toast=Some(format!("Request failed: {error:#}"))},
                    Update::Event(response)=>{
                        redraw=app.page==Page::Home || app.status.state!=response.state || response.text.is_some();
                        let final_event=response.text.is_some();app.event(response);
                        if final_event && matches!(app.page,Page::History|Page::Stats){effects=app.refresh();}
                    },
                    Update::Connection(connected,message)=>{app.connected=connected;app.connection=message;if connected{effects=app.refresh();}},
                    Update::Overlay(result)=>match result{Ok(rows)=>app.overlay=rows,Err(e)=>app.toast=Some(format!("{e:#}"))},
                    Update::Providers(rows)=>app.providers=rows,
                    Update::Devices(result)=>match result{Ok(devices)=>app.devices(devices),Err(e)=>app.toast=Some(format!("{e:#}"))},
                    Update::Doctor(rows)=>app.diagnostics=rows,
                    Update::Notice(result)=>{app.toast=Some(match result{Ok(text)=>text,Err(e)=>format!("{e:#}")});if app.page==Page::Providers{effects=app.refresh();}},
                }
            },
            event=input.next()=>{
                let Some(event)=event else{break;};redraw=true;
                match event? {
                    Event::Key(key) if key.kind!=KeyEventKind::Release=>effects=app.key(key),
                    Event::Paste(text)=>app.paste(&text),Event::Resize(_,_)=>(),
                    Event::Mouse(event) if mouse=>match event.kind {
                        MouseEventKind::ScrollUp=>effects=app.select(-1),MouseEventKind::ScrollDown=>effects=app.select(1),
                        MouseEventKind::Down(MouseButton::Left)=>{
                            let point=ratatui::layout::Position::new(event.column,event.row);
                            if let Some((_,page))=hits.tabs.iter().find(|(rect,_)|rect.contains(point)){effects=app.action(Action::Go(*page));}
                            else if let Some((_,action))=hits.actions.iter().find(|(rect,_)|rect.contains(point)){effects=app.action(*action);}
                            else if let Some((_,index))=hits.rows.iter().find(|(rect,_)|rect.contains(point)){effects=app.select(*index as isize-app.selected() as isize);}
                        },_=>redraw=false,
                    },_=>redraw=false,
                }
            },
            _=ticks.tick(),if app.active()=>{app.phase=app.phase.wrapping_add(1);redraw=app.page==Page::Home;},
            _=term.recv()=>{effects=app.action(Action::Quit);redraw=true;},
            Some(result)=jobs.join_next(),if !jobs.is_empty()=>{if let Err(error)=result{app.toast=Some(format!("Background operation failed: {error}"));redraw=true;}},
        }
        dispatch(effects, &tx, &mut jobs, &generation);
    }
    jobs.abort_all();
    Ok(())
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
                Effect::Providers(config, cleanup) => {
                    Update::Providers(integrations::providers(&config, cleanup).await)
                }
                Effect::SaveKey(id, secret) => Update::Notice(
                    xflow_providers::save_key(&id, &secret)
                        .await
                        .map(|()| "Key saved securely; Ctrl+S reloads the daemon".into()),
                ),
                Effect::DeleteKey(id) => Update::Notice(
                    integrations::delete_key(&id)
                        .await
                        .map(|()| "Key deleted".into()),
                ),
                Effect::CheckProvider(config, cleanup) => {
                    Update::Notice(integrations::check(&config, cleanup).await)
                }
                Effect::Devices => Update::Devices(blocking(integrations::devices).await),
                Effect::Export(path, entry) => Update::Notice(
                    blocking(move || {
                        export(&path, &entry)?;
                        Ok(format!("Exported {}", path.display()))
                    })
                    .await,
                ),
                Effect::ExportDraft(path, text) => Update::Notice(
                    blocking(move || {
                        write_new(&path, text.as_bytes())?;
                        Ok(format!(
                            "Exported configuration draft to {}",
                            path.display()
                        ))
                    })
                    .await,
                ),
                Effect::Doctor => Update::Doctor(
                    blocking(doctor)
                        .await
                        .unwrap_or_else(|e| vec![format!("! Diagnostics: {e:#}")]),
                ),
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
pub(super) fn save(file: &ConfigFile, expected: Option<&str>) -> Result<()> {
    if read_optional(file.path())?.as_deref() != expected {
        bail!("Configuration changed outside the TUI; export your edits before reverting and reapplying them");
    }
    file.save()
}
pub(super) fn export(path: &Path, entry: &HistoryEntry) -> Result<()> {
    let text = serde_json::to_vec_pretty(entry)?;
    write_new(path, &text)
}
fn write_new(path: &Path, text: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
    let mut file = options
        .open(path)
        .context("Choose a new writable export path")?;
    file.write_all(text)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn doctor() -> Result<Vec<String>> {
    let mut rows = vec![];
    let path = crate::paths::config_path()?;
    rows.push(match ConfigFile::load(&path).and_then(|f| f.config()) {
        Ok(_) => format!("✓ Config valid: {}", path.display()),
        Err(e) => format!("! Config: {e:#}; repair config.toml"),
    });
    rows.push(match GSettings::detect() {
        Ok(_) => "✓ GNOME extension schema installed".into(),
        Err(_) => "! GNOME extension missing; xflow extension install".into(),
    });
    for (tool, fix) in [
        ("wl-copy", "install wl-clipboard"),
        ("ydotool", "install ydotool or enable the GNOME extension"),
        ("gsettings", "install libglib2.0-bin"),
    ] {
        let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .any(|p| p.join(tool).is_file());
        rows.push(if found {
            format!("✓ {tool} available")
        } else {
            format!("! {tool} missing; {fix}")
        });
    }
    rows.push("Daemon connection is shown in the header; xflow service start when offline".into());
    rows.push("Keys: see 6 Providers for environment/keyring status and connection checks".into());
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use xflow_core::State;

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
