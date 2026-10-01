use crate::{
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
    config::Config,
    ipc::{
        Delivery, HistoryEntry, Request, Response, Timings, MAX_MESSAGE_BYTES, PROTOCOL_VERSION,
    },
    AppContext, AudioCapture, AudioClip, CleanupMode, Desktop, DesktopEvent, InjectionOutcome,
    Mode, SpeechToText, State, TextTransformer, TranscriptionOptions, TransformRequest,
};

struct Processed {
    text: String,
    target: AppContext,
    entry: Option<HistoryEntry>,
    timings: Timings,
    stopped: Instant,
    warning: Option<String>,
}
enum Completion {
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
        result: Box<Result<(Config, Services, Store)>>,
        reply: oneshot::Sender<Response>,
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
pub struct Engine {
    config: Config,
    services: Services,
    store: Store,
    config_path: Option<PathBuf>,
    reload_factory: ReloadFactory,
    reloading: bool,
    state: State,
    mode: Mode,
    delivery: Delivery,
    last: Option<String>,
    message: Option<String>,
    generation: u64,
    epoch: Arc<AtomicU64>,
    job: Option<JoinHandle<()>>,
    context_job: Option<JoinHandle<AppContext>>,
    context_abort: Option<tokio::task::AbortHandle>,
    warm_job: Option<JoinHandle<()>>,
    notifications: tokio::task::JoinSet<()>,
    started: Option<Instant>,
    silent_since: Option<Instant>,
    timings: Timings,
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
            reloading: false,
            state: State::Idle,
            mode: Mode::Dictation,
            delivery: Delivery::Inject,
            last: None,
            message: None,
            generation: 0,
            epoch: Arc::new(AtomicU64::new(0)),
            job: None,
            context_job: None,
            context_abort: None,
            warm_job: None,
            notifications: tokio::task::JoinSet::new(),
            started: None,
            silent_since: None,
            timings: Timings::default(),
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
        r.message = self.message.clone();
        r.version = Some(env!("CARGO_PKG_VERSION").into());
        r.protocol = Some(PROTOCOL_VERSION);
        r.provider = Some(self.services.stt.name().into());
        r.model = Some(self.services.stt.model().to_owned()).filter(|s| !s.is_empty());
        if self.active() {
            r.mode = Some(self.mode);
        }
        r
    }
    fn active(&self) -> bool {
        matches!(self.state, State::Listening | State::Processing)
    }
    fn publish(&self) {
        let r = self.status();
        let _ = self.desktop_events.send(DesktopEvent {
            state: r.state,
            level: r.level,
            message: r.message.clone(),
            mode: self.mode,
        });
        let _ = self.events.send(r);
    }
    fn set_state(&mut self, state: State, message: Option<String>) {
        self.state = state;
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
    fn notify(&mut self, summary: &'static str, body: &'static str) {
        if !cfg!(test) && self.config.notifications.enabled && self.notifications.len() < 4 {
            self.notifications.spawn(async move {
                let _ = xflow_platform::notify(summary, body).await;
            });
        }
    }
    fn advance(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.epoch.store(self.generation, Ordering::SeqCst);
    }
    async fn start(
        &mut self,
        mode: Mode,
        context: Option<AppContext>,
        t0_us: Option<u64>,
        delivery: Delivery,
    ) -> Result<()> {
        if self.active() {
            bail!("a recording is already active");
        }
        if self.reloading {
            bail!("configuration reload is in progress");
        }
        if mode == Mode::Command && self.services.transformer.is_none() {
            bail!("command mode requires a configured cleanup provider");
        }
        let opened = Instant::now();
        if let Err(error) = self.services.audio.start().await {
            let _ = self.services.audio.cancel().await;
            self.set_state(State::Error, Some(error.to_string()));
            return Err(error);
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
        self.set_state(State::Listening, None);
        self.cue(xflow_platform::Cue::Start);
        Ok(())
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
                self.set_state(State::Error, Some(e.to_string()));
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
        if let Err(e) = self.services.audio.cancel().await {
            self.set_state(State::Error, Some(e.to_string()));
            return Err(e);
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
            Request::Start {
                mode,
                context,
                t0_us,
                delivery,
            } => self.start(mode, context, t0_us, delivery).await?,
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
                    self.start(mode, context, t0_us, delivery).await?;
                }
            }
            Request::Cancel => self.cancel().await?,
            Request::Last => {
                let mut r = self.status();
                r.text = self.last.clone();
                return Ok(r);
            }
            Request::Status | Request::Subscribe | Request::Shutdown => (),
            _ => bail!("invalid actor request"),
        }
        Ok(self.status())
    }
    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let (completion_tx, mut completion_rx) = mpsc::channel(8);
        let mut levels = tokio::time::interval(Duration::from_millis(40));
        levels.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending: Option<Processed> = None;
        let mut queries = tokio::task::JoinSet::new();
        loop {
            let deadline = self.started.map(|s| {
                tokio::time::Instant::from_std(
                    s + Duration::from_secs(self.config.recording.max_seconds as u64),
                )
            });
            tokio::select! { biased;
                command = commands.recv() => {
                    let Some(Command { request, reply }) = command else { break; };
                    if (is_query(&request) || matches!(request, Request::Reload)) && queries.len() >= 32 { let _ = reply.send(Response::error(self.state, "daemon request capacity reached")); continue; }
                    if matches!(request, Request::Reload) {
                        if self.active() || self.reloading { let _ = reply.send(Response::error(self.state, "reload requires an idle daemon")); continue; }
                        let Some(path) = self.config_path.clone() else { let _ = reply.send(Response::error(self.state, "reload config path is unavailable")); continue; };
                        self.reloading = true;
                        let old = self.config.clone(); let services = self.services.clone(); let store = self.store.clone(); let factory = self.reload_factory.clone(); let tx = completion_tx.clone();
                        queries.spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                let config = Config::load(&path)?;
                                let services = factory(&old, &config, services)?;
                                let store = if store.settings() == (config.privacy.history, config.privacy.history_limit) { store } else { store.reopen(config.privacy.history, config.privacy.history_limit)? };
                                Ok((config, services, store))
                            }).await.unwrap_or_else(|e| Err(e.into()));
                            let _ = tx.send(Completion::Reloaded { result: Box::new(result), reply }).await;
                        }); continue;
                    }
                    if is_query(&request) {
                        if matches!(request, Request::PasteLast | Request::HistoryPaste { .. }) && self.active() { let _ = reply.send(Response::error(self.state, "wait for the current recording before pasting")); continue; }
                        if matches!(request, Request::ClearHistory) {
                            if self.active() { let _ = self.cancel().await; } else { self.advance(); }
                            self.last = None; pending = None;
                        }
                        let status = self.status(); let store = self.store.clone(); let desktop = self.services.desktop.clone(); let last = self.last.clone();
                        queries.spawn(async move { let state = status.state; let response = query(status, request, store, desktop, last).await.unwrap_or_else(|e| Response::error(state, e.to_string())); let _ = reply.send(response); });
                        continue;
                    }
                    let shutdown = matches!(request, Request::Shutdown);
                    let response = self.handle(request, &completion_tx).await.unwrap_or_else(|e| Response::error(self.state, e.to_string()));
                    let _ = reply.send(response); if shutdown { break; }
                }
                Some(completion) = completion_rx.recv() => match completion {
                    Completion::Ready { generation, result } if generation == self.generation => {
                        self.job = None;
                        match result { Ok(processed) => { self.last = Some(processed.text.clone()); pending = Some(processed); }, Err(e) => { self.abort_context(); self.set_state(State::Error, Some(e.to_string())); } }
                    }
                    Completion::Finished { generation, mut processed, result } if generation == self.generation => {
                        self.job = None; self.abort_context();
                        match result {
                            Ok(outcome) => {
                                if outcome == Some(InjectionOutcome::ClipboardOnly) { self.notify("XFlow transcript copied", "Paste manually to insert your dictation."); }
                                let message = if outcome == Some(InjectionOutcome::ClipboardOnly) { Some("Transcript copied; paste manually".into()) } else { processed.warning.take() };
                                // One final success event: subscribers can trust its complete payload.
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
                        let r = match *result { Ok((config, services, store)) => { self.config = config; self.services = services; self.store = store; self.publish(); self.status() }, Err(e) => Response::error(self.state, e.to_string()) };
                        let _ = reply.send(r);
                    }
                    _ => (),
                },
                _ = async {}, if pending.is_some() => {
                    let mut processed = pending.take().unwrap();
                    if self.state != State::Processing { continue; }
                    let generation = self.generation; let delivery = self.delivery; let desktop = self.services.desktop.clone(); let store = self.store.clone(); let tx = completion_tx.clone();
                    self.job = Some(tokio::spawn(async move {
                        let start = Instant::now();
                        let result = match delivery { Delivery::Inject => desktop.inject(&processed.text, &processed.target).await.map(Some), Delivery::Clipboard => desktop.copy(&processed.text).await.map(|_| Some(InjectionOutcome::ClipboardOnly)), Delivery::None => Ok(None) };
                        if delivery != Delivery::None { processed.timings.inject_ms = Some(ms(start.elapsed())); }
                        processed.timings.total_ms = ms(processed.stopped.elapsed());
                        if let Some(entry) = &mut processed.entry { entry.latency_ms = Some(processed.timings.total_ms as u64); if store.set_latency(entry.id, processed.timings.total_ms as u64).await.is_err() { processed.warning = Some("History latency update failed".into()); } }
                        let _ = tx.send(Completion::Finished { generation, processed, result }).await;
                    }));
                }
                _ = queries.join_next(), if !queries.is_empty() => (),
                _ = self.notifications.join_next(), if !self.notifications.is_empty() => (),
                _ = levels.tick(), if self.state == State::Listening => {
                    self.publish(); let level = self.services.audio.level();
                    if level.is_finite() && level > self.config.recording.silence_threshold { self.silent_since = None; }
                    else { self.silent_since.get_or_insert_with(Instant::now); }
                    if self.config.recording.auto_stop_secs > 0 && self.silent_since.is_some_and(|s| s.elapsed() >= Duration::from_secs(self.config.recording.auto_stop_secs as u64)) {
                        if let Err(e) = self.stop(completion_tx.clone()).await { self.set_state(State::Error, Some(e.to_string())); }
                    }
                }
                _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } }, if deadline.is_some() => {
                    if let Err(e) = self.stop(completion_tx.clone()).await { self.set_state(State::Error, Some(e.to_string())); }
                }
            }
        }
        queries.abort_all();
        self.notifications.abort_all();
        let _ = self.cancel().await;
    }
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
            r.history = rows;
            r.total = Some(total);
            while serde_json::to_vec(&r)?.len() + 1 > MAX_MESSAGE_BYTES {
                if r.history.pop().is_none() {
                    bail!("history response exceeds frame limit");
                }
            }
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
fn native_services(old: &Config, config: &Config, services: Services) -> Result<Services> {
    let stt = xflow_providers::build_stt(&config.stt, config.privacy.offline)?;
    let transformer = build_transformer(config)?;
    let audio = if old.recording != config.recording {
        Arc::new(xflow_platform::CpalCapture::new(&config.recording)?) as Arc<dyn AudioCapture>
    } else {
        services.audio
    };
    Ok(Services {
        audio,
        desktop: Arc::new(xflow_platform::LinuxDesktop::new(&config.injection)),
        stt,
        transformer,
    })
}
fn build_transformer(config: &Config) -> Result<Option<Arc<dyn TextTransformer>>> {
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
    if serde_json::to_vec(&r)?.len() + 128 >= MAX_MESSAGE_BYTES {
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
            Ok(InjectionOutcome::Pasted)
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
    async fn ipc_history_crud_stats_and_final_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        let desktop = Arc::new(RecordingDesktop::default());
        let (engine, tx, rx, mut events, _) = fixture(Config::default(), desktop.clone(), store);
        let task = tokio::spawn(engine.run(rx));
        let snapshot = ask(&tx, Request::Subscribe).await;
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
    fn escaped_transcript_must_fit_last_frame() {
        assert!(validate_last_frame(&"a".repeat(32 * 1024)).is_ok());
        assert!(validate_last_frame(&"\0".repeat(12 * 1024)).is_err());
    }
}
