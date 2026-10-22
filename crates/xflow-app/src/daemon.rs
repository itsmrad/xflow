use crate::{
    config_edit::ConfigFile,
    store::Store,
    transport::{read_frame, write_frame},
};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::{
    fs::File,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
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
    config::{Config, DictionaryConfig, HotkeyConfig},
    ipc::{
        Delivery, HistoryEntry, Request, Response, Timings, MAX_MESSAGE_BYTES, PROTOCOL_VERSION,
    },
    AppContext, AudioCapture, AudioClip, CleanupMode, Desktop, DesktopEvent, GlobalHotkeys,
    HotkeyAction, InjectionOutcome, Mode, SpeechToText, State, TextTransformer,
    TranscriptionOptions, TransformRequest,
};

struct Processed {
    text: String,
    target: AppContext,
    entry: Option<HistoryEntry>,
    timings: Timings,
    stopped: Instant,
    warning: Option<String>,
}
const RELOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// Dropping a tap adapter joins its run-loop thread; keep that off the actor.
fn retire(hotkeys: Option<Box<dyn GlobalHotkeys>>) {
    if let Some(hotkeys) = hotkeys {
        tokio::task::spawn_blocking(move || drop(hotkeys));
    }
}
/// Status message while a double-tap has latched recording; the macOS pill keys off it.
pub const HANDS_FREE: &str = "Hands-free recording";
/// Status message once a capture open is slow, typically behind the macOS microphone prompt.
const MIC_WAITING: &str = "Waiting for microphone permission";
const MIC_SLOW: Duration = Duration::from_secs(1);
/// An IPC start still opening after this is refused (the CLI gives up at 9 s), so a
/// late permission grant never records for a client that already left.
const START_GRACE: Duration = Duration::from_secs(5);
struct Opening {
    mode: Mode,
    context: Option<AppContext>,
    t0_us: Option<u64>,
    delivery: Delivery,
    since: Instant,
    /// The IPC requester; None for a hotkey start, which Stop/Cancel abandons.
    reply: Option<oneshot::Sender<Response>>,
    waiting: bool,
    abandoned: bool,
    hands_free: bool,
}
struct Reloaded {
    config: Config,
    services: Services,
    store: Store,
    /// None keeps the running adapter; Err keeps it too and becomes a status warning.
    hotkeys: Option<std::result::Result<Option<Box<dyn GlobalHotkeys>>, String>>,
}
enum Completion {
    Opened(Result<()>),
    Ready {
        generation: u64,
        result: Result<Processed>,
    },
    Finished {
        generation: u64,
        processed: Processed,
        result: Result<Option<InjectionOutcome>>,
    },
    Reloaded {
        result: Box<Result<Reloaded>>,
        reply: oneshot::Sender<Response>,
    },
    /// Learned words were saved to config.toml; `message` says which.
    Learned {
        dictionary: DictionaryConfig,
        message: String,
    },
}
#[derive(Clone)]
pub struct Services {
    pub audio: Arc<dyn AudioCapture>,
    pub desktop: Arc<dyn Desktop>,
    pub stt: Arc<dyn SpeechToText>,
    pub transformer: Option<Arc<dyn TextTransformer>>,
}
// Injectable so reload tests never construct real desktop/audio/provider services.
type ReloadFactory = Arc<dyn Fn(&Config, &Config, Services) -> Result<Services> + Send + Sync>;
type HotkeyFactory =
    Arc<dyn Fn(&HotkeyConfig) -> Result<Option<Box<dyn GlobalHotkeys>>> + Send + Sync>;
pub struct Engine {
    config: Config,
    services: Services,
    store: Store,
    config_path: Option<PathBuf>,
    reload_factory: ReloadFactory,
    hotkey_factory: Option<HotkeyFactory>,
    hotkeys: Option<Box<dyn GlobalHotkeys>>,
    hotkey_warning: Option<String>,
    opening: Option<Opening>,
    /// A hotkey session not yet revealed: capture runs, but no cue or pill.
    quiet: bool,
    reloading: bool,
    delivering: bool,
    state: State,
    mode: Mode,
    delivery: Delivery,
    last: Option<String>,
    message: Option<String>,
    persistence_warning: Option<String>,
    generation: u64,
    epoch: Arc<AtomicU64>,
    job: Option<JoinHandle<()>>,
    context_job: Option<JoinHandle<AppContext>>,
    context_abort: Option<tokio::task::AbortHandle>,
    warm_job: Option<JoinHandle<()>>,
    /// Ends the observation of the last pasted field (see `observe`).
    observing: Option<oneshot::Sender<()>>,
    /// Words learned while a reload was in flight; the reload may have read the file first.
    learned_during_reload: Option<DictionaryConfig>,
    notifications: tokio::task::JoinSet<()>,
    started: Option<Instant>,
    silent_since: Option<Instant>,
    timings: Timings,
    last_timings: Option<Timings>,
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
            services,
            store,
            config_path: None,
            reload_factory: Arc::new(native_services),
            hotkey_factory: None,
            hotkeys: None,
            hotkey_warning: None,
            opening: None,
            quiet: false,
            reloading: false,
            delivering: false,
            state: State::Idle,
            mode: Mode::Dictation,
            delivery: Delivery::Inject,
            last: None,
            message: None,
            persistence_warning: None,
            generation: 0,
            epoch: Arc::new(AtomicU64::new(0)),
            job: None,
            context_job: None,
            context_abort: None,
            warm_job: None,
            observing: None,
            learned_during_reload: None,
            notifications: tokio::task::JoinSet::new(),
            started: None,
            silent_since: None,
            timings: Timings::default(),
            last_timings: None,
            events,
            desktop_events,
        }
    }
    fn status(&self) -> Response {
        let mut r = Response::status(
            self.state,
            if self.state == State::Listening {
                let level = self.services.audio.level();
                if level.is_finite() {
                    level.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            } else {
                0.0
            },
        );
        r.message = match (&self.message, &self.persistence_warning) {
            (Some(message), Some(warning)) => {
                Some(bounded_message(&format!("{message}; {warning}")))
            }
            (None, Some(warning)) => Some(warning.clone()),
            (message, None) => message.clone(),
        };
        if let Some(warning) = &self.hotkey_warning {
            r.message = Some(bounded_message(&match r.message {
                Some(message) => format!("{message}; {warning}"),
                None => warning.clone(),
            }));
        }
        r.version = Some(env!("CARGO_PKG_VERSION").into());
        r.protocol = Some(PROTOCOL_VERSION);
        r.provider = Some(self.services.stt.name().into());
        r.model = Some(self.services.stt.model().to_owned()).filter(|s| !s.is_empty());
        if self.active() {
            r.mode = Some(self.mode);
        } else {
            r.timings = self.last_timings.clone();
        }
        r
    }
    fn active(&self) -> bool {
        matches!(self.state, State::Listening | State::Processing)
    }
    fn reset_hotkeys(&mut self) {
        if let Some(hotkeys) = &mut self.hotkeys {
            hotkeys.reset();
        }
    }
    fn publish(&self) {
        let r = self.status();
        // A quiet (unrevealed) hotkey session is hidden from the pill until Reveal.
        if !(self.quiet && r.state == State::Listening) {
            let _ = self.desktop_events.send(DesktopEvent {
                state: r.state,
                level: r.level,
                message: r.message.clone(),
                mode: self.mode,
            });
        }
        let _ = self.events.send(r);
    }
    fn set_state(&mut self, state: State, message: Option<String>) {
        self.state = state;
        self.quiet &= state == State::Listening;
        self.message = message.map(|s| bounded_message(&s));
        self.publish();
        if state == State::Error {
            log("error", "session failed; see IPC status");
            self.cue(xflow_platform::Cue::Error);
            self.notify(
                "XFlow error",
                "Dictation failed; check xflow status for details.",
            );
        }
    }
    fn cue(&self, cue: xflow_platform::Cue) {
        // Actor fixtures must never play audio on the user's desktop.
        if !cfg!(test) {
            xflow_platform::play(cue, &self.config.sounds);
        }
    }
    fn notify(&mut self, summary: &'static str, body: &str) {
        if !cfg!(test) && self.config.notifications.enabled && self.notifications.len() < 4 {
            let body = body.to_owned();
            self.notifications.spawn(async move {
                let _ = xflow_platform::notify(summary, &body).await;
            });
        }
    }
    fn advance(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.epoch.store(self.generation, Ordering::SeqCst);
    }
    /// Opens capture off the actor: the first open can wait on the macOS microphone
    /// prompt indefinitely. `opened` begins the session once it returns.
    fn start(
        &mut self,
        mode: Mode,
        context: Option<AppContext>,
        t0_us: Option<u64>,
        delivery: Delivery,
        completion: &mpsc::Sender<Completion>,
    ) -> Result<()> {
        if self.active() {
            bail!("a recording is already active");
        }
        if self.opening.is_some() {
            bail!("{MIC_WAITING}; the microphone is still opening");
        }
        if self.delivering {
            bail!("previous delivery is finishing; retry shortly");
        }
        if self.reloading {
            bail!("configuration reload is in progress");
        }
        if mode == Mode::Command && self.services.transformer.is_none() {
            bail!("command mode requires a configured cleanup provider");
        }
        let (audio, tx) = (self.services.audio.clone(), completion.clone());
        tokio::spawn(async move {
            let result = audio.start().await;
            let _ = tx.send(Completion::Opened(result)).await;
        });
        self.opening = Some(Opening {
            mode,
            context,
            t0_us,
            delivery,
            since: Instant::now(),
            reply: None,
            waiting: false,
            abandoned: false,
            hands_free: false,
        });
        Ok(())
    }
    async fn opened(&mut self, result: Result<()>) {
        let Some(mut opening) = self.opening.take() else {
            return;
        };
        let reply = opening.reply.take();
        // Never record for a requester that left, timed out, released the key or canceled.
        let live = !opening.abandoned && reply.as_ref().is_none_or(|r| !r.is_closed());
        let response = match result {
            Ok(()) if live => {
                self.capture(opening);
                self.status()
            }
            result => {
                let _ = self.services.audio.cancel().await;
                match result {
                    Err(error) if live => self.set_state(State::Error, Some(format!("{error:#}"))),
                    _ => self.set_state(State::Idle, None),
                }
                self.reset_hotkeys();
                Response::error(self.state, self.message.as_deref().unwrap_or("canceled"))
            }
        };
        if let Some(reply) = reply {
            let _ = reply.send(response);
        }
    }
    fn capture(&mut self, opening: Opening) {
        let Opening {
            mode,
            context,
            t0_us,
            delivery,
            since: opened,
            hands_free,
            ..
        } = opening;
        // The user is done editing the last dictation; read the field once more.
        if let Some(finalize) = self.observing.take() {
            let _ = finalize.send(());
        }
        self.advance();
        self.mode = mode;
        self.delivery = delivery;
        self.started = Some(Instant::now());
        self.silent_since = self.started;
        self.timings = Timings {
            open_ms: Some(ms(opened.elapsed())),
            hotkey_ms: t0_us
                .and_then(|t| monotonic_us().checked_sub(t))
                .map(|n| (n / 1000).min(u32::MAX as u64) as u32),
            ..Timings::default()
        };
        let stt = self.services.stt.clone();
        if let Some(job) = self.warm_job.take() {
            job.abort();
        }
        self.warm_job = Some(tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(2), stt.warm()).await;
        }));
        let desktop = self.services.desktop.clone();
        // Both queries run after capture opens and outside the command actor.
        let task = tokio::spawn(async move {
            let context = async {
                match context {
                    Some(c) => c,
                    None if delivery == Delivery::None && mode == Mode::Dictation => {
                        AppContext::default()
                    }
                    None => desktop.context().await.unwrap_or_default(),
                }
            };
            let selection = async {
                if mode == Mode::Command {
                    desktop.selection().await.unwrap_or_default()
                } else {
                    None
                }
            };
            let (mut target, selection) = tokio::join!(context, selection);
            if mode == Mode::Command {
                target.selected_text = selection.or(target.selected_text);
            }
            target
        });
        self.context_abort = Some(task.abort_handle());
        self.context_job = Some(task);
        self.set_state(State::Listening, hands_free.then(|| HANDS_FREE.into()));
        if !self.quiet {
            self.cue(xflow_platform::Cue::Start);
        }
    }
    async fn stop(&mut self, completion: mpsc::Sender<Completion>) -> Result<()> {
        if self.state != State::Listening {
            bail!("no recording is active");
        }
        let stopped = Instant::now();
        self.started = None;
        self.silent_since = None;
        let audio = match self.services.audio.stop().await {
            Ok(a) => a,
            Err(e) => {
                let _ = self.services.audio.cancel().await;
                self.abort_context();
                self.set_state(State::Error, Some(format!("{e:#}")));
                return Err(e);
            }
        };
        let audio_ms = audio_duration(&audio)?;
        if audio_ms < self.config.recording.min_ms {
            self.abort_context();
            self.set_state(State::Idle, Some("Recording too short; discarded".into()));
            return Ok(());
        }
        self.timings.audio_ms = audio_ms;
        self.cue(xflow_platform::Cue::Stop);
        self.set_state(State::Processing, None);
        self.transcribe(audio, stopped, completion);
        Ok(())
    }
    fn transcribe(
        &mut self,
        audio: AudioClip,
        stopped: Instant,
        completion: mpsc::Sender<Completion>,
    ) {
        let services = self.services.clone();
        let config = self.config.clone();
        let store = self.store.clone();
        let generation = self.generation;
        let epoch = self.epoch.clone();
        let session_mode = self.mode;
        let mut context = self.context_job.take();
        let mut timings = self.timings.clone();
        self.job = Some(tokio::spawn(async move {
            let result = async {
                let options = TranscriptionOptions {
                    vocabulary: vocabulary(&config),
                    ..TranscriptionOptions::default()
                };
                let transcript = services.stt.transcribe(audio, options).await?;
                timings.stt_ms = ms(stopped.elapsed());
                let raw = transcript.text;
                if raw.trim().is_empty() {
                    bail!("no speech recognized");
                }
                if raw.len() > 32 * 1024 {
                    bail!("transcript exceeds 32 KiB safety limit");
                }
                let target = if let Some(ref mut job) = context {
                    match tokio::time::timeout(Duration::from_secs(2), &mut *job).await {
                        Ok(Ok(c)) => c,
                        _ => {
                            job.abort();
                            AppContext::default()
                        }
                    }
                } else {
                    AppContext::default()
                };
                let selection = target.selected_text.as_deref().unwrap_or("");
                if selection.len() > MAX_MESSAGE_BYTES {
                    bail!("selected text exceeds safety limit");
                }
                let local = xflow_core::text::before_cleanup(&raw, &config);
                let style = xflow_core::text::style(&config, target.app_id.as_deref());
                let mode = style.and_then(|s| s.mode).unwrap_or(config.cleanup.mode);
                let instructions = style
                    .and_then(|s| s.prompt.as_deref())
                    .or(config.cleanup.prompt.as_deref());
                let mut warning = None;
                let text = if let Some(transformer) = services
                    .transformer
                    .filter(|_| session_mode == Mode::Command || mode != CleanupMode::Raw)
                {
                    let cleanup_start = Instant::now();
                    let transformed = transformer
                        .transform(TransformRequest {
                            text: if session_mode == Mode::Command {
                                selection
                            } else {
                                &local
                            },
                            mode: if session_mode == Mode::Command && mode == CleanupMode::Raw {
                                CleanupMode::Light
                            } else {
                                mode
                            },
                            instructions,
                            command: (session_mode == Mode::Command).then_some(local.as_str()),
                            app_id: target
                                .app_id
                                .as_deref()
                                .filter(|_| config.cleanup.app_context),
                            vocabulary: &config.dictionary.words,
                        })
                        .await;
                    timings.cleanup_ms = Some(ms(cleanup_start.elapsed()));
                    match transformed {
                        Ok(s) if !s.trim().is_empty() && s.len() <= 32 * 1024 => s,
                        _ if session_mode == Mode::Command => {
                            bail!("command transformation failed; selection left untouched")
                        }
                        _ => {
                            warning = Some("Cleanup failed; original transcript retained".into());
                            local
                        }
                    }
                } else {
                    local
                };
                let text = xflow_core::text::after_cleanup(&text, &config);
                if text.trim().is_empty() {
                    bail!("no speech recognized after formatting");
                }
                validate_last_frame(&text)?;
                let entry = HistoryEntry {
                    text: text.clone(),
                    provider: services.stt.name().into(),
                    model: Some(services.stt.model().into()).filter(|s: &String| !s.is_empty()),
                    raw_text: (raw != text).then_some(raw),
                    app_id: target.app_id.clone(),
                    language: transcript.language.or(config.stt.language),
                    duration_ms: Some(timings.audio_ms as u64),
                    mode: session_mode,
                    ..HistoryEntry::default()
                };
                // A generation check under the SQLite lock prevents a canceled save from
                // resurrecting history after clear_history has returned.
                let entry = match store.append_current(entry, epoch, generation).await {
                    Ok(entry) => entry,
                    Err(_) => {
                        warning = Some("History save failed; transcript available via last".into());
                        None
                    }
                };
                Ok(Processed {
                    text,
                    target,
                    entry,
                    timings,
                    stopped,
                    warning,
                })
            }
            .await;
            let _ = completion
                .send(Completion::Ready { generation, result })
                .await;
        }));
    }
    fn abort_context(&mut self) {
        if let Some(job) = self.context_abort.take() {
            job.abort();
        }
        self.context_job = None;
        if let Some(job) = self.warm_job.take() {
            job.abort();
        }
    }
    async fn cancel(&mut self) -> Result<()> {
        self.advance();
        if let Some(job) = self.job.take() {
            job.abort();
        }
        self.abort_context();
        self.started = None;
        self.silent_since = None;
        // An open in flight still owns the audio thread; `opened` discards it instead.
        if let Some(opening) = &mut self.opening {
            opening.abandoned = true;
        } else if let Err(e) = self.services.audio.cancel().await {
            self.set_state(State::Error, Some(format!("{e:#}")));
            return Err(e);
        }
        self.set_state(State::Idle, None);
        Ok(())
    }
    /// `Ok(None)` defers the reply until the microphone opens (see `opened`).
    async fn handle(
        &mut self,
        request: Request,
        completion: &mpsc::Sender<Completion>,
    ) -> Result<Option<Response>> {
        if let Some(opening) = self.opening.as_mut().filter(|_| {
            matches!(
                request,
                Request::Stop | Request::Cancel | Request::Toggle { .. }
            )
        }) {
            // Released or canceled before capture began: this session never records.
            opening.abandoned = true;
            if let Some(reply) = opening.reply.take() {
                let _ = reply.send(Response::error(self.state, "canceled"));
            }
            return Ok(Some(self.status()));
        }
        match request {
            Request::Start {
                mode,
                context,
                t0_us,
                delivery,
            } => {
                self.start(mode, context, t0_us, delivery, completion)?;
                return Ok(None);
            }
            Request::Stop => self.stop(completion.clone()).await?,
            Request::Toggle {
                mode,
                context,
                t0_us,
                delivery,
            } => {
                if self.state == State::Listening {
                    self.stop(completion.clone()).await?;
                } else {
                    self.start(mode, context, t0_us, delivery, completion)?;
                    return Ok(None);
                }
            }
            Request::Cancel => self.cancel().await?,
            Request::Last => {
                let mut r = self.status();
                r.text = self.last.clone();
                return Ok(Some(r));
            }
            Request::Status | Request::Subscribe | Request::Shutdown => (),
            _ => bail!("invalid actor request"),
        }
        Ok(Some(self.status()))
    }
    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let (completion_tx, mut completion_rx) = mpsc::channel(8);
        let mut levels = tokio::time::interval(Duration::from_millis(40));
        levels.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending: Option<Processed> = None;
        let mut queries = tokio::task::JoinSet::new();
        let mut deliveries = tokio::task::JoinSet::new();
        let mut desktop_ops = tokio::task::JoinSet::new();
        loop {
            let deadline = self.started.map(|s| {
                tokio::time::Instant::from_std(
                    s + Duration::from_secs(self.config.recording.max_seconds as u64),
                )
            });
            // First the waiting message, then (IPC only) the start grace.
            let opening = self
                .opening
                .as_ref()
                .filter(|o| !o.abandoned)
                .and_then(|o| {
                    if o.waiting {
                        o.reply.as_ref().map(|_| o.since + START_GRACE)
                    } else {
                        Some(o.since + MIC_SLOW)
                    }
                });
            tokio::select! { biased;
                command = commands.recv() => {
                    let Some(Command { request, reply }) = command else { break; };
                    if reply.is_closed() { continue; }
                    while queries.try_join_next().is_some() {}
                    while desktop_ops.try_join_next().is_some() {}
                    if (is_query(&request) || matches!(request, Request::Reload)) && queries.len() + desktop_ops.len() >= 32 { let _ = reply.send(Response::error(self.state, "daemon request capacity reached")); continue; }
                    if let Request::HistoryUpdate { id, text } = request {
                        let status = self.status(); let store = self.store.clone(); let path = self.config_path.clone(); let tx = completion_tx.clone();
                        queries.spawn(async move { let state = status.state; let response = edit_history(status, store, id, text, path, tx).await.unwrap_or_else(|e| Response::error(state, format!("{e:#}"))); let _ = reply.send(response); });
                        continue;
                    }
                    if matches!(request, Request::Reload) {
                        if self.active() || self.reloading || self.opening.is_some() { let _ = reply.send(Response::error(self.state, "reload requires an idle daemon")); continue; }
                        let Some(path) = self.config_path.clone() else { let _ = reply.send(Response::error(self.state, "reload config path is unavailable")); continue; };
                        self.reloading = true;
                        let old = self.config.clone(); let services = self.services.clone(); let store = self.store.clone(); let factory = self.reload_factory.clone(); let hotkey_factory = self.hotkey_factory.clone(); let retry_hotkeys = self.hotkeys.is_none() || self.hotkey_warning.is_some(); let tx = completion_tx.clone();
                        queries.spawn(async move {
                            // Bounded so a wedged provider/tap setup cannot leave `reloading` stuck.
                            let result = tokio::time::timeout(RELOAD_TIMEOUT, tokio::task::spawn_blocking(move || {
                                let config = Config::load(&path)?;
                                let services = factory(&old, &config, services)?;
                                // Recreate the tap only when its config changed or it is missing (e.g. permission granted since).
                                let hotkeys = hotkey_factory.filter(|_| old.hotkey != config.hotkey || retry_hotkeys).map(|factory| factory(&config.hotkey).map_err(|e| bounded_message(&format!("{e:#}"))));
                                let store = if store.settings() == (config.privacy.history, config.privacy.history_limit) { store } else { store.reopen(config.privacy.history, config.privacy.history_limit)? };
                                Ok(Reloaded { config, services, store, hotkeys })
                            })).await.unwrap_or_else(|_| Ok(Err(anyhow::anyhow!("configuration reload timed out")))).unwrap_or_else(|e| Err(e.into()));
                            let _ = tx.send(Completion::Reloaded { result: Box::new(result), reply }).await;
                        }); continue;
                    }
                    if is_query(&request) {
                        if is_desktop_query(&request) && (!desktop_ops.is_empty() || self.delivering) { let _ = reply.send(Response::error(self.state, "desktop delivery is busy")); continue; }
                        if matches!(request, Request::PasteLast | Request::HistoryPaste { .. }) && self.active() { let _ = reply.send(Response::error(self.state, "wait for the current recording before pasting")); continue; }
                        if matches!(request, Request::ClearHistory) {
                            if self.active() { let _ = self.cancel().await; } else { self.advance(); }
                            self.last = None; self.last_timings = None; pending = None;
                        }
                        let status = self.status(); let store = self.store.clone(); let desktop = self.services.desktop.clone(); let last = self.last.clone();
                        let desktop_query = is_desktop_query(&request);
                        let work = async move { let state = status.state; let response = query(status, request, store, desktop, last).await.unwrap_or_else(|e| Response::error(state, format!("{e:#}"))); let _ = reply.send(response); };
                        if desktop_query { desktop_ops.spawn(work); } else { queries.spawn(work); }
                        continue;
                    }
                    if matches!(request, Request::Start { .. } | Request::Toggle { .. }) && !self.active() && !desktop_ops.is_empty() { let _ = reply.send(Response::error(self.state, "desktop delivery is busy")); continue; }
                    let shutdown = matches!(request, Request::Shutdown);
                    match self.handle(request, &completion_tx).await {
                        Ok(Some(response)) => { let _ = reply.send(response); }
                        Ok(None) => if let Some(opening) = &mut self.opening { opening.reply = Some(reply); },
                        Err(e) => { let _ = reply.send(Response::error(self.state, format!("{e:#}"))); }
                    }
                    // A hotkey open in flight still needs the gesture's release (Stop).
                    if self.state != State::Listening && self.opening.is_none() { self.reset_hotkeys(); }
                    if shutdown { break; }
                }
                Some(completion) = completion_rx.recv() => match completion {
                    Completion::Opened(result) => self.opened(result).await,
                    Completion::Ready { generation, result } if generation == self.generation => {
                        self.job = None;
                        match result { Ok(processed) => { self.last = Some(processed.text.clone()); pending = Some(processed); }, Err(e) => { self.abort_context(); self.set_state(State::Error, Some(format!("{e:#}"))); } }
                    }
                    Completion::Finished { generation, mut processed, result } => {
                        self.delivering = false;
                        if generation != self.generation { continue; }
                        self.abort_context();
                        match result {
                            Ok(outcome) => {
                                if outcome == Some(InjectionOutcome::ClipboardOnly) { self.notify("XFlow transcript copied", "Paste manually to insert your dictation."); }
                                let message = delivery_message(processed.warning.take(), outcome);
                                // One final success event: subscribers can trust its complete payload.
                                self.last_timings = Some(processed.timings.clone());
                                if matches!(outcome, Some(InjectionOutcome::Pasted | InjectionOutcome::Typed)) && self.mode == Mode::Dictation && self.config.dictionary.auto_learn {
                                    if let Some(path) = self.config_path.clone() {
                                        let (finalize, finalized) = oneshot::channel();
                                        self.observing = Some(finalize);
                                        tokio::spawn(observe(self.services.desktop.clone(), processed.target.clone(), processed.text.clone(), finalized, path, completion_tx.clone()));
                                    }
                                }
                                self.state = State::Success; self.message = message; let mut r = self.status();
                                r.text = Some(processed.text); r.injection = outcome; r.entry = processed.entry; r.timings = Some(processed.timings);
                                fit_final(&mut r);
                                let _ = self.desktop_events.send(DesktopEvent { state: self.state, level: 0.0, message: self.message.clone(), mode: self.mode });
                                let _ = self.events.send(r);
                            }
                            Err(e) => self.set_state(State::Error, Some(format!("Injection failed: {e}; transcript available via last"))),
                        }
                    }
                    Completion::Reloaded { result, reply } => {
                        self.reloading = false;
                        let learned = self.learned_during_reload.take();
                        let r = match *result { Ok(Reloaded { config, services, store, hotkeys }) => { self.config = config; if let Some(dictionary) = learned { self.config.dictionary = dictionary; } self.services = services; self.store = store; match hotkeys { Some(Ok(hotkeys)) => { retire(std::mem::replace(&mut self.hotkeys, hotkeys)); if self.hotkey_warning.take().is_some() && self.state == State::Error && self.message.is_none() { self.state = State::Idle; } } Some(Err(warning)) => self.hotkey_warning = Some(warning), None => () } self.persistence_warning = None; self.publish(); self.status() }, Err(e) => Response::error(self.state, format!("{e:#}")) };
                        let _ = reply.send(r);
                    }
                    Completion::Learned { dictionary, message } => {
                        // Event only: the state and status message stay with the current session.
                        if self.reloading { self.learned_during_reload = Some(dictionary.clone()); }
                        self.config.dictionary = dictionary;
                        let mut r = self.status(); r.message = Some(message.clone()); let _ = self.events.send(r);
                        self.notify("XFlow dictionary", &message);
                    }
                    _ => (),
                },
                _ = async {}, if pending.is_some() => {
                    let mut processed = pending.take().unwrap();
                    if self.state != State::Processing { continue; }
                    let generation = self.generation; let delivery = self.delivery; let desktop = self.services.desktop.clone(); let store = self.store.clone(); let tx = completion_tx.clone();
                    self.delivering = true;
                    deliveries.spawn(async move {
                        let start = Instant::now();
                        let result = match delivery { Delivery::Inject => desktop.inject(&processed.text, &processed.target).await.map(Some), Delivery::Clipboard => desktop.copy(&processed.text).await.map(|_| Some(InjectionOutcome::ClipboardOnly)), Delivery::None => Ok(None) };
                        if delivery != Delivery::None { processed.timings.inject_ms = Some(ms(start.elapsed())); }
                        processed.timings.total_ms = ms(processed.stopped.elapsed());
                        if let Some(entry) = &mut processed.entry { entry.latency_ms = Some(processed.timings.total_ms as u64); if store.set_latency(entry.id, processed.timings.total_ms as u64).await.is_err() { processed.warning = Some("History latency update failed".into()); } }
                        let _ = tx.send(Completion::Finished { generation, processed, result }).await;
                    });
                }
                _ = queries.join_next(), if !queries.is_empty() => (),
                _ = deliveries.join_next(), if !deliveries.is_empty() => (),
                _ = desktop_ops.join_next(), if !desktop_ops.is_empty() => (),
                _ = self.notifications.join_next(), if !self.notifications.is_empty() => (),
                action = async { self.hotkeys.as_mut().unwrap().next_action().await }, if self.hotkeys.is_some() => {
                    match action {
                        Ok(action @ (HotkeyAction::Reveal | HotkeyAction::HandsFree)) => {
                            if let Some(opening) = &mut self.opening {
                                opening.hands_free |= action == HotkeyAction::HandsFree;
                                self.quiet = false;
                            } else if self.state == State::Listening {
                                if action == HotkeyAction::HandsFree { self.message = Some(HANDS_FREE.into()); }
                                if std::mem::take(&mut self.quiet) { self.cue(xflow_platform::Cue::Start); }
                                self.publish();
                            }
                        }
                        Ok(HotkeyAction::Start) if !desktop_ops.is_empty() => self.reset_hotkeys(),
                        Ok(action @ (HotkeyAction::Start | HotkeyAction::Stop | HotkeyAction::Cancel)) => {
                            // Capture starts at once; the cue and pill wait for Reveal, so a
                            // lone tap or a Fn+key shortcut shows and plays nothing.
                            self.quiet = action == HotkeyAction::Start && !self.active();
                            let request = match action {
                                HotkeyAction::Start => Request::Start { mode: Mode::Dictation, context: None, t0_us: Some(monotonic_us()), delivery: Delivery::Inject },
                                HotkeyAction::Stop => Request::Stop,
                                _ => Request::Cancel,
                            };
                            if self.handle(request, &completion_tx).await.is_err() { self.quiet = false; self.reset_hotkeys(); }
                        }
                        Ok(_) => (), // Toggle/PasteLast are never produced by the gesture machine.
                        Err(error) => {
                            retire(self.hotkeys.take());
                            self.hotkey_warning = Some(bounded_message(&format!("{error:#}")));
                            if !self.active() { self.state = State::Error; }
                            self.publish();
                        }
                    }
                }
                _ = levels.tick(), if self.state == State::Listening => {
                    if self.services.audio.finished() {
                        if let Err(e) = self.stop(completion_tx.clone()).await { self.set_state(State::Error, Some(format!("{e:#}"))); }
                        self.reset_hotkeys();
                        continue;
                    }
                    self.publish(); let level = self.services.audio.level();
                    if level.is_finite() && level > self.config.recording.silence_threshold { self.silent_since = None; }
                    else { self.silent_since.get_or_insert_with(Instant::now); }
                    if self.config.recording.auto_stop_secs > 0 && self.silent_since.is_some_and(|s| s.elapsed() >= Duration::from_secs(self.config.recording.auto_stop_secs as u64)) {
                        if let Err(e) = self.stop(completion_tx.clone()).await { self.set_state(State::Error, Some(format!("{e:#}"))); }
                        self.reset_hotkeys();
                    }
                }
                _ = async { if let Some(at) = opening { tokio::time::sleep_until(at.into()).await; } }, if opening.is_some() => {
                    if let Some(opening) = &mut self.opening {
                        if !std::mem::replace(&mut opening.waiting, true) {
                            self.set_state(State::Idle, Some(MIC_WAITING.into()));
                        } else if let Some(reply) = opening.reply.take() {
                            opening.abandoned = true;
                            let _ = reply.send(Response::error(self.state, format!("{MIC_WAITING}. Allow xflowd to use the microphone, then start again")));
                        }
                    }
                }
                _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } }, if deadline.is_some() => {
                    if let Err(e) = self.stop(completion_tx.clone()).await { self.set_state(State::Error, Some(format!("{e:#}"))); }
                    self.reset_hotkeys();
                }
            }
        }
        drop(completion_rx);
        queries.abort_all();
        self.notifications.abort_all();
        let _ = self.cancel().await;
        // Once delivery starts, finish its key release even after cancel/shutdown.
        while deliveries.join_next().await.is_some() {}
        while desktop_ops.join_next().await.is_some() {}
    }
}
fn is_desktop_query(r: &Request) -> bool {
    matches!(
        r,
        Request::CopyLast
            | Request::PasteLast
            | Request::HistoryCopy { .. }
            | Request::HistoryPaste { .. }
    )
}
fn is_query(r: &Request) -> bool {
    matches!(
        r,
        Request::CopyLast
            | Request::PasteLast
            | Request::History { .. }
            | Request::HistoryGet { .. }
            | Request::HistoryDelete { .. }
            | Request::HistoryCopy { .. }
            | Request::HistoryPaste { .. }
            | Request::HistoryUpdate { .. }
            | Request::ClearHistory
            | Request::Stats
    )
}
async fn query(
    mut r: Response,
    request: Request,
    store: Store,
    desktop: Arc<dyn Desktop>,
    last: Option<String>,
) -> Result<Response> {
    match request {
        Request::History {
            limit,
            offset,
            query,
        } => {
            let (rows, total) = store.history(limit, offset, query).await?;
            r.total = Some(total);
            fit_history(&mut r, rows)?;
        }
        Request::HistoryGet { id } => {
            r.entry = Some(store.get(id).await?.context("history entry not found")?);
            if serde_json::to_vec(&r)?.len() + 1 > MAX_MESSAGE_BYTES {
                bail!("history entry exceeds IPC frame limit");
            }
        }
        Request::HistoryDelete { id } => {
            if !store.delete(id).await? {
                bail!("history entry not found");
            }
        }
        Request::HistoryCopy { id } | Request::HistoryPaste { id } => {
            let entry = store.get(id).await?.context("history entry not found")?;
            if matches!(request, Request::HistoryCopy { .. }) {
                desktop.copy(&entry.text).await?;
            } else {
                let target = desktop.context().await.unwrap_or_default();
                r.injection = Some(desktop.inject(&entry.text, &target).await?);
            }
        }
        Request::CopyLast => {
            desktop
                .copy(last.as_deref().context("no transcript yet")?)
                .await?
        }
        Request::PasteLast => {
            let target = desktop.context().await.unwrap_or_default();
            r.injection = Some(
                desktop
                    .inject(last.as_deref().context("no transcript yet")?, &target)
                    .await?,
            );
        }
        Request::ClearHistory => store.clear().await?,
        Request::Stats => r.stats = Some(store.stats().await?),
        _ => bail!("invalid history request"),
    }
    Ok(r)
}
/// Keep the longest prefix of `rows` that fits one IPC frame, serializing each row once.
fn fit_history(r: &mut Response, rows: Vec<HistoryEntry>) -> Result<()> {
    r.history.clear();
    let mut used = serde_json::to_vec(r)?.len() + 1;
    if used > MAX_MESSAGE_BYTES {
        bail!("history response exceeds frame limit");
    }
    // `,"history":[]` plus one comma or newline byte per row.
    used += r#","history":[]"#.len() - 1;
    let mut fit = 0;
    for row in &rows {
        used += serde_json::to_vec(row)?.len() + 1;
        if used > MAX_MESSAGE_BYTES {
            break;
        }
        fit += 1;
    }
    r.history = rows;
    r.history.truncate(fit);
    Ok(())
}
/// Watch the field a dictation was pasted into, without polling: read it ~120 ms
/// after the paste to anchor the inserted span, then once more when the next
/// dictation starts or after 60 s. Only the span and 16 characters on each side
/// are kept in between.
// ponytail: focus changes are noticed at the final read only; an AXObserver
// (or AT-SPI on Linux) could end the window earlier.
async fn observe(
    desktop: Arc<dyn Desktop>,
    target: AppContext,
    inserted: String,
    finalize: oneshot::Receiver<()>,
    path: PathBuf,
    completion: mpsc::Sender<Completion>,
) {
    tokio::time::sleep(Duration::from_millis(120)).await;
    let Some(receipt) = focused_field(&*desktop, &target)
        .await
        .and_then(|field| xflow_core::learn::Receipt::new(&field, &inserted))
    else {
        return;
    };
    drop(inserted);
    tokio::select! {
        // A dropped sender means the daemon is stopping.
        finalized = finalize => if finalized.is_err() { return; },
        _ = tokio::time::sleep(Duration::from_secs(60)) => (),
    }
    let Some(corrected) = focused_field(&*desktop, &target)
        .await
        .and_then(|field| receipt.corrected(&field).map(str::to_owned))
    else {
        return;
    };
    if learn(path, receipt.inserted().to_owned(), corrected, completion)
        .await
        .is_err()
    {
        log("warn", "could not save learned dictionary words");
    }
}
/// Text of the focused field while focus is still on the paste target; 100 ms budget.
async fn focused_field(desktop: &dyn Desktop, target: &AppContext) -> Option<String> {
    tokio::time::timeout(Duration::from_millis(100), async {
        let (now, text) = desktop.focused_text().await.ok()?;
        if now.app_id != target.app_id || now.window_id != target.window_id {
            return None;
        }
        text
    })
    .await
    .ok()
    .flatten()
}
/// The one learning path for corrected pastes and history edits: find close word
/// corrections, save them to config.toml, and hand the new dictionary to the actor.
/// Returns the user-facing message.
async fn learn(
    path: PathBuf,
    original: String,
    corrected: String,
    completion: mpsc::Sender<Completion>,
) -> Result<Option<String>> {
    let learned = tokio::task::spawn_blocking(move || -> Result<_> {
        let _lock = crate::config_edit::lock(&path)?;
        let mut file = ConfigFile::load(&path)?;
        let mut dictionary = file.config()?.dictionary;
        if !dictionary.auto_learn {
            return Ok(None);
        }
        let found = xflow_core::learn::corrections(&original, &corrected, &dictionary);
        let added = dictionary.learn(&found);
        if added.is_empty() {
            return Ok(None);
        }
        file.set_typed("dictionary.words", &dictionary.words)?;
        file.set_typed("dictionary.learned", &dictionary.learned)?;
        file.save()?;
        Ok(Some((dictionary, added)))
    })
    .await??;
    let Some((dictionary, added)) = learned else {
        return Ok(None);
    };
    let words: Vec<_> = added.iter().map(|w| format!("'{w}'")).collect();
    let message = format!("Added {} to your dictionary", words.join(", "));
    let _ = completion
        .send(Completion::Learned {
            dictionary,
            message: message.clone(),
        })
        .await;
    Ok(Some(message))
}
/// Save a corrected history entry, then learn from the correction.
async fn edit_history(
    mut r: Response,
    store: Store,
    id: i64,
    text: String,
    path: Option<PathBuf>,
    completion: mpsc::Sender<Completion>,
) -> Result<Response> {
    if text.trim().is_empty() || text.len() > 32 * 1024 {
        bail!("corrected text must be non-empty and at most 32 KiB");
    }
    let original = store
        .update(id, text.clone())
        .await?
        .context("history entry not found")?;
    r.entry = store.get(id).await?;
    // Command-mode output is the model's text, not speech recognition; nothing to learn.
    let dictated = r.entry.as_ref().is_some_and(|e| e.mode == Mode::Dictation);
    if let Some(path) = path.filter(|_| dictated) {
        r.message = learn(path, original, text, completion)
            .await
            .unwrap_or_else(|e| Some(format!("Transcript saved; dictionary not updated: {e:#}")));
    }
    fit_final(&mut r);
    Ok(r)
}
fn delivery_message(warning: Option<String>, outcome: Option<InjectionOutcome>) -> Option<String> {
    if outcome == Some(InjectionOutcome::ClipboardOnly) {
        Some(bounded_message(&match warning {
            Some(warning) => format!("{warning}; Transcript copied; paste manually"),
            None => "Transcript copied; paste manually".into(),
        }))
    } else {
        warning.map(|s| bounded_message(&s))
    }
}
fn native_services(old: &Config, config: &Config, services: Services) -> Result<Services> {
    let stt = xflow_providers::build_stt(&config.stt, config.privacy.offline)?;
    let transformer = build_transformer(config)?;
    #[cfg(feature = "test-support")]
    if let Some((audio, desktop)) = crate::test_support::adapters(&config.recording)? {
        return Ok(Services {
            audio,
            desktop,
            stt,
            transformer,
        });
    }
    let audio = if old.recording != config.recording {
        Arc::new(xflow_platform::CpalCapture::new(&config.recording)?) as Arc<dyn AudioCapture>
    } else {
        services.audio
    };
    Ok(Services {
        audio,
        desktop: Arc::new(xflow_platform::NativeDesktop::new(&config.injection)),
        stt,
        transformer,
    })
}
pub(crate) fn build_transformer(config: &Config) -> Result<Option<Arc<dyn TextTransformer>>> {
    let mut cleanup = config.cleanup.clone();
    if cleanup.mode == CleanupMode::Raw
        && (cleanup.provider.is_some()
            || cleanup.endpoint.is_some()
            || config
                .styles
                .iter()
                .any(|s| s.mode.is_some_and(|m| m != CleanupMode::Raw)))
    {
        cleanup.mode = CleanupMode::Light;
    }
    xflow_providers::build_transformer(&cleanup, config.privacy.offline)
}

fn audio_duration(audio: &AudioClip) -> Result<u32> {
    if audio.sample_rate == 0 || audio.channels == 0 {
        bail!("invalid audio stream configuration");
    }
    Ok(
        ((audio.samples.len() as u64 / audio.channels as u64).saturating_mul(1000)
            / audio.sample_rate as u64)
            .min(u32::MAX as u64) as u32,
    )
}
fn vocabulary(config: &Config) -> Vec<String> {
    if config.dictionary.words.is_empty() {
        return vec![];
    }
    let mut words = config.stt.vocabulary.clone();
    for word in &config.dictionary.words {
        if !words.iter().any(|old| old.eq_ignore_ascii_case(word)) {
            words.push(word.clone());
        }
    }
    words
}
fn ms(d: Duration) -> u32 {
    d.as_millis().min(u32::MAX as u128) as u32
}
fn monotonic_us() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) } != 0 {
        return 0;
    }
    (t.tv_sec as u64)
        .saturating_mul(1_000_000)
        .saturating_add(t.tv_nsec as u64 / 1000)
}
fn log(level: &str, message: &str) {
    let rank = |s| match s {
        "error" => 0,
        "warn" => 1,
        "info" => 2,
        "debug" => 3,
        _ => 1,
    };
    if rank(level) <= rank(&std::env::var("XFLOW_LOG").unwrap_or_else(|_| "warn".into())) {
        eprintln!("xflowd [{level}]: {message}");
    }
}
fn fit_final(r: &mut Response) {
    if serde_json::to_vec(r).is_ok_and(|v| v.len() + 1 > MAX_MESSAGE_BYTES) {
        if let Some(entry) = &mut r.entry {
            entry.raw_text = None;
        }
        if serde_json::to_vec(r).is_ok_and(|v| v.len() + 1 > MAX_MESSAGE_BYTES) {
            r.entry = None;
        }
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
    let mut r = Response::status(State::Processing, 0.0);
    r.message = Some("\0".repeat(4096));
    r.text = Some(text.to_owned());
    if serde_json::to_vec(&r)?.len() + 1024 >= MAX_MESSAGE_BYTES {
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
    serve_with_path(config, crate::paths::config_path()?).await
}

pub async fn serve_with_path(config: Config, config_path: PathBuf) -> Result<()> {
    config.validate()?;
    #[cfg(feature = "test-support")]
    let test_adapters = crate::test_support::adapters(&config.recording)?;
    #[cfg(all(target_os = "macos", feature = "test-support"))]
    let native = test_adapters.is_none();
    #[cfg(all(target_os = "macos", not(feature = "test-support")))]
    let native = true;
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
    let transformer = build_transformer(&config)?;
    let native_adapters = || -> Result<(Arc<dyn AudioCapture>, Arc<dyn Desktop>)> {
        Ok((
            Arc::new(xflow_platform::CpalCapture::new(&config.recording)?),
            Arc::new(xflow_platform::NativeDesktop::new(&config.injection)),
        ))
    };
    #[cfg(feature = "test-support")]
    let (audio, desktop) = test_adapters.map(Ok).unwrap_or_else(native_adapters)?;
    #[cfg(not(feature = "test-support"))]
    let (audio, desktop) = native_adapters()?;
    let history_path = crate::paths::data_dir()?.join("history.db");
    let (store, persistence_warning) = match Store::open(
        &history_path,
        config.privacy.history,
        config.privacy.history_limit,
    ) {
        Ok(store) => (store, None),
        Err(error) => {
            log(
                "warn",
                "history unavailable; original database preserved; dictation remains available",
            );
            (Store::open(&history_path, false, config.privacy.history_limit)?, Some(bounded_message(&format!("History unavailable: {error:#}; original database preserved; repair it and reload"))))
        }
    };
    let (commands_tx, commands_rx) = mpsc::channel(32);
    let (events_tx, _) = broadcast::channel(64);
    let (desktop_tx, desktop_rx) = broadcast::channel(32);
    #[cfg(target_os = "macos")]
    let pill = {
        let (events, config) = (desktop_tx.subscribe(), config_path.clone());
        tokio::spawn(async move {
            if native {
                crate::overlay_pill::run(events, config).await;
            }
        })
    };
    let bridge_commands = commands_tx.downgrade();
    let bridge = tokio::spawn(async move {
        if xflow_platform::run_desktop_bridge(desktop_rx, bridge_commands)
            .await
            .is_err()
        {
            log("warn", "desktop bridge unavailable; CLI remains usable");
        }
    });
    let mut engine = Engine::new(
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
    engine.config_path = Some(config_path);
    engine.persistence_warning = persistence_warning;
    #[cfg(target_os = "macos")]
    {
        if native {
            let factory: HotkeyFactory = Arc::new(|config| {
                if config.enabled {
                    Ok(Some(Box::new(xflow_platform::MacHotkeys::new(
                        config.clone(),
                    )?)))
                } else {
                    Ok(None)
                }
            });
            let (create, initial) = (factory.clone(), engine.config.hotkey.clone());
            let created = tokio::task::spawn_blocking(move || create(&initial)).await;
            match created.unwrap_or_else(|e| Err(e.into())) {
                Ok(hotkeys) => engine.hotkeys = hotkeys,
                Err(error) => {
                    engine.state = State::Error;
                    engine.hotkey_warning = Some(bounded_message(&format!("{error:#}")));
                    log(
                        "warn",
                        "global hotkey unavailable; check xflow status for permission instructions",
                    );
                }
            }
            engine.hotkey_factory = Some(factory);
        }
    }
    let mut engine_task = tokio::spawn(engine.run(commands_rx));
    let capacity = Arc::new(tokio::sync::Semaphore::new(32));
    let mut clients = tokio::task::JoinSet::new();
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut engine_finished = false;
    let mut accept_failing = false;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                // Transient accept errors (EMFILE, ECONNABORTED) must not end the daemon.
                // Log the first error of a burst only; the 50 ms pause bounds the retry rate.
                let (stream, _) = match accepted {
                    Ok(accepted) => { accept_failing = false; accepted }
                    Err(error) => {
                        if !std::mem::replace(&mut accept_failing, true) { log("warn", &format!("IPC accept failed: {error}")); }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                // macOS loses peer credentials when a probe disconnects before accept.
                if !stream.peer_cred().is_ok_and(|peer| peer.uid() == unsafe { libc::geteuid() }) { continue; }
                let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                let commands = commands_tx.clone(); let events = events_tx.clone();
                clients.spawn(async move { let _permit = permit; let _ = connection(stream, commands, events).await; });
            }
            _ = clients.join_next(), if !clients.is_empty() => (),
            result = &mut engine_task => { result?; engine_finished = true; break; }
            _ = hangup.recv() => {
                let (reply, response) = oneshot::channel();
                if commands_tx.try_send(Command { request: Request::Reload, reply }).is_ok() {
                    clients.spawn(async move { if !response.await.is_ok_and(|r| r.ok) { log("warn", "SIGHUP reload refused or failed; running config retained"); } });
                } else { log("warn", "SIGHUP reload could not be queued"); }
            }
            _ = interrupt.recv() => break,
            _ = terminate.recv() => break,
        }
    }
    drop(commands_tx);
    if engine_finished {
        // The actor's reply may still be queued for the socket task. Let replies
        // flush before aborting clients; idle or stalled peers remain bounded.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while clients.join_next().await.is_some() {}
        })
        .await;
    }
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    // Engine is already finished for Shutdown, otherwise channel closure drives cleanup.
    if !engine_task.is_finished() {
        tokio::time::timeout(Duration::from_secs(20), &mut engine_task)
            .await
            .context("daemon cleanup timed out")??;
    }
    bridge.abort();
    #[cfg(target_os = "macos")]
    {
        // Let the Idle event close the helper rather than orphaning its window.
        let _ = tokio::time::timeout(Duration::from_secs(2), pill).await;
    }
    Ok(())
}
async fn connection(
    stream: UnixStream,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Response>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let request: Request =
        match tokio::time::timeout(Duration::from_secs(3), read_frame(&mut reader)).await {
            Ok(Ok(Some(request))) => request,
            Ok(Ok(None)) => return Ok(()),
            _ => {
                let mut response = Response::error(
                    State::Idle,
                    "invalid or unsupported IPC request; check client and daemon versions",
                );
                response.protocol = Some(PROTOCOL_VERSION);
                write_frame(&mut write, &response).await?;
                return Ok(());
            }
        };
    let subscribe = matches!(request, Request::Subscribe);
    // Subscribe first, then get an actor snapshot, avoiding missed state transitions.
    let mut receiver = events.subscribe();
    let (reply_tx, reply_rx) = oneshot::channel();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(9);
    let answer = tokio::time::timeout_at(deadline, async {
        commands
            .send(Command {
                request,
                reply: reply_tx,
            })
            .await?;
        reply_rx.await.context("daemon is shutting down")
    })
    .await;
    // A live but busy daemon says so; closing silently reads as "daemon not running".
    let response = answer.unwrap_or_else(|_| {
        let mut r = Response::error(
            State::Idle,
            "xflowd is running but busy and did not answer in time; try again in a moment",
        );
        r.protocol = Some(PROTOCOL_VERSION);
        Ok(r)
    })?;
    write_frame(&mut write, &response).await?;
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
                // The actor closes its command receiver after cleanup. Leave only
                // after the initial response above has been written.
                _ = commands.closed() => break,
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
                samples: vec![0.2; 8000],
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
                samples: vec![0.2; 8000],
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
    struct FakeHotkeys(mpsc::Receiver<HotkeyAction>);
    #[async_trait]
    impl GlobalHotkeys for FakeHotkeys {
        async fn next_action(&mut self) -> Result<HotkeyAction> {
            self.0.recv().await.context("fixture hotkeys closed")
        }
    }
    async fn hotkey_state(events: &mut broadcast::Receiver<Response>, state: State) -> Response {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = events.recv().await.unwrap();
                if event.state == state {
                    return event;
                }
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn hotkeys_drive_capture_and_reload_keeps_old_adapter_on_failure() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let audio = Arc::new(FakeAudio(AtomicBool::new(false)));
        let desktop = Arc::new(FakeDesktop(AtomicUsize::new(0)));
        let options = Arc::new(Mutex::new(Vec::new()));
        let called = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);
        let (events, mut observed) = broadcast::channel(32);
        let (desktop_events, mut pill) = broadcast::channel(32);
        let mut config = Config::default();
        config.privacy.history = false;
        config.privacy.history_limit = 5;
        let mut engine = Engine::new(
            config,
            Services {
                audio: audio.clone(),
                desktop: desktop.clone(),
                transformer: None,
                stt: Arc::new(FakeStt {
                    called: called.clone(),
                    release: Arc::new(Notify::new()),
                    options: options.clone(),
                }),
            },
            Store::open(std::path::Path::new("unused"), false, 5).unwrap(),
            events,
            desktop_events,
        );
        engine.config_path = Some(path.clone());
        engine.reload_factory = Arc::new(|_, _, services| Ok(services));
        let (keys, key_events) = mpsc::channel(8);
        engine.hotkeys = Some(Box::new(FakeHotkeys(key_events)));
        let (new_keys, new_events) = mpsc::channel(8);
        let new_events = Mutex::new(Some(new_events));
        engine.hotkey_factory = Some(Arc::new(move |config| {
            if config.key == "caps_lock" {
                bail!("fixture permission failure");
            }
            assert_eq!(config.key, "right_option");
            Ok(Some(Box::new(FakeHotkeys(
                new_events.lock().unwrap().take().unwrap(),
            ))))
        }));
        let actor = tokio::spawn(engine.run(rx));
        keys.send(HotkeyAction::Start).await.unwrap();
        hotkey_state(&mut observed, State::Listening).await;
        assert!(audio.0.load(Ordering::SeqCst));
        // Capture runs at once, but the pill sees nothing until the gesture is revealed.
        assert!(pill.try_recv().is_err());
        keys.send(HotkeyAction::Reveal).await.unwrap();
        let shown = tokio::time::timeout(Duration::from_secs(2), pill.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(shown.state, State::Listening);
        keys.send(HotkeyAction::HandsFree).await.unwrap();
        while hotkey_state(&mut observed, State::Listening)
            .await
            .message
            .as_deref()
            != Some("Hands-free recording")
        {}
        keys.send(HotkeyAction::Cancel).await.unwrap();
        hotkey_state(&mut observed, State::Idle).await;
        assert!(!audio.0.load(Ordering::SeqCst));
        assert!(options.lock().unwrap().is_empty());

        std::fs::write(
            &path,
            "[hotkey]\nkey = 'caps_lock'\n[privacy]\nhistory = false\nhistory_limit = 5\n",
        )
        .unwrap();
        // A failed tap applies the rest of the config, warns, and keeps the old binding.
        let reloaded = ask(&tx, Request::Reload).await;
        assert!(reloaded.ok);
        assert!(reloaded
            .message
            .unwrap()
            .contains("fixture permission failure"));
        keys.send(HotkeyAction::Start).await.unwrap();
        hotkey_state(&mut observed, State::Listening).await;
        keys.send(HotkeyAction::Stop).await.unwrap();
        hotkey_state(&mut observed, State::Processing).await;
        called.notified().await;
        keys.send(HotkeyAction::Cancel).await.unwrap();
        hotkey_state(&mut observed, State::Idle).await;
        assert_eq!(desktop.0.load(Ordering::SeqCst), 0);

        std::fs::write(
            &path,
            "[hotkey]\nkey = 'right_option'\n[privacy]\nhistory = false\nhistory_limit = 5\n",
        )
        .unwrap();
        assert!(ask(&tx, Request::Reload).await.ok);
        // The replaced adapter is dropped off the actor.
        tokio::time::timeout(Duration::from_secs(2), keys.closed())
            .await
            .unwrap();
        new_keys.send(HotkeyAction::Start).await.unwrap();
        hotkey_state(&mut observed, State::Listening).await;
        assert!(ask(&tx, Request::Cancel).await.ok);
        assert!(ask(&tx, Request::Shutdown).await.ok);
        actor.await.unwrap();
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
        assert!(!ask(&tx, Request::Reload).await.ok);
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
            Some("Postgres is ready. ")
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
            Some("Postgres is ready. ")
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

    #[derive(Default)]
    struct RecordingDesktop {
        contexts: AtomicUsize,
        copies: Mutex<Vec<String>>,
        injections: Mutex<Vec<String>>,
        block_context: Option<Arc<Notify>>,
        empty_selection: bool,
        /// The focused text field; pastes append to it.
        field: Mutex<Option<String>>,
    }
    #[async_trait]
    impl Desktop for RecordingDesktop {
        async fn context(&self) -> Result<AppContext> {
            self.contexts.fetch_add(1, Ordering::SeqCst);
            if let Some(block) = &self.block_context {
                block.notified().await;
            }
            Ok(AppContext {
                app_id: Some("Org.Editor".into()),
                ..AppContext::default()
            })
        }
        async fn selection(&self) -> Result<Option<String>> {
            Ok((!self.empty_selection).then(|| "selected draft".into()))
        }
        async fn copy(&self, text: &str) -> Result<()> {
            self.copies.lock().unwrap().push(text.into());
            Ok(())
        }
        async fn inject(&self, text: &str, _: &AppContext) -> Result<InjectionOutcome> {
            self.injections.lock().unwrap().push(text.into());
            if let Some(field) = self.field.lock().unwrap().as_mut() {
                field.push_str(text);
            }
            Ok(InjectionOutcome::Pasted)
        }
        async fn focused_text(&self) -> Result<(AppContext, Option<String>)> {
            Ok((self.context().await?, self.field.lock().unwrap().clone()))
        }
    }
    struct ImmediateStt {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl SpeechToText for ImmediateStt {
        fn name(&self) -> &str {
            "fixture"
        }
        fn model(&self) -> &str {
            "mock-model"
        }
        async fn transcribe(&self, _: AudioClip, _: TranscriptionOptions) -> Result<Transcript> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Transcript {
                text: "um post gres is ready".into(),
                language: Some("en".into()),
            })
        }
    }
    type TransformCall = (
        String,
        String,
        CleanupMode,
        Option<String>,
        Option<String>,
        Vec<String>,
    );
    #[derive(Default)]
    struct RecordingTransformer(Mutex<Vec<TransformCall>>);
    #[async_trait]
    impl TextTransformer for RecordingTransformer {
        async fn transform(&self, r: TransformRequest<'_>) -> Result<String> {
            self.0.lock().unwrap().push((
                r.text.into(),
                r.command.unwrap_or("").into(),
                r.mode,
                r.instructions.map(str::to_owned),
                r.app_id.map(str::to_owned),
                r.vocabulary.to_vec(),
            ));
            Ok("my signature".into())
        }
    }
    fn fixture(
        config: Config,
        desktop: Arc<dyn Desktop>,
        store: Store,
    ) -> (
        Engine,
        mpsc::Sender<Command>,
        mpsc::Receiver<Command>,
        broadcast::Receiver<Response>,
        Arc<AtomicUsize>,
    ) {
        let (tx, rx) = mpsc::channel(8);
        let (events, receiver) = broadcast::channel(128);
        let (desktop_events, _) = broadcast::channel(32);
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = Engine::new(
            config,
            Services {
                audio: Arc::new(FakeAudio(AtomicBool::new(false))),
                desktop,
                stt: Arc::new(ImmediateStt {
                    calls: calls.clone(),
                }),
                transformer: None,
            },
            store,
            events,
            desktop_events,
        );
        (engine, tx, rx, receiver, calls)
    }
    async fn success(events: &mut broadcast::Receiver<Response>) -> Response {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let r = events.recv().await.unwrap();
                if r.state == State::Success {
                    return r;
                }
                assert_ne!(r.state, State::Error, "{:?}", r.message);
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn corrected_paste_and_history_edit_learn_dictionary_words() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let desktop = Arc::new(RecordingDesktop {
            field: Mutex::new(Some("Notes: ".into())),
            ..RecordingDesktop::default()
        });
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        let (mut engine, tx, rx, mut events, _) =
            fixture(Config::default(), desktop.clone(), store);
        engine.config_path = Some(path.clone());
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        let id = success(&mut events).await.entry.unwrap().id;
        // After the baseline read, the user fixes a word and edits elsewhere.
        tokio::time::sleep(Duration::from_millis(400)).await;
        *desktop.field.lock().unwrap() = Some("My Notes: Postgres is ready ".into());
        ask(&tx, Request::start()).await;
        let learned = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(message) = events.recv().await.unwrap().message {
                    if message.starts_with("Added") {
                        return message;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(learned, "Added 'Postgres' to your dictionary");
        ask(&tx, Request::Cancel).await;
        // History edits go through the same learning path.
        let edited = ask(
            &tx,
            Request::HistoryUpdate {
                id,
                text: "post Gress is ready".into(),
            },
        )
        .await;
        assert_eq!(
            edited.message.as_deref(),
            Some("Added 'Gress' to your dictionary")
        );
        let entry = edited.entry.unwrap();
        assert_eq!(entry.text, "post Gress is ready");
        assert_eq!(entry.raw_text.as_deref(), Some("um post gres is ready"));
        let dictionary = Config::load(&path).unwrap().dictionary;
        assert_eq!(dictionary.words, ["Gress", "Postgres"]);
        assert_eq!(dictionary.learned, ["Postgres", "Gress"]);
        // Learning adds vocabulary hints only, never replacements.
        assert!(dictionary.replacements.is_empty());
        assert!(
            !ask(
                &tx,
                Request::HistoryUpdate {
                    id: 999,
                    text: "x".into()
                }
            )
            .await
            .ok
        );
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn ipc_history_crud_stats_and_final_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        let desktop = Arc::new(RecordingDesktop::default());
        let (engine, tx, rx, mut events, _) = fixture(Config::default(), desktop.clone(), store);
        let task = tokio::spawn(engine.run(rx));
        let snapshot = ask(&tx, Request::Subscribe).await;
        assert!(snapshot.timings.is_none());
        assert_eq!(
            (
                snapshot.protocol,
                snapshot.provider.as_deref(),
                snapshot.model.as_deref()
            ),
            (Some(2), Some("fixture"), Some("mock-model"))
        );
        let request = Request::Start {
            mode: Mode::Dictation,
            context: None,
            t0_us: Some(monotonic_us()),
            delivery: Delivery::Inject,
        };
        assert_eq!(ask(&tx, request).await.state, State::Listening);
        assert!(!ask(&tx, Request::Reload).await.ok);
        ask(&tx, Request::Stop).await;
        let final_event = success(&mut events).await;
        let entry = final_event.entry.unwrap();
        let timings = final_event.timings.unwrap();
        assert_eq!(final_event.text.as_deref(), Some("post gres is ready "));
        assert_eq!(final_event.injection, Some(InjectionOutcome::Pasted));
        assert_eq!(entry.raw_text.as_deref(), Some("um post gres is ready"));
        assert_eq!(entry.language.as_deref(), Some("en"));
        assert_eq!(entry.duration_ms, Some(500));
        assert!(
            timings.hotkey_ms.is_some() && timings.open_ms.is_some() && timings.inject_ms.is_some()
        );
        assert_eq!(timings.audio_ms, 500);
        assert_eq!(entry.latency_ms, Some(timings.total_ms as u64));
        assert_eq!(
            ask(&tx, Request::Status).await.timings,
            Some(timings.clone())
        );
        assert!(ask(&tx, Request::start()).await.timings.is_none());
        ask(&tx, Request::Cancel).await;
        assert_eq!(
            ask(&tx, Request::Status).await.timings,
            Some(timings.clone())
        );
        let page = ask(
            &tx,
            Request::History {
                limit: 10,
                offset: 0,
                query: Some("READY".into()),
            },
        )
        .await;
        assert_eq!(page.total, Some(1));
        assert_eq!(page.history[0], entry);
        assert!(ask(
            &tx,
            Request::History {
                limit: 10,
                offset: 1,
                query: None
            }
        )
        .await
        .history
        .is_empty());
        assert_eq!(
            ask(&tx, Request::HistoryGet { id: entry.id }).await.entry,
            Some(entry.clone())
        );
        assert!(ask(&tx, Request::HistoryCopy { id: entry.id }).await.ok);
        assert_eq!(
            ask(&tx, Request::HistoryPaste { id: entry.id })
                .await
                .injection,
            Some(InjectionOutcome::Pasted)
        );
        assert_eq!(desktop.copies.lock().unwrap().len(), 1);
        assert_eq!(desktop.injections.lock().unwrap().len(), 2);
        assert!(ask(&tx, Request::CopyLast).await.ok);
        assert_eq!(
            ask(&tx, Request::PasteLast).await.injection,
            Some(InjectionOutcome::Pasted)
        );
        assert_eq!(ask(&tx, Request::Stats).await.stats.unwrap().sessions, 1);
        assert!(ask(&tx, Request::HistoryDelete { id: entry.id }).await.ok);
        for r in [
            Request::HistoryGet { id: entry.id },
            Request::HistoryDelete { id: entry.id },
            Request::HistoryCopy { id: entry.id },
            Request::HistoryPaste { id: entry.id },
        ] {
            assert!(!ask(&tx, r).await.ok);
        }
        assert_eq!(ask(&tx, Request::Stats).await.stats.unwrap().sessions, 0);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn delivery_none_and_clipboard_do_not_inject() {
        for delivery in [Delivery::None, Delivery::Clipboard] {
            let desktop = Arc::new(RecordingDesktop::default());
            let (engine, tx, rx, mut events, _) = fixture(
                Config::default(),
                desktop.clone(),
                Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
            );
            let task = tokio::spawn(engine.run(rx));
            ask(
                &tx,
                Request::Start {
                    mode: Mode::Dictation,
                    context: None,
                    t0_us: None,
                    delivery,
                },
            )
            .await;
            ask(&tx, Request::Stop).await;
            let final_event = success(&mut events).await;
            assert!(desktop.injections.lock().unwrap().is_empty());
            assert!(final_event.entry.is_none());
            if delivery == Delivery::None {
                assert_eq!(desktop.contexts.load(Ordering::SeqCst), 0);
                assert!(desktop.copies.lock().unwrap().is_empty());
                assert_eq!(final_event.injection, None);
            } else {
                assert_eq!(desktop.copies.lock().unwrap().len(), 1);
                assert_eq!(final_event.injection, Some(InjectionOutcome::ClipboardOnly));
            }
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn reload_success_invalid_config_and_build_failure_keep_running_services() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        store
            .append(HistoryEntry {
                text: "previous session".into(),
                provider: "fixture".into(),
                ..HistoryEntry::default()
            })
            .await
            .unwrap();
        let (mut engine, tx, rx, _, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            store,
        );
        engine.config_path = Some(path.clone());
        engine.reload_factory = Arc::new(|_, new, services| {
            if new.stt.provider == "broken" {
                bail!("fixture build failed");
            }
            Ok(services)
        });
        let task = tokio::spawn(engine.run(rx));
        std::fs::write(&path, "[privacy]\nhistory = false\n").unwrap();
        assert!(ask(&tx, Request::Reload).await.ok);
        std::fs::write(&path, "[recording]\nmin_ms = 999999\n").unwrap();
        assert!(!ask(&tx, Request::Reload).await.ok);
        std::fs::write(
            &path,
            "[stt]\nprovider = 'broken'\n[privacy]\nhistory = true\n",
        )
        .unwrap();
        assert!(!ask(&tx, Request::Reload).await.ok);
        ask(&tx, Request::start()).await;
        assert!(!ask(&tx, Request::Reload).await.ok);
        ask(&tx, Request::Cancel).await;
        assert_eq!(
            ask(&tx, Request::Status).await.provider.as_deref(),
            Some("fixture")
        );
        assert_eq!(ask(&tx, Request::Stats).await.stats.unwrap().sessions, 0);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn slow_focus_does_not_block_stop_or_cancel() {
        let desktop = Arc::new(RecordingDesktop {
            block_context: Some(Arc::new(Notify::new())),
            ..RecordingDesktop::default()
        });
        let (engine, tx, rx, _, _) = fixture(
            Config::default(),
            desktop.clone(),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let task = tokio::spawn(engine.run(rx));
        assert_eq!(ask(&tx, Request::start()).await.state, State::Listening);
        assert_eq!(ask(&tx, Request::Stop).await.state, State::Processing);
        assert_eq!(ask(&tx, Request::Cancel).await.state, State::Idle);
        assert!(desktop.injections.lock().unwrap().is_empty());
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn command_uses_selection_local_instruction_style_and_literal_snippet() {
        let mut config = Config::default();
        config.cleanup.app_context = false;
        config.dictionary.words = vec!["Postgres".into()];
        config.dictionary.replacements = vec![xflow_core::config::Replacement {
            from: "post gres".into(),
            to: "Postgres".into(),
        }];
        config.styles = vec![xflow_core::config::Style {
            name: "editor".into(),
            apps: vec!["EDITOR".into()],
            mode: Some(CleanupMode::Polished),
            prompt: Some("style prompt".into()),
        }];
        config.snippets = vec![xflow_core::config::Snippet {
            trigger: "my signature".into(),
            text: "Literal\n  Postgres".into(),
        }];
        let (mut engine, tx, rx, mut events, _) = fixture(
            config,
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let transformer = Arc::new(RecordingTransformer::default());
        engine.services.transformer = Some(transformer.clone());
        let task = tokio::spawn(engine.run(rx));
        assert!(
            ask(
                &tx,
                Request::Start {
                    mode: Mode::Command,
                    context: None,
                    t0_us: None,
                    delivery: Delivery::Inject
                }
            )
            .await
            .ok
        );
        ask(&tx, Request::Stop).await;
        let r = success(&mut events).await;
        assert_eq!(r.text.as_deref(), Some("Literal\n  Postgres "));
        let calls = transformer.0.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            (
                "selected draft".into(),
                "Postgres is ready".into(),
                CleanupMode::Polished,
                Some("style prompt".into()),
                None,
                vec!["Postgres".into()]
            )
        );
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn short_recording_never_calls_provider_and_command_without_cleanup_fails() {
        let mut config = Config::default();
        config.recording.min_ms = 1000;
        let (engine, tx, rx, _, calls) = fixture(
            config,
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let task = tokio::spawn(engine.run(rx));
        assert!(
            !ask(
                &tx,
                Request::Start {
                    mode: Mode::Command,
                    context: None,
                    t0_us: None,
                    delivery: Delivery::Inject
                }
            )
            .await
            .ok
        );
        ask(&tx, Request::start()).await;
        let stop = ask(&tx, Request::Stop).await;
        assert_eq!(stop.state, State::Idle);
        assert!(stop.message.unwrap().contains("too short"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }

    #[tokio::test]
    async fn silence_auto_stop_and_level_events_are_listening_only() {
        let mut config = Config::default();
        config.recording.auto_stop_secs = 1;
        config.recording.silence_threshold = 0.5;
        let (engine, tx, rx, mut events, calls) = fixture(
            config,
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        let r = success(&mut events).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(r.level, 0.0);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), events.recv())
                .await
                .is_err()
        );
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn canceled_focus_cannot_override_a_new_session() {
        let block = Arc::new(Notify::new());
        let desktop = Arc::new(RecordingDesktop {
            block_context: Some(block.clone()),
            ..RecordingDesktop::default()
        });
        let (engine, tx, rx, mut events, _) = fixture(
            Config::default(),
            desktop.clone(),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        ask(&tx, Request::Cancel).await;
        ask(
            &tx,
            Request::Start {
                mode: Mode::Dictation,
                context: Some(AppContext::default()),
                t0_us: Some(u64::MAX),
                delivery: Delivery::Inject,
            },
        )
        .await;
        ask(&tx, Request::Stop).await;
        let r = success(&mut events).await;
        assert_eq!(r.timings.unwrap().hotkey_ms, None);
        block.notify_waiters();
        assert_eq!(ask(&tx, Request::Last).await.text, r.text);
        assert_eq!(desktop.injections.lock().unwrap().len(), 1);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn command_empty_selection_generates_and_failed_transform_never_injects() {
        for fail in [false, true] {
            let desktop = Arc::new(RecordingDesktop {
                empty_selection: true,
                ..RecordingDesktop::default()
            });
            let (mut engine, tx, rx, mut events, _) = fixture(
                Config::default(),
                desktop.clone(),
                Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
            );
            let transformer = Arc::new(RecordingTransformer::default());
            engine.services.transformer = Some(if fail {
                Arc::new(FailingCleanup)
            } else {
                transformer.clone()
            });
            let task = tokio::spawn(engine.run(rx));
            ask(
                &tx,
                Request::Start {
                    mode: Mode::Command,
                    context: None,
                    t0_us: None,
                    delivery: Delivery::Inject,
                },
            )
            .await;
            ask(&tx, Request::Stop).await;
            if fail {
                let error = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let r = events.recv().await.unwrap();
                        if r.state == State::Error {
                            return r;
                        }
                    }
                })
                .await
                .unwrap();
                assert!(error.message.unwrap().contains("selection left untouched"));
                assert!(desktop.injections.lock().unwrap().is_empty());
                assert!(ask(&tx, Request::Last).await.text.is_none());
            } else {
                success(&mut events).await;
                assert_eq!(transformer.0.lock().unwrap()[0].0, "");
            }
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    struct LevelAudio(std::sync::atomic::AtomicU32);
    #[async_trait]
    impl AudioCapture for LevelAudio {
        async fn start(&self) -> Result<()> {
            Ok(())
        }
        async fn stop(&self) -> Result<AudioClip> {
            Ok(AudioClip {
                samples: vec![0.2; 8000],
                sample_rate: 16000,
                channels: 1,
            })
        }
        async fn cancel(&self) -> Result<()> {
            Ok(())
        }
        fn level(&self) -> f32 {
            f32::from_bits(self.0.load(Ordering::SeqCst))
        }
    }
    #[tokio::test]
    async fn speech_resets_continuous_silence_deadline() {
        let mut config = Config::default();
        config.recording.auto_stop_secs = 1;
        let (mut engine, tx, rx, mut events, _) = fixture(
            config,
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let audio = Arc::new(LevelAudio(std::sync::atomic::AtomicU32::new(
            0.0_f32.to_bits(),
        )));
        engine.services.audio = audio.clone();
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        audio.0.store(0.2_f32.to_bits(), Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        audio.0.store(0.0_f32.to_bits(), Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(ask(&tx, Request::Status).await.state, State::Listening);
        success(&mut events).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[test]
    fn recognition_hints_include_stt_and_dictionary_words() {
        let mut config = Config::default();
        config.stt.vocabulary = vec!["Postgres".into(), "XFlow".into()];
        config.dictionary.words = vec!["postgres".into(), "Éva".into()];
        assert_eq!(vocabulary(&config), ["Postgres", "XFlow", "Éva"]);
    }

    #[tokio::test]
    async fn abandoned_queued_start_does_not_open_capture() {
        let (engine, tx, rx, _, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let (reply, dropped) = oneshot::channel();
        drop(dropped);
        tx.send(Command {
            request: Request::start(),
            reply,
        })
        .await
        .unwrap();
        let task = tokio::spawn(engine.run(rx));
        assert_eq!(ask(&tx, Request::Status).await.state, State::Idle);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// Open blocks until released, like CoreAudio behind the microphone permission prompt.
    #[derive(Default)]
    struct GatedAudio {
        open: Notify,
        cancels: AtomicUsize,
    }
    #[async_trait]
    impl AudioCapture for GatedAudio {
        async fn start(&self) -> Result<()> {
            self.open.notified().await;
            Ok(())
        }
        async fn stop(&self) -> Result<AudioClip> {
            bail!("fixture never records")
        }
        async fn cancel(&self) -> Result<()> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn level(&self) -> f32 {
            0.0
        }
    }
    #[tokio::test]
    async fn pending_mic_open_keeps_ipc_responsive_and_never_records_for_a_gone_requester() {
        let (mut engine, tx, rx, mut events, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let audio = Arc::new(GatedAudio::default());
        engine.services.audio = audio.clone();
        let (keys, key_events) = mpsc::channel(4);
        engine.hotkeys = Some(Box::new(FakeHotkeys(key_events)));
        let task = tokio::spawn(engine.run(rx));
        // The discarded open publishes Idle with the waiting message cleared.
        let discarded = |mut events: broadcast::Receiver<Response>| {
            tokio::spawn(async move {
                let event = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let event = events.recv().await.unwrap();
                        if event.message.is_none() {
                            return event;
                        }
                    }
                });
                assert_eq!(event.await.unwrap().state, State::Idle);
            })
        };
        // An IPC client starts, the open hangs, and the client gives up.
        let (reply, response) = oneshot::channel();
        tx.send(Command {
            request: Request::start(),
            reply,
        })
        .await
        .unwrap();
        assert_eq!(ask(&tx, Request::Status).await.state, State::Idle);
        let waiting = hotkey_state(&mut events, State::Idle).await;
        assert_eq!(waiting.message.as_deref(), Some(MIC_WAITING));
        assert_eq!(
            ask(&tx, Request::Status).await.message.as_deref(),
            Some(MIC_WAITING)
        );
        drop(response);
        let idle = discarded(events.resubscribe());
        audio.open.notify_one();
        idle.await.unwrap();
        assert_eq!(audio.cancels.load(Ordering::SeqCst), 1);
        // A hotkey start whose key is released before the open completes.
        keys.send(HotkeyAction::Start).await.unwrap();
        keys.send(HotkeyAction::Stop).await.unwrap();
        // The actor has taken the release once the channel drains.
        while keys.capacity() < keys.max_capacity() {
            tokio::task::yield_now().await;
        }
        let idle = discarded(events.resubscribe());
        audio.open.notify_one();
        idle.await.unwrap();
        assert_eq!(audio.cancels.load(Ordering::SeqCst), 2);
        let status = ask(&tx, Request::Status).await;
        assert_eq!((status.state, status.message), (State::Idle, None));
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    struct FinishingAudio {
        finished: AtomicBool,
        fail: bool,
    }
    #[async_trait]
    impl AudioCapture for FinishingAudio {
        async fn start(&self) -> Result<()> {
            self.finished.store(false, Ordering::SeqCst);
            Ok(())
        }
        async fn stop(&self) -> Result<AudioClip> {
            self.finished.store(false, Ordering::SeqCst);
            if self.fail {
                return Err(
                    anyhow::anyhow!("fixture device disconnected").context("capture failed")
                );
            }
            Ok(AudioClip {
                samples: vec![0.2; 8000],
                sample_rate: 16000,
                channels: 1,
            })
        }
        async fn cancel(&self) -> Result<()> {
            self.finished.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn level(&self) -> f32 {
            0.2
        }
        fn finished(&self) -> bool {
            self.finished.load(Ordering::SeqCst)
        }
    }
    #[tokio::test]
    async fn completed_capture_stops_early_and_surfaces_original_device_error() {
        for fail in [false, true] {
            let (mut engine, tx, rx, mut events, calls) = fixture(
                Config::default(),
                Arc::new(RecordingDesktop::default()),
                Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
            );
            let audio = Arc::new(FinishingAudio {
                finished: AtomicBool::new(false),
                fail,
            });
            engine.services.audio = audio.clone();
            let task = tokio::spawn(engine.run(rx));
            ask(&tx, Request::start()).await;
            audio.finished.store(true, Ordering::SeqCst);
            if fail {
                let r = timeout_event_error(&mut events).await;
                assert!(r.message.unwrap().contains("fixture device disconnected"));
                assert_eq!(calls.load(Ordering::SeqCst), 0);
            } else {
                success(&mut events).await;
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            }
            assert!(!audio.finished());
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    async fn timeout_event_error(events: &mut broadcast::Receiver<Response>) -> Response {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let r = events.recv().await.unwrap();
                if r.state == State::Error {
                    return r;
                }
            }
        })
        .await
        .unwrap()
    }
    struct HoldingDesktop {
        pressed: AtomicBool,
        started: Notify,
        release: Notify,
    }
    #[async_trait]
    impl Desktop for HoldingDesktop {
        async fn context(&self) -> Result<AppContext> {
            Ok(AppContext::default())
        }
        async fn copy(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn inject(&self, _: &str, _: &AppContext) -> Result<InjectionOutcome> {
            self.pressed.store(true, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            self.pressed.store(false, Ordering::SeqCst);
            Ok(InjectionOutcome::Pasted)
        }
    }
    #[tokio::test]
    async fn cancel_and_shutdown_allow_started_delivery_to_release_keys() {
        let desktop = Arc::new(HoldingDesktop {
            pressed: AtomicBool::new(false),
            started: Notify::new(),
            release: Notify::new(),
        });
        let (engine, tx, rx, _, _) = fixture(
            Config::default(),
            desktop.clone(),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        tokio::time::timeout(Duration::from_secs(2), desktop.started.notified())
            .await
            .unwrap();
        assert_eq!(ask(&tx, Request::Cancel).await.state, State::Idle);
        assert!(!ask(&tx, Request::start()).await.ok);
        ask(&tx, Request::Shutdown).await;
        assert!(desktop.pressed.load(Ordering::SeqCst));
        desktop.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(!desktop.pressed.load(Ordering::SeqCst));
    }
    #[test]
    fn clipboard_delivery_retains_warnings() {
        let message = delivery_message(
            Some("Cleanup failed; original transcript retained".into()),
            Some(InjectionOutcome::ClipboardOnly),
        )
        .unwrap();
        assert!(message.contains("Cleanup failed") && message.contains("paste manually"));
    }

    #[test]
    fn final_event_sheds_optional_metadata_to_fit_frame() {
        let mut r = Response {
            text: Some("x".repeat(32 * 1024)),
            entry: Some(HistoryEntry {
                text: "x".repeat(32 * 1024),
                raw_text: Some("y".repeat(32 * 1024)),
                ..HistoryEntry::default()
            }),
            timings: Some(Timings::default()),
            ..Response::status(State::Success, 0.0)
        };
        fit_final(&mut r);
        assert!(serde_json::to_vec(&r).unwrap().len() < MAX_MESSAGE_BYTES);
        assert!(r.text.is_some() && r.timings.is_some());
    }

    #[test]
    fn history_page_is_longest_prefix_that_fits_frame() {
        let rows: Vec<_> = (1..=40)
            .map(|id| HistoryEntry {
                id,
                text: "x".repeat(4000),
                ..HistoryEntry::default()
            })
            .collect();
        let mut r = Response::status(State::Idle, 0.0);
        r.total = Some(40);
        fit_history(&mut r, rows.clone()).unwrap();
        let n = r.history.len();
        assert!(n > 0 && n < rows.len());
        assert!(serde_json::to_vec(&r).unwrap().len() < MAX_MESSAGE_BYTES);
        r.history.push(rows[n].clone());
        assert!(serde_json::to_vec(&r).unwrap().len() >= MAX_MESSAGE_BYTES);
    }

    #[test]
    fn escaped_transcript_must_fit_last_frame() {
        assert!(validate_last_frame(&"a".repeat(32 * 1024)).is_ok());
        assert!(validate_last_frame(&"\0".repeat(12 * 1024)).is_err());
    }
}
