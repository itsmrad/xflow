use crate::{
    store::Store,
    transport::{read_frame, write_frame},
};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::{
    fs::File,
    os::unix::fs::OpenOptionsExt,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::BufReader,
    net::{UnixListener, UnixStream},
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};
pub use xflow_core::ipc::Command;
use xflow_core::{
    config::Config,
    ipc::{Request, Response, MAX_MESSAGE_BYTES, PROTOCOL_VERSION},
    AppContext, AudioCapture, AudioClip, Desktop, DesktopEvent, Mode, SpeechToText, State,
    TextTransformer, TranscriptionOptions, TransformRequest,
};
enum Completion {
    Transcribed {
        generation: u64,
        result: Result<(String, Option<String>)>,
    },
    Injected {
        generation: u64,
        result: Result<xflow_core::InjectionOutcome>,
        warning: Option<String>,
    },
}

pub struct Services {
    pub audio: Arc<dyn AudioCapture>,
    pub desktop: Arc<dyn Desktop>,
    pub stt: Arc<dyn SpeechToText>,
    pub transformer: Option<Arc<dyn TextTransformer>>,
}

pub struct Engine {
    config: Config,
    audio: Arc<dyn AudioCapture>,
    desktop: Arc<dyn Desktop>,
    stt: Arc<dyn SpeechToText>,
    transformer: Option<Arc<dyn TextTransformer>>,
    store: Store,
    state: State,
    mode: Mode,
    target: AppContext,
    last: Option<String>,
    message: Option<String>,
    generation: u64,
    job: Option<JoinHandle<()>>,
    started: Option<Instant>,
    events: broadcast::Sender<Response>,
    desktop_events: broadcast::Sender<DesktopEvent>,
}
impl Engine {
    pub fn new(
        config: Config,
        services: Services,
        store: Store,
        events: broadcast::Sender<Response>,
        desktop_events: broadcast::Sender<DesktopEvent>,
    ) -> Self {
        Self {
            config,
            audio: services.audio,
            desktop: services.desktop,
            stt: services.stt,
            transformer: services.transformer,
            store,
            state: State::Idle,
            mode: Mode::Dictation,
            target: AppContext::default(),
            last: None,
            message: None,
            generation: 0,
            job: None,
            started: None,
            events,
            desktop_events,
        }
    }
    fn status(&self) -> Response {
        let mut response = Response::status(
            self.state,
            if self.state == State::Listening {
                self.audio.level()
            } else {
                0.0
            },
        );
        response.message = self.message.clone();
        response.version = Some(env!("CARGO_PKG_VERSION").into());
        response.protocol = Some(PROTOCOL_VERSION);
        response.provider = Some(self.stt.name().into());
        response.model = Some(self.stt.model().to_owned()).filter(|model| !model.is_empty());
        if matches!(self.state, State::Listening | State::Processing) {
            response.mode = Some(self.mode);
        }
        response
    }
    fn publish(&self) {
        let response = self.status();
        let _ = self.desktop_events.send(DesktopEvent {
            state: response.state,
            level: response.level,
            message: response.message.clone(),
            mode: self.mode,
        });
        let _ = self.events.send(response);
    }
    fn set_state(&mut self, state: State, message: Option<String>) {
        self.state = state;
        self.message = message.map(|message| bounded_message(&message));
        self.publish();
    }
    async fn start(&mut self, mode: Mode, context: Option<AppContext>) -> Result<()> {
        if matches!(self.state, State::Listening | State::Processing) {
            bail!("a recording is already active");
        }
        if mode == Mode::Command {
            bail!("command mode is not available yet");
        }
        // Open the microphone first: a slow focus query must never clip speech.
        if let Err(error) = self.audio.start().await {
            let _ = self.audio.cancel().await;
            self.set_state(State::Error, Some(error.to_string()));
            return Err(error);
        }
        let stt = self.stt.clone();
        tokio::spawn(async move {
            let _ = stt.warm().await;
        });
        self.generation += 1;
        self.mode = mode;
        self.started = Some(Instant::now());
        self.set_state(State::Listening, None);
        self.target = match context {
            Some(context) => context,
            None => self.desktop.context().await.unwrap_or_default(),
        };
        Ok(())
    }
    async fn stop(&mut self, completion: mpsc::Sender<Completion>) -> Result<()> {
        if self.state != State::Listening {
            bail!("no recording is active");
        }
        self.started = None;
        let audio = match self.audio.stop().await {
            Ok(audio) => audio,
            Err(error) => {
                let _ = self.audio.cancel().await;
                self.set_state(State::Error, Some(error.to_string()));
                return Err(error);
            }
        };
        self.set_state(State::Processing, None);
        self.transcribe(audio, completion);
        Ok(())
    }
    fn transcribe(&mut self, audio: AudioClip, completion: mpsc::Sender<Completion>) {
        let stt = self.stt.clone();
        let transformer = self.transformer.clone();
        let context = self.target.clone();
        // The provider was built from SttConfig and already owns these defaults.
        let options = TranscriptionOptions::default();
        let mode = self.config.cleanup.mode;
        let instructions = self.config.cleanup.prompt.clone();
        let app_id = context
            .app_id
            .clone()
            .filter(|_| self.config.cleanup.app_context);
        let vocabulary = self.config.dictionary.words.clone();
        let generation = self.generation;
        self.job = Some(tokio::spawn(async move {
            let result = async {
                let text = stt
                    .transcribe(audio, options)
                    .await?
                    .text
                    .trim()
                    .to_string();
                if text.is_empty() {
                    bail!("no speech recognized");
                }
                if text.len() > 32 * 1024 {
                    bail!("transcript exceeds 32 KiB safety limit");
                }
                let result = if let Some(transformer) = transformer {
                    let request = TransformRequest {
                        text: &text,
                        mode,
                        instructions: instructions.as_deref(),
                        command: None,
                        app_id: app_id.as_deref(),
                        vocabulary: &vocabulary,
                    };
                    match transformer.transform(request).await {
                        Ok(clean) if !clean.trim().is_empty() && clean.len() <= 32 * 1024 => {
                            (clean, None)
                        }
                        _ => (
                            text,
                            Some("Cleanup failed; original transcript retained".into()),
                        ),
                    }
                } else {
                    (text, None)
                };
                validate_last_frame(&result.0)?;
                Ok(result)
            }
            .await;
            let _ = completion
                .send(Completion::Transcribed { generation, result })
                .await;
        }));
    }
    async fn cancel(&mut self) -> Result<()> {
        self.generation += 1;
        if let Some(job) = self.job.take() {
            job.abort();
        }
        self.started = None;
        if let Err(error) = self.audio.cancel().await {
            self.set_state(State::Error, Some(error.to_string()));
            return Err(error);
        }
        self.set_state(State::Idle, None);
        Ok(())
    }
    async fn handle(
        &mut self,
        request: Request,
        completion: &mpsc::Sender<Completion>,
    ) -> Result<Response> {
        match request {
            Request::Start { mode, context, .. } => self.start(mode, context).await?,
            Request::Stop => self.stop(completion.clone()).await?,
            Request::Toggle { mode, context, .. } => {
                if self.state == State::Listening {
                    self.stop(completion.clone()).await?
                } else {
                    self.start(mode, context).await?
                }
            }
            Request::Cancel => self.cancel().await?,
            Request::Last => {
                let mut response = self.status();
                response.text = self.last.clone();
                return Ok(response);
            }
            Request::CopyLast => {
                self.desktop
                    .copy(self.last.as_deref().context("no transcript yet")?)
                    .await?
            }
            Request::PasteLast => {
                if matches!(self.state, State::Listening | State::Processing) {
                    bail!("wait for the current recording before pasting");
                }
                let text = self.last.as_deref().context("no transcript yet")?;
                let target = self.desktop.context().await.unwrap_or_default();
                let injection = self.desktop.inject(text, &target).await?;
                let mut response = self.status();
                response.injection = Some(injection);
                return Ok(response);
            }
            Request::History {
                limit,
                offset: 0,
                query: None,
            } => {
                let mut response = self.status();
                response.history = self.store.history(limit).await?;
                // Keep complete entries and a bounded frame; omit older entries until it fits.
                while serde_json::to_vec(&response)?.len() >= xflow_core::ipc::MAX_MESSAGE_BYTES - 1
                {
                    response.history.pop();
                }
                return Ok(response);
            }
            Request::ClearHistory => {
                if matches!(self.state, State::Listening | State::Processing) {
                    self.cancel().await?;
                }
                self.store.clear().await?;
                self.last = None;
            }
            Request::History { .. }
            | Request::HistoryGet { .. }
            | Request::HistoryDelete { .. }
            | Request::HistoryCopy { .. }
            | Request::HistoryPaste { .. }
            | Request::Stats
            | Request::Reload => bail!("this request is not available yet"),
            Request::Status | Request::Subscribe | Request::Shutdown => (),
        }
        Ok(self.status())
    }
    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let (completion_tx, mut completion_rx) = mpsc::channel(8);
        let mut levels = tokio::time::interval(Duration::from_millis(50));
        levels.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending_injection: Option<(u64, String, Option<String>)> = None;
        loop {
            let deadline = self.started.map(|s| {
                tokio::time::Instant::from_std(
                    s + Duration::from_secs(self.config.recording.max_seconds as u64),
                )
            });
            tokio::select! {
                biased;
                command = commands.recv() => {
                    let Some(command) = command else { break; };
                    let shutdown = matches!(command.request, Request::Shutdown);
                    let response = match self.handle(command.request, &completion_tx).await {
                        Ok(response) => response,
                        Err(error) => Response::error(self.state, error.to_string()),
                    };
                    let _ = command.reply.send(response);
                    if shutdown { break; }
                }
                Some(completion) = completion_rx.recv() => match completion {
                    Completion::Transcribed { generation, result } if generation == self.generation => {
                        self.job = None;
                        match result {
                            Ok((text, mut warning)) => {
                                // Save before injection so an unsupported desktop never loses dictation.
                                self.last = Some(text.clone());
                                if self.store.append(&text, self.stt.name()).await.is_err() { warning = Some("History save failed; transcript available via last".into()); }
                                // A cancel queued while SQLite was saving must run before
                                // desktop injection starts.
                                pending_injection = Some((generation, text, warning));
                            }
                            Err(error) => self.set_state(State::Error, Some(error.to_string())),
                        }
                    }
                    Completion::Injected { generation, result, warning } if generation == self.generation => {
                        self.job = None;
                        match result {
                            Ok(outcome) => {
                                let message = match outcome {
                                    xflow_core::InjectionOutcome::ClipboardOnly => Some("Transcript copied; paste manually".into()),
                                    _ => warning,
                                };
                                self.set_state(State::Success, message);
                                let mut response = self.status(); response.injection = Some(outcome); let _ = self.events.send(response);
                            }
                            Err(error) => self.set_state(State::Error, Some(format!("Injection failed: {error}; transcript available via last"))),
                        }
                    }
                    _ => (), // canceled jobs cannot overwrite newer sessions
                },
                _ = async {}, if pending_injection.is_some() => {
                    let (generation, text, warning) = pending_injection.take().unwrap();
                    if generation != self.generation { continue; }
                    let desktop = self.desktop.clone();
                    let target = self.target.clone();
                    let tx = completion_tx.clone();
                    self.job = Some(tokio::spawn(async move {
                        let result = desktop.inject(&text, &target).await;
                        let _ = tx.send(Completion::Injected { generation, result, warning }).await;
                    }));
                }
                _ = levels.tick(), if self.state == State::Listening => self.publish(),
                _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await } }, if deadline.is_some() => {
                    if let Err(error) = self.stop(completion_tx.clone()).await { self.started = None; self.set_state(State::Error, Some(error.to_string())); }
                }
            }
        }
        let _ = self.cancel().await;
    }
}

fn bounded_message(message: &str) -> String {
    const MAX_BYTES: usize = 4096;
    if message.len() <= MAX_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_BYTES - 3;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &message[..end])
}

fn validate_last_frame(text: &str) -> Result<()> {
    // Last can be requested in any state. Reserve the largest possible escaped
    // status message (4 KiB of control characters) plus numeric/state overhead.
    let mut response = Response::status(State::Processing, 0.0);
    response.message = Some("\0".repeat(4096));
    response.text = Some(text.to_owned());
    if serde_json::to_vec(&response)?.len() + 128 >= MAX_MESSAGE_BYTES {
        bail!("transcript exceeds IPC last-response limit after JSON encoding");
    }
    Ok(())
}

struct SocketGuard {
    path: std::path::PathBuf,
    _lock: File,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub async fn serve(config: Config) -> Result<()> {
    config.validate()?;
    let directory = crate::paths::runtime_dir()?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("xflowd is already running")?;
    let path = directory.join("daemon.sock");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_socket() {
                bail!("refusing to replace non-socket {}", path.display());
            }
            std::fs::remove_file(&path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(&path)?;
    let _guard = SocketGuard { path, _lock: lock };
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&_guard.path, std::fs::Permissions::from_mode(0o600))?;
    let stt = xflow_providers::build_stt(&config.stt, config.privacy.offline)?;
    let transformer = xflow_providers::build_transformer(&config.cleanup, config.privacy.offline)?;
    let audio = Arc::new(xflow_platform::CpalCapture::new(&config.recording)?);
    let desktop = Arc::new(xflow_platform::LinuxDesktop::new(&config.injection));
    let store = Store::open(
        &crate::paths::data_dir()?.join("history.db"),
        config.privacy.history,
        config.privacy.history_limit,
    )?;
    let (commands_tx, commands_rx) = mpsc::channel(32);
    let (events_tx, _) = broadcast::channel(64);
    let (desktop_tx, desktop_rx) = broadcast::channel(32);
    let bridge_commands = commands_tx.downgrade();
    let bridge = tokio::spawn(async move {
        if xflow_platform::run_desktop_bridge(desktop_rx, bridge_commands)
            .await
            .is_err()
        {
            eprintln!("xflowd: desktop bridge unavailable; CLI remains usable");
        }
    });
    let engine = Engine::new(
        config,
        Services {
            audio,
            desktop,
            stt,
            transformer,
        },
        store,
        events_tx.clone(),
        desktop_tx,
    );
    let mut engine_task = tokio::spawn(engine.run(commands_rx));
    let capacity = Arc::new(tokio::sync::Semaphore::new(32));
    let mut clients = tokio::task::JoinSet::new();
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } { continue; }
                let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                let commands = commands_tx.clone(); let events = events_tx.clone();
                clients.spawn(async move { let _permit = permit; let _ = connection(stream, commands, events).await; });
            }
            _ = clients.join_next(), if !clients.is_empty() => (),
            result = &mut engine_task => { result?; break; }
            _ = interrupt.recv() => break,
            _ = terminate.recv() => break,
        }
    }
    drop(commands_tx);
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    // Engine is already finished for Shutdown, otherwise channel closure drives cleanup.
    if !engine_task.is_finished() {
        tokio::time::timeout(Duration::from_secs(5), &mut engine_task)
            .await
            .context("daemon cleanup timed out")??;
    }
    bridge.abort();
    Ok(())
}
async fn connection(
    stream: UnixStream,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Response>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let request: Request = tokio::time::timeout(Duration::from_secs(3), read_frame(&mut reader))
        .await??
        .context("empty IPC request")?;
    let subscribe = matches!(request, Request::Subscribe);
    // Subscribe first, then get an actor snapshot, avoiding missed state transitions.
    let mut receiver = events.subscribe();
    let (reply_tx, reply_rx) = oneshot::channel();
    commands
        .send(Command {
            request,
            reply: reply_tx,
        })
        .await?;
    write_frame(&mut write, &reply_rx.await?).await?;
    if subscribe {
        // Keep one read future alive across broadcasts. Dropping read_line after a
        // partial frame would discard bytes already consumed from the socket.
        let closed = read_frame::<Request, _>(&mut reader);
        tokio::pin!(closed);
        loop {
            tokio::select! {
                event = receiver.recv() => match event {
                    Ok(response) => write_frame(&mut write, &response).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
                // Observe client closure even while daemon is idle, without polling.
                result = &mut closed => { let _ = result?; break; }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    };
    use tokio::sync::Notify;
    use xflow_core::{InjectionOutcome, Transcript};

    struct FakeAudio(AtomicBool);
    #[async_trait]
    impl AudioCapture for FakeAudio {
        async fn start(&self) -> Result<()> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
        async fn stop(&self) -> Result<AudioClip> {
            self.0.store(false, Ordering::SeqCst);
            Ok(AudioClip {
                samples: vec![0.2; 1600],
                sample_rate: 16000,
                channels: 1,
            })
        }
        async fn cancel(&self) -> Result<()> {
            self.0.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn level(&self) -> f32 {
            0.2
        }
    }
    struct FailsFirstStop(AtomicBool);
    #[async_trait]
    impl AudioCapture for FailsFirstStop {
        async fn start(&self) -> Result<()> {
            Ok(())
        }
        async fn stop(&self) -> Result<AudioClip> {
            if !self.0.swap(true, Ordering::SeqCst) {
                bail!("no speech detected");
            }
            Ok(AudioClip {
                samples: vec![0.2; 1600],
                sample_rate: 16000,
                channels: 1,
            })
        }
        async fn cancel(&self) -> Result<()> {
            Ok(())
        }
        fn level(&self) -> f32 {
            0.0
        }
    }
    struct FakeDesktop(AtomicUsize);
    #[async_trait]
    impl Desktop for FakeDesktop {
        async fn context(&self) -> Result<AppContext> {
            Ok(AppContext::default())
        }
        async fn inject(&self, _: &str, _: &AppContext) -> Result<InjectionOutcome> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(InjectionOutcome::ClipboardOnly)
        }
        async fn copy(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }
    struct FakeStt {
        called: Arc<Notify>,
        release: Arc<Notify>,
        options: Arc<Mutex<Vec<TranscriptionOptions>>>,
    }
    #[async_trait]
    impl SpeechToText for FakeStt {
        fn name(&self) -> &str {
            "fixture"
        }
        async fn transcribe(
            &self,
            _: AudioClip,
            options: TranscriptionOptions,
        ) -> Result<Transcript> {
            self.options.lock().unwrap().push(options);
            self.called.notify_one();
            self.release.notified().await;
            Ok(Transcript {
                text: "Postgres is ready.".into(),
                language: Some("en".into()),
            })
        }
    }
    struct FailingCleanup;
    #[async_trait]
    impl TextTransformer for FailingCleanup {
        async fn transform(&self, _: TransformRequest<'_>) -> Result<String> {
            bail!("provider unavailable")
        }
    }
    async fn ask(tx: &mpsc::Sender<Command>, request: Request) -> Response {
        let (reply, receive) = oneshot::channel();
        tx.send(Command { request, reply }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), receive)
            .await
            .unwrap()
            .unwrap()
    }
    #[tokio::test]
    async fn cancel_during_network_keeps_actor_responsive_and_prevents_injection() {
        let audio = Arc::new(FakeAudio(AtomicBool::new(false)));
        let desktop = Arc::new(FakeDesktop(AtomicUsize::new(0)));
        let called = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let options = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel(8);
        let (events, _) = broadcast::channel(32);
        let (desktop_events, _) = broadcast::channel(32);
        let store = Store::open(std::path::Path::new("unused"), false, 5).unwrap();
        let engine = Engine::new(
            Config::default(),
            Services {
                audio: audio.clone(),
                desktop: desktop.clone(),
                stt: Arc::new(FakeStt {
                    called: called.clone(),
                    release: release.clone(),
                    options,
                }),
                transformer: None,
            },
            store,
            events,
            desktop_events,
        );
        let task = tokio::spawn(engine.run(rx));
        assert_eq!(ask(&tx, Request::start()).await.state, State::Listening);
        assert!(!ask(&tx, Request::start()).await.ok);
        assert_eq!(ask(&tx, Request::Stop).await.state, State::Processing);
        called.notified().await;
        assert_eq!(ask(&tx, Request::Status).await.state, State::Processing);
        assert_eq!(ask(&tx, Request::Cancel).await.state, State::Idle);
        release.notify_one();
        assert!(ask(&tx, Request::Last).await.text.is_none());
        assert!(!audio.0.load(Ordering::SeqCst));
        assert_eq!(desktop.0.load(Ordering::SeqCst), 0);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn cleanup_failure_preserves_transcript_history_and_clipboard_fallback() {
        let audio = Arc::new(FakeAudio(AtomicBool::new(false)));
        let desktop = Arc::new(FakeDesktop(AtomicUsize::new(0)));
        let called = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let options = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel(8);
        let (events, mut receiver) = broadcast::channel(32);
        let (desktop_events, _) = broadcast::channel(32);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db"), true, 5).unwrap();
        let mut config = Config::default();
        config.stt.language = Some("en".into());
        config.stt.vocabulary = vec!["Postgres".into()];
        let engine = Engine::new(
            config,
            Services {
                audio,
                desktop: desktop.clone(),
                stt: Arc::new(FakeStt {
                    called: called.clone(),
                    release: release.clone(),
                    options: options.clone(),
                }),
                transformer: Some(Arc::new(FailingCleanup)),
            },
            store,
            events,
            desktop_events,
        );
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::toggle()).await;
        ask(&tx, Request::toggle()).await;
        called.notified().await;
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while receiver.recv().await.unwrap().state != State::Success {}
        })
        .await
        .unwrap();
        assert_eq!(
            ask(&tx, Request::Last).await.text.as_deref(),
            Some("Postgres is ready.")
        );
        assert_eq!(
            ask(
                &tx,
                Request::History {
                    limit: 5,
                    offset: 0,
                    query: None,
                }
            )
            .await
            .history
            .len(),
            1
        );
        assert_eq!(desktop.0.load(Ordering::SeqCst), 1);
        {
            let observed = options.lock().unwrap();
            assert_eq!(observed.len(), 1);
            assert!(observed[0].language.is_none());
            assert!(observed[0].vocabulary.is_empty());
        }
        ask(&tx, Request::ClearHistory).await;
        assert!(ask(&tx, Request::Last).await.text.is_none());
        assert!(ask(
            &tx,
            Request::History {
                limit: 5,
                offset: 0,
                query: None,
            }
        )
        .await
        .history
        .is_empty());
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }

    #[tokio::test]
    async fn no_speech_error_allows_next_recording() {
        let (tx, rx) = mpsc::channel(8);
        let (events, mut receiver) = broadcast::channel(32);
        let (desktop_events, _) = broadcast::channel(32);
        let called = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let engine = Engine::new(
            Config::default(),
            Services {
                audio: Arc::new(FailsFirstStop(AtomicBool::new(false))),
                desktop: Arc::new(FakeDesktop(AtomicUsize::new(0))),
                stt: Arc::new(FakeStt {
                    called: called.clone(),
                    release: release.clone(),
                    options: Arc::new(Mutex::new(Vec::new())),
                }),
                transformer: None,
            },
            Store::open(std::path::Path::new("unused"), false, 5).unwrap(),
            events,
            desktop_events,
        );
        let task = tokio::spawn(engine.run(rx));
        assert_eq!(ask(&tx, Request::start()).await.state, State::Listening);
        let failed = ask(&tx, Request::Stop).await;
        assert!(!failed.ok);
        assert_eq!(failed.state, State::Error);
        assert_eq!(ask(&tx, Request::toggle()).await.state, State::Listening);
        assert_eq!(ask(&tx, Request::Stop).await.state, State::Processing);
        called.notified().await;
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while receiver.recv().await.unwrap().state != State::Success {}
        })
        .await
        .unwrap();
        assert_eq!(
            ask(&tx, Request::Last).await.text.as_deref(),
            Some("Postgres is ready.")
        );
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }

    #[tokio::test]
    async fn clear_history_cancels_pending_transcript() {
        let (tx, rx) = mpsc::channel(8);
        let (events, _) = broadcast::channel(32);
        let (desktop_events, _) = broadcast::channel(32);
        let called = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let desktop = Arc::new(FakeDesktop(AtomicUsize::new(0)));
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Config::default(),
            Services {
                audio: Arc::new(FakeAudio(AtomicBool::new(false))),
                desktop: desktop.clone(),
                stt: Arc::new(FakeStt {
                    called: called.clone(),
                    release: release.clone(),
                    options: Arc::new(Mutex::new(Vec::new())),
                }),
                transformer: None,
            },
            Store::open(&dir.path().join("db"), true, 5).unwrap(),
            events,
            desktop_events,
        );
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        called.notified().await;
        assert_eq!(ask(&tx, Request::ClearHistory).await.state, State::Idle);
        release.notify_one();
        assert!(ask(&tx, Request::Last).await.text.is_none());
        assert!(ask(
            &tx,
            Request::History {
                limit: 5,
                offset: 0,
                query: None,
            }
        )
        .await
        .history
        .is_empty());
        assert_eq!(desktop.0.load(Ordering::SeqCst), 0);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }

    #[test]
    fn escaped_transcript_must_fit_last_frame() {
        assert!(validate_last_frame(&"a".repeat(32 * 1024)).is_ok());
        assert!(validate_last_frame(&"\0".repeat(12 * 1024)).is_err());
    }
}
