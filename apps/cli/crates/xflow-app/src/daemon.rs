use crate::{
    config_edit::{ConfigFile, Undo},
    retry::{self, Part},
    speech::{self, NOT_CAUGHT},
    store::Store,
    transport::{read_frame, write_frame},
};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::{
    fs::File,
    hash::BuildHasher,
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
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
};
pub use xflow_core::ipc::Command;
use xflow_core::{
    config::{Config, DictionaryConfig, HotkeyConfig},
    ipc::{
        CleanupOutcome, Delivery, FailedAudio, Grant, HistoryEntry, Learning, Permissions, Request,
        Response, Timings, MAX_MESSAGE_BYTES, PROTOCOL_VERSION,
    },
    log, AppContext, AudioCapture, AudioClip, CleanupMode, Desktop, DesktopEvent, FieldEvent,
    GlobalHotkeys, HotkeyAction, InjectionOutcome, Mode, SpeechToText, State, TextTransformer,
    Transcript, TranscriptionOptions, TransformRequest,
};

#[cfg(debug_assertions)]
mod test_audio;

#[cfg(any(debug_assertions, test))]
fn test_audio_path() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    {
        std::env::var_os("XFLOW_TEST_AUDIO").map(PathBuf::from)
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

fn native_audio(config: &xflow_core::config::RecordingConfig) -> Result<Arc<dyn AudioCapture>> {
    #[cfg(debug_assertions)]
    if let Some(path) = test_audio_path() {
        log("warn", "TEST AUDIO: using WAV instead of microphone");
        return Ok(Arc::new(test_audio::WavCapture::new(path)));
    }
    Ok(Arc::new(xflow_platform::CpalCapture::new(config)?))
}

struct Processed {
    /// The text rewrites `target.selected_text` (resolved only once processing ends).
    rewrite: bool,
    /// Clipboard only: a failed rewrite must leave the selection untouched.
    copy: bool,
    provider: String,
    model: Option<String>,
    text: String,
    /// Field text around the caret for smart spacing; dropped once applied.
    around: Option<(String, String)>,
    target: AppContext,
    entry: Option<HistoryEntry>,
    timings: Timings,
    stopped: Instant,
    warning: Option<String>,
}
const RELOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// Reload requests queued behind a reload in flight; one more reload answers them all.
const MAX_RELOAD_WAITERS: usize = 8;
/// The daemon's learning and undo wait this long for the config lock, then fail
/// closed: a stopped CLI or editor never wedges them.
const LOCK_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(500)
} else {
    Duration::from_secs(5)
};
/// Dropping a tap adapter joins its run-loop thread; keep that off the actor.
fn retire(hotkeys: Option<Box<dyn GlobalHotkeys>>) {
    if let Some(hotkeys) = hotkeys {
        tokio::task::spawn_blocking(move || drop(hotkeys));
    }
}
/// Status message while a double-tap has latched recording; the macOS pill keys off it.
pub const HANDS_FREE: &str = "Hands-free recording";
/// Status message once a capture open is slow, typically behind the macOS microphone prompt.
pub const MIC_WAITING: &str = "Waiting for microphone permission";
const MIC_SLOW: Duration = Duration::from_secs(1);
/// A late permission grant must not record for an expired request or gesture.
const START_GRACE: Duration = Duration::from_secs(5);
/// How often captured audio moves from the microphone buffer into segments.
const DRAIN_EVERY: Duration = Duration::from_secs(1);
/// Longer selections are dictated over, not rewritten: one model call stays fast.
const MAX_REWRITE_CHARS: usize = 6000;
/// Longer speech over a selection is replacement text, pasted as dictated.
const REWRITE_WORDS: usize = 12;
const REWRITE_FAILED: &str = "Rewrite failed; selection left untouched";
struct Opening {
    mode: Mode,
    context: Option<AppContext>,
    t0_us: Option<u64>,
    delivery: Delivery,
    since: Instant,
    /// The IPC requester; None for a hotkey start.
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
        result: Result<Option<Processed>>,
    },
    Finished {
        generation: u64,
        processed: Processed,
        result: Result<Option<InjectionOutcome>>,
    },
    /// `seq`: the dictionary clock, read before the file was (see `Learned`).
    Reloaded {
        result: Box<Result<Reloaded>>,
        replies: Vec<oneshot::Sender<Response>>,
        seq: u64,
    },
    /// A tap recreated after Input Monitoring was granted (see `rearm`).
    Rearmed(std::result::Result<Option<Box<dyn GlobalHotkeys>>, String>),
    /// The daemon saved a dictionary to config.toml (a learn or an undo).
    /// Whole snapshots can arrive out of order, so each carries `seq`, taken
    /// from the dictionary clock while the config lock was held: the actor
    /// installs only a newer one. A reload reads the clock before the file, so
    /// it installs its dictionary only when no daemon write is newer than its
    /// read (an external write took the lock and saved before that read).
    Learned {
        dictionary: DictionaryConfig,
        seq: u64,
        /// For subscribers, e.g. "Added 'claud' to your dictionary".
        message: Option<String>,
        change: Option<Change>,
    },
}
/// The latest automatic dictionary change, kept in memory only while it can
/// be undone once (see `config_edit::Undo`).
struct Change {
    /// Opaque; the pill's Undo sends it back.
    id: u64,
    undo: Undo,
    /// The words saved, exactly as stored, for the notice.
    words: Vec<String>,
    /// A correction rule for known words rather than new words.
    correction: bool,
    /// From the paste observer: the pill may show it.
    notice: bool,
}
/// What the pill shows for one second after a correction is saved (macOS).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LearnNotice {
    pub op: u64,
    pub words: Vec<String>,
    pub correction: bool,
}
/// One watch's counts for `Learning`; never text.
#[derive(Default)]
struct Counts {
    reads: u32,
    unread_max: u32,
    late: u32,
    merged: u32,
    editing: u32,
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
    permissions: Option<Permissions>,
    permission_probe: Option<fn() -> Permissions>,
    #[cfg(target_os = "macos")]
    permission_request: Option<fn(Grant)>,
    /// The tap was created without Input Monitoring, so it never sees keys.
    hotkeys_blind: bool,
    rearming: bool,
    opening: Option<Opening>,
    /// A hotkey session not yet revealed: capture runs, but no cue or pill.
    quiet: bool,
    /// This session latched hands-free; it never rewrites a selection.
    hands_free: bool,
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
    /// The active session's generation, for connections that hold one (see `connection`).
    live: watch::Sender<Option<u64>>,
    job: Option<JoinHandle<()>>,
    /// The focused app, and terms from its window title (`privacy.window_terms`).
    context_job: Option<JoinHandle<(AppContext, Vec<String>)>>,
    /// Shipped hint terms picked off the actor at key-down from the focused app,
    /// window title and recent terms; uploads take the last pick without waiting.
    /// None until the first context read, or after the config changes.
    hints: Arc<std::sync::Mutex<Option<Vec<&'static str>>>>,
    /// Lexicon terms from recent deliveries, in memory only (`hints::Recent`).
    recent: Arc<std::sync::Mutex<xflow_core::hints::Recent>>,
    context_abort: Option<tokio::task::AbortHandle>,
    warm_job: Option<JoinHandle<()>>,
    /// Ends the observation of the last pasted field (see `observe`).
    observing: Option<oneshot::Sender<()>>,
    /// Orders dictionary snapshots (see `Completion::Learned`).
    dictionary_clock: Arc<AtomicU64>,
    /// The clock value of the dictionary installed now.
    dictionary_seq: u64,
    /// The latest automatic dictionary change, while it can still be undone.
    change: Option<Change>,
    /// Its notice for the pill, latest only; None once it can't be undone.
    notice: watch::Sender<Option<LearnNotice>>,
    /// Reload requests that arrived while one was in flight.
    reload_waiters: Vec<oneshot::Sender<Response>>,
    /// Counts for status and doctor.
    learning: Arc<std::sync::Mutex<Learning>>,
    notifications: tokio::task::JoinSet<()>,
    // Tokio's clock (the same in production) so the listening hints can be tested on paused time.
    started: Option<tokio::time::Instant>,
    silent_since: Option<tokio::time::Instant>,
    segments: Segments,
    timings: Timings,
    last_timings: Option<Timings>,
    served: Option<(String, Option<String>)>,
    /// Where a failed dictation is kept for `retry`; None never keeps one.
    kept_dir: Option<PathBuf>,
    kept: Option<FailedAudio>,
    /// The session in progress retries the kept dictation: cancel discards it.
    retrying: bool,
    #[cfg(test)]
    keep_hook: Option<KeepHook>,
    events: broadcast::Sender<Response>,
    desktop_events: broadcast::Sender<DesktopEvent>,
}
/// Where `transcribe` keeps a dictation that failed (see `retry`).
#[derive(Default)]
struct Keep {
    dir: Option<PathBuf>,
    created_at: i64,
    retry: bool,
    /// The retried dictation's parts that were already delivered...
    delivered: Vec<bool>,
    /// ...and are delivered again only when `full`.
    full: bool,
    #[cfg(test)]
    hook: Option<KeepHook>,
}
/// Test-only: runs in the save thread at "staged" and "committed".
#[cfg(test)]
type KeepHook = Arc<dyn Fn(&str) + Send + Sync>;
/// A segment's transcript, or its audio and energy flag for one retry after stop.
type Piece = std::result::Result<Transcript, (anyhow::Error, AudioClip, bool)>;
/// The current recording, transcribed in segments while it continues (see
/// `speech::split_point`): memory stays bounded and stop only waits for the tail.
#[derive(Default)]
struct Segments {
    /// Captured audio not yet sent.
    tail: Option<AudioClip>,
    /// Dropping the set aborts uploads, so cancel never delivers a stale segment.
    sent: tokio::task::JoinSet<(usize, Piece)>,
    count: usize,
    audio_ms: u64,
    heard: bool,
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
            permissions: None,
            permission_probe: None,
            #[cfg(target_os = "macos")]
            permission_request: None,
            hotkeys_blind: false,
            rearming: false,
            opening: None,
            quiet: false,
            hands_free: false,
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
            live: watch::Sender::new(None),
            job: None,
            context_job: None,
            hints: Default::default(),
            recent: Default::default(),
            context_abort: None,
            warm_job: None,
            observing: None,
            dictionary_clock: Arc::new(AtomicU64::new(0)),
            dictionary_seq: 0,
            change: None,
            notice: watch::Sender::new(None),
            reload_waiters: Vec::new(),
            learning: Default::default(),
            notifications: tokio::task::JoinSet::new(),
            started: None,
            silent_since: None,
            segments: Segments::default(),
            timings: Timings::default(),
            last_timings: None,
            served: None,
            kept_dir: None,
            kept: None,
            retrying: false,
            #[cfg(test)]
            keep_hook: None,
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
        let mut message = self.message.clone();
        if let Some(hint) = self.hint() {
            add_warning(&mut message, hint);
        }
        r.message = match (message, &self.persistence_warning) {
            (Some(message), Some(warning)) => {
                Some(bounded_message(&format!("{message}; {warning}")))
            }
            (None, Some(warning)) => Some(warning.clone()),
            (message, None) => message,
        };
        if let Some(warning) = &self.hotkey_warning {
            r.message = Some(bounded_message(&match r.message {
                Some(message) => format!("{message}; {warning}"),
                None => warning.clone(),
            }));
        }
        r.microphone = self
            .services
            .audio
            .microphone()
            .map(|name| bounded_message(&name));
        r.permissions = self.permissions.clone();
        if let Some(permissions) = &mut r.permissions {
            permissions.keyboard_events = self
                .hotkeys
                .as_ref()
                .and_then(|keys| keys.received_events());
            if let Some(warning) = permissions.warning() {
                r.message = Some(bounded_message(&match r.message {
                    Some(message) => format!("{message}; {warning}"),
                    None => warning.into(),
                }));
            }
        }
        r.version = Some(env!("CARGO_PKG_VERSION").into());
        r.protocol = Some(PROTOCOL_VERSION);
        if let Some(warning) = self.services.stt.warning() {
            add_warning(&mut r.message, warning);
        }
        r.provider = Some(self.services.stt.name().into());
        r.model = Some(self.services.stt.model().to_owned()).filter(|s| !s.is_empty());
        if self.state == State::Success {
            if let Some((provider, model)) = &self.served {
                r.provider = Some(provider.clone());
                r.model = model.clone();
            }
        }
        if self.active() {
            r.mode = Some(self.mode);
        } else {
            r.timings = self.last_timings.clone();
        }
        r.failed_audio = self.kept.clone();
        r
    }
    /// Where failed dictations go while `privacy.keep_failed_audio` is on.
    fn keeping(&self) -> Option<&std::path::Path> {
        self.kept_dir
            .as_deref()
            .filter(|_| self.config.privacy.keep_failed_audio)
    }
    /// Reads the kept dictation from disk; while idle, deletes what is expired,
    /// invalid, never committed or no longer allowed.
    fn load_kept(&mut self) {
        let Some(dir) = self.kept_dir.as_deref() else {
            return;
        };
        let kept = retry::load(dir)
            .ok()
            .flatten()
            .filter(|_| self.config.privacy.keep_failed_audio);
        if !self.active() {
            let cleaned = match kept {
                Some(_) => retry::prune(dir),
                None => retry::clear(dir),
            };
            if let Err(e) = cleaned {
                log(
                    "warn",
                    &format!("could not delete kept failed audio: {e:#}"),
                );
            }
        }
        self.kept = kept.map(|k| k.summary());
    }
    /// Deletes what `generation` kept, then re-reads the disk.
    fn discard_session(&mut self, generation: u64) {
        let Some(dir) = self.keeping().map(ToOwned::to_owned) else {
            return;
        };
        if let Err(e) = retry::discard(&dir, &retry::session(generation)) {
            log(
                "warn",
                &format!("could not delete kept failed audio: {e:#}"),
            );
        }
        self.load_kept();
    }
    fn discard_kept(&mut self) -> Result<()> {
        self.kept = None;
        self.kept_dir.as_deref().map_or(Ok(()), retry::clear)
    }
    /// Transcribes the kept dictation's failed parts again, then cleans up and
    /// delivers like `stop`, to the current focus: never the window it failed in.
    fn retry(
        &mut self,
        full: bool,
        delivery: Delivery,
        completion: &mpsc::Sender<Completion>,
    ) -> Result<()> {
        if self.active() || self.opening.is_some() || self.delivering || self.reloading {
            bail!("xflow is busy; retry when the current dictation is done");
        }
        let dir = self
            .kept_dir
            .clone()
            .context("no failed dictation is kept")?;
        let kept = retry::load(&dir)?
            .filter(|_| self.config.privacy.keep_failed_audio)
            .context("no failed dictation is kept")?;
        if let Some(finalize) = self.observing.take() {
            let _ = finalize.send(());
        }
        self.advance();
        self.retrying = true;
        self.mode = Mode::Dictation;
        self.delivery = delivery;
        self.timings = Timings {
            audio_ms: kept.audio_ms.min(u64::from(u32::MAX)) as u32,
            ..Timings::default()
        };
        let mut sent = tokio::task::JoinSet::new();
        let mut delivered = Vec::with_capacity(kept.parts.len());
        let domain = self.domain_hints();
        for (index, part) in kept.parts.into_iter().enumerate() {
            let (stt, config, dir) = (self.services.stt.clone(), self.config.clone(), dir.clone());
            let domain = domain.clone();
            match part {
                Part::Text {
                    text,
                    delivered: d,
                    provider,
                } => {
                    delivered.push(d);
                    let transcript = Transcript {
                        provider,
                        text,
                        language: None,
                    };
                    sent.spawn(async move { (index, Ok(transcript)) });
                }
                Part::Audio { low_energy } => {
                    delivered.push(false);
                    sent.spawn(async move {
                        let audio = tokio::task::spawn_blocking(move || retry::audio(&dir, index))
                            .await
                            .unwrap_or_else(|e| Err(e.into()));
                        let piece = match audio {
                            Ok(audio) => {
                                transcribe_segment(&*stt, audio, low_energy, &config, domain).await
                            }
                            // An empty clip fails again locally, before any upload.
                            Err(e) => Err((
                                e,
                                AudioClip {
                                    samples: vec![],
                                    sample_rate: 16_000,
                                    channels: 1,
                                },
                                low_energy,
                            )),
                        };
                        (index, piece)
                    });
                }
            }
        }
        let desktop = self.services.desktop.clone();
        let task = tokio::spawn(async move {
            match delivery {
                Delivery::None => Default::default(),
                _ => (desktop.context().await.unwrap_or_default(), Vec::new()),
            }
        });
        self.context_abort = Some(task.abort_handle());
        self.context_job = Some(task);
        self.set_state(State::Processing, None);
        let keep = Keep {
            dir: Some(dir),
            created_at: kept.created_at,
            retry: true,
            delivered,
            full,
            #[cfg(test)]
            hook: self.keep_hook.clone(),
        };
        self.transcribe(sent, 0, None, Instant::now(), keep, completion.clone());
        Ok(())
    }
    /// Shipped hint terms for the last known context. ponytail: a segment sent
    /// before this dictation's context read finishes uses the previous pick.
    fn domain_hints(&self) -> Vec<&'static str> {
        if !self.config.stt.domain_hints {
            return vec![];
        }
        let hints = lock(&self.hints);
        match &*hints {
            Some(hints) => hints.clone(),
            None => xflow_core::hints::select(&self.config, &Default::default()),
        }
    }
    /// Forgets the last pick and the recent terms when the config no longer
    /// allows them; called on every config change.
    fn reset_hints(&self) {
        *lock(&self.hints) = None;
        if !self.config.privacy.history || !self.config.stt.domain_hints {
            lock(&self.recent).clear();
        }
    }
    /// Install a dictionary saved by the daemon or read by a reload, with the
    /// caches built from it.
    fn install(&mut self, dictionary: DictionaryConfig, seq: u64) {
        self.config.dictionary = dictionary;
        self.dictionary_seq = seq;
        self.reset_hints();
        // Rebuild the snap index now, not on the next dictation.
        xflow_core::snap::prepare(&self.config);
    }
    /// Replace the undoable change and the pill's notice for it.
    fn set_change(&mut self, change: Option<Change>) {
        let notice = change.as_ref().filter(|c| c.notice).map(|c| LearnNotice {
            op: c.id,
            words: c.words.clone(),
            correction: c.correction,
        });
        self.change = change;
        self.notice.send_if_modified(move |current| {
            let changed = *current != notice;
            *current = notice;
            changed
        });
    }
    /// The overlay's view of the undoable change.
    pub fn notices(&self) -> watch::Receiver<Option<LearnNotice>> {
        self.notice.subscribe()
    }
    fn learning(&self) -> Learning {
        let mut learning = lock(&self.learning).clone();
        let dictionary = &self.config.dictionary;
        learning.words = dictionary.words.len() as u32;
        learning.learned = dictionary.learned.len() as u32;
        learning.replacements = dictionary.replacements.len() as u32;
        learning.fingerprint = format!("{:016x}", dictionary.fingerprint());
        learning.undo = self.change.is_some();
        learning
    }
    /// Start a reload that answers `replies`, or answer them with why not.
    /// Reloads never take the config lock: the clock read before the file
    /// orders them against the daemon's own writes.
    fn reload(
        &mut self,
        replies: Vec<oneshot::Sender<Response>>,
        completion: &mpsc::Sender<Completion>,
    ) -> Option<impl std::future::Future<Output = ()> + Send + 'static> {
        let refused = if self.opening.is_some() {
            Some(MIC_WAITING)
        } else if self.active() {
            Some("reload requires an idle daemon")
        } else if self.config_path.is_none() {
            Some("reload config path is unavailable")
        } else {
            None
        };
        if let Some(why) = refused {
            for reply in replies {
                let _ = reply.send(Response::error(self.state, why));
            }
            return None;
        }
        let path = self.config_path.clone()?;
        self.reloading = true;
        let old = self.config.clone();
        let services = self.services.clone();
        let store = self.store.clone();
        let factory = self.reload_factory.clone();
        let hotkey_factory = self.hotkey_factory.clone();
        let retry_hotkeys = self.hotkeys.is_none() || self.hotkey_warning.is_some();
        let tx = completion.clone();
        let clock = self.dictionary_clock.clone();
        Some(async move {
            // Before the file: every daemon write numbered up to here is in what loads.
            let seq = clock.load(Ordering::SeqCst);
            // Bounded so a wedged provider/tap setup cannot leave `reloading` stuck.
            let result = tokio::time::timeout(
                RELOAD_TIMEOUT,
                tokio::task::spawn_blocking(move || {
                    let config = Config::load(&path)?;
                    let services = factory(&old, &config, services)?;
                    // Recreate the tap only when its config changed or it is missing (e.g. permission granted since).
                    let hotkeys = hotkey_factory
                        .filter(|_| old.hotkey != config.hotkey || retry_hotkeys)
                        .map(|factory| {
                            factory(&config.hotkey).map_err(|e| bounded_message(&format!("{e:#}")))
                        });
                    let store = if store.settings()
                        == (config.privacy.history, config.privacy.history_limit)
                    {
                        store
                    } else {
                        store.reopen(config.privacy.history, config.privacy.history_limit)?
                    };
                    {
                        let store = store.clone();
                        tokio::spawn(async move {
                            if store.upgrade_search_index().await.is_err() {
                                log("warn", "history search index upgrade failed; history remains available");
                            }
                        });
                    }
                    Ok(Reloaded {
                        config,
                        services,
                        store,
                        hotkeys,
                    })
                }),
            )
            .await
            .unwrap_or_else(|_| Ok(Err(anyhow::anyhow!("configuration reload timed out"))))
            .unwrap_or_else(|e| Err(e.into()));
            let _ = tx
                .send(Completion::Reloaded {
                    result: Box::new(result),
                    replies,
                    seq,
                })
                .await;
        })
    }
    /// A reload finished (see `Completion::Reloaded`): the reply for its requests.
    fn reloaded(&mut self, result: Result<Reloaded>, seq: u64) -> Response {
        self.reloading = false;
        // config.toml was read again: no earlier change stays undoable.
        self.set_change(None);
        match result {
            Ok(Reloaded {
                config,
                services,
                store,
                hotkeys,
            }) => {
                // A daemon write newer than this read keeps its dictionary.
                let dictionary = if seq >= self.dictionary_seq {
                    config.dictionary.clone()
                } else {
                    self.config.dictionary.clone()
                };
                let seq = seq.max(self.dictionary_seq);
                self.config = config;
                self.install(dictionary, seq);
                self.services = services;
                self.store = store;
                match hotkeys {
                    Some(Ok(hotkeys)) => {
                        self.hotkeys_blind = !self.permissions.as_ref().is_some_and(|p| p.events());
                        retire(std::mem::replace(&mut self.hotkeys, hotkeys));
                        if self.hotkey_warning.take().is_some()
                            && self.state == State::Error
                            && self.message.is_none()
                        {
                            self.state = State::Idle;
                        }
                    }
                    Some(Err(warning)) => self.hotkey_warning = Some(warning),
                    None => (),
                }
                self.persistence_warning = None;
                self.load_kept();
                self.publish();
                self.status()
            }
            Err(e) => Response::error(self.state, format!("{e:#}")),
        }
    }
    /// The daemon saved a dictionary (see `Completion::Learned`): installed
    /// only when newer than the one in use. Event only: the state and status
    /// message stay with the current session.
    fn learned(
        &mut self,
        dictionary: DictionaryConfig,
        seq: u64,
        message: Option<String>,
        change: Option<Change>,
    ) {
        if seq > self.dictionary_seq {
            self.install(dictionary, seq);
            // config.toml changed again: an older change can no longer be undone exactly.
            self.set_change(change);
        }
        if let Some(message) = message {
            let mut r = self.status();
            r.message = Some(message.clone());
            let _ = self.events.send(r);
            self.notify("XFlow dictionary", &message);
        }
    }
    fn active(&self) -> bool {
        matches!(self.state, State::Listening | State::Processing)
    }
    fn track(&self) {
        let live = self.active().then_some(self.generation);
        self.live
            .send_if_modified(|s| std::mem::replace(s, live) != live);
    }
    /// Clients' probes (status, check-up, onboarding) are the only trigger: no polling.
    fn rearm(&mut self, events: bool, completion: &mpsc::Sender<Completion>) {
        if !events
            || !self.hotkeys_blind
            || self.rearming
            || self.reloading
            || self.active()
            || self.opening.is_some()
        {
            return;
        }
        let Some(factory) = self.hotkey_factory.clone() else {
            return;
        };
        self.rearming = true;
        let (config, tx) = (self.config.hotkey.clone(), completion.clone());
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || factory(&config))
                .await
                .unwrap_or_else(|e| Err(e.into()))
                .map_err(|e| bounded_message(&format!("{e:#}")));
            let _ = tx.send(Completion::Rearmed(result)).await;
        });
    }
    /// Listening advisories (#30), read off the session clocks on each level tick; no timer of their own.
    fn hint(&self) -> Option<&'static str> {
        let started = self.started.filter(|_| self.state == State::Listening)?;
        let max = u64::from(self.config.recording.max_seconds);
        if max > ENDS_SOON_SECS && started.elapsed() >= Duration::from_secs(max - ENDS_SOON_SECS) {
            Some(ENDS_SOON)
        // `silent_since` stays at `started` until the first level above the threshold.
        } else if self.silent_since == Some(started) && started.elapsed() >= NO_SIGNAL_AFTER {
            Some(NO_SIGNAL)
        } else {
            None
        }
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
        if state == State::Error {
            // Status timings describe the last success; after a failure they would read as its.
            self.last_timings = None;
        }
        self.quiet &= state == State::Listening;
        self.message = message.map(|s| bounded_message(&s));
        // Before any reply: a holder must see its session live as soon as it learns of it.
        self.track();
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
        let live = !opening.abandoned
            && opening.since.elapsed() < START_GRACE
            && reply.as_ref().is_none_or(|r| !r.is_closed());
        let response = match result {
            Ok(()) if live => {
                self.capture(opening);
                let mut r = self.status();
                r.session = Some(self.generation);
                r
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
        // A requester that left after the liveness check never learns of its session.
        if reply.is_some_and(|reply| reply.send(response).is_err()) && self.active() {
            let _ = self.cancel().await;
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
        self.retrying = false;
        self.hands_free = hands_free;
        self.mode = mode;
        self.delivery = delivery;
        self.started = Some(tokio::time::Instant::now());
        self.silent_since = self.started;
        self.segments = Segments::default();
        self.timings = Timings {
            open_ms: Some(ms(opened.elapsed())),
            hotkey_ms: t0_us
                .and_then(|t| monotonic_us().checked_sub(t))
                .map(|n| (n / 1000).min(u32::MAX as u64) as u32),
            ..Timings::default()
        };
        let stt = self.services.stt.clone();
        let cleanup = self.services.transformer.clone();
        if let Some(job) = self.warm_job.take() {
            job.abort();
        }
        self.warm_job = Some(tokio::spawn(async move {
            let cleanup = async {
                if let Some(cleanup) = cleanup {
                    let _ = cleanup.warm().await;
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                let _ = tokio::join!(stt.warm(), cleanup);
            })
            .await;
        }));
        let desktop = self.services.desktop.clone();
        let window_terms =
            self.config.privacy.window_terms && self.config.formatting.snap_to_dictionary;
        let (hints, recent) = (self.hints.clone(), self.recent.clone());
        let config = self.config.clone();
        let remember = config.privacy.history && config.stt.domain_hints;
        // Recent terms survive a daemon restart through history, read once, lazily.
        if remember && !std::mem::replace(&mut lock(&self.recent).seeded, true) {
            let (store, recent) = (self.store.clone(), self.recent.clone());
            tokio::spawn(async move {
                if let Ok((rows, _)) = store.history(50, 0, None).await {
                    let mut recent = lock(&recent);
                    // Oldest first, so the newest dictation's terms end up first.
                    for row in rows.iter().rev() {
                        recent.note(&row.text);
                    }
                }
            });
        }
        // A hold with text selected may rewrite it; the read shares the context's focus walk.
        let rewrite = mode == Mode::Dictation
            && !hands_free
            && delivery == Delivery::Inject
            && context.is_none()
            && self.config.cleanup.rewrite_selection
            && self.services.transformer.is_some();
        // Both queries run after capture opens and outside the command actor.
        let task = tokio::spawn(async move {
            let context = async {
                match context {
                    Some(c) => c,
                    None if delivery == Delivery::None && mode == Mode::Dictation => {
                        AppContext::default()
                    }
                    None if rewrite => desktop.context_with_selection().await.unwrap_or_default(),
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
            } else if !rewrite || xflow_platform::terminal_context(&target) {
                // Never rewrite in a terminal, whatever the desktop reports.
                target.selected_text = None;
            }
            // Microseconds, during the recording; never on the stop path. The pick
            // is published before the title read, then refined with its terms.
            let recent = if remember {
                lock(&recent).terms().to_vec()
            } else {
                Vec::new()
            };
            let pick = |title: &[String]| {
                xflow_core::hints::select(
                    &config,
                    &xflow_core::hints::Signals {
                        terminal: xflow_platform::terminal_context(&target),
                        app: target.app_id.as_deref(),
                        title,
                        recent: &recent,
                    },
                )
            };
            *lock(&hints) = Some(pick(&[]));
            // Editor and terminal apps only, by app id: other titles name documents, mail and
            // private tabs, including a browser whose web terminal has focus.
            // A read past 100 ms is dropped so it never holds the context back.
            let app = target.app_id.as_deref();
            let dev = xflow_platform::is_terminal(app) || xflow_platform::is_editor(app);
            let terms = if window_terms && dev {
                tokio::time::timeout(Duration::from_millis(100), desktop.window_title(&target))
                    .await
                    .ok()
                    .flatten()
                    .map(|title| xflow_core::title::terms(&title))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if !terms.is_empty() {
                *lock(&hints) = Some(pick(&terms));
            }
            (target, terms)
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
        // Before the final drain, so sends above this line started while recording.
        trace_segment("stop");
        self.started = None;
        self.silent_since = None;
        // Whatever the capture holds, even if its stream just failed.
        self.drain().await;
        let live = self.segments.count;
        let mut warning = None;
        match self.services.audio.stop().await {
            Ok(audio) => {
                if let Err(e) = audio_duration(&audio) {
                    self.segments = Segments::default();
                    return Err(e);
                }
                self.queue(audio, true);
            }
            // A microphone lost minutes into a dictation keeps what was already captured.
            Err(e) if live > 0 || self.segments.tail.is_some() => {
                let _ = self.services.audio.cancel().await;
                warning = Some(bounded_message(&format!(
                    "{e:#}; transcribed the audio captured before it"
                )));
                if let Some(tail) = self.segments.tail.take() {
                    self.send(tail, true);
                }
            }
            Err(e) => {
                let _ = self.services.audio.cancel().await;
                self.segments = Segments::default();
                self.abort_context();
                self.set_state(State::Error, Some(format!("{e:#}")));
                return Err(e);
            }
        }
        let segments = std::mem::take(&mut self.segments);
        if segments.audio_ms < u64::from(self.config.recording.min_ms) {
            self.abort_context();
            self.set_state(State::Idle, Some("Recording too short; discarded".into()));
            return Ok(());
        }
        if !segments.heard {
            self.abort_context();
            self.set_state(State::Idle, Some(NOT_CAUGHT.into()));
            return Ok(());
        }
        self.timings.audio_ms = segments.audio_ms.min(u64::from(u32::MAX)) as u32;
        self.cue(xflow_platform::Cue::Stop);
        self.set_state(State::Processing, None);
        let keep = match self.keeping().filter(|_| self.mode == Mode::Dictation) {
            Some(dir) => Keep {
                dir: Some(dir.to_owned()),
                created_at: retry::now(),
                #[cfg(test)]
                hook: self.keep_hook.clone(),
                ..Keep::default()
            },
            None => Keep::default(),
        };
        self.transcribe(segments.sent, live, warning, stopped, keep, completion);
        Ok(())
    }
    /// Pulls the audio captured so far into the segment pipeline.
    async fn drain(&mut self) {
        // On error the capture keeps its samples and stop() reports the failure.
        if let Ok(Some(audio)) = self.services.audio.drain().await {
            self.queue(audio, false);
        }
    }
    /// Appends captured audio and sends every finished segment; `last` sends the rest.
    fn queue(&mut self, audio: AudioClip, last: bool) {
        let mut tail = match self.segments.tail.take() {
            Some(mut tail)
                if (tail.sample_rate, tail.channels) == (audio.sample_rate, audio.channels) =>
            {
                tail.samples.extend_from_slice(&audio.samples);
                tail
            }
            Some(tail) => {
                self.send(tail, false);
                audio
            }
            None => audio,
        };
        while let Some(cut) = speech::split_point(&tail) {
            let rest = tail.samples.split_off(cut);
            let head = AudioClip {
                samples: std::mem::replace(&mut tail.samples, rest),
                ..tail
            };
            self.send(head, false);
        }
        if last {
            if !tail.samples.is_empty() {
                self.send(tail, true);
            }
        } else {
            self.segments.tail = Some(tail);
        }
    }
    /// Uploads one segment in the background; silence is counted, never sent.
    /// The dictation's edge silence is trimmed: before the first segment, after the last.
    fn send(&mut self, mut audio: AudioClip, last: bool) {
        let segments = &mut self.segments;
        segments.audio_ms += u64::from(audio_duration(&audio).unwrap_or(0));
        let index = segments.count;
        segments.count += 1;
        let activity = speech::activity(&audio, self.config.recording.silence_threshold);
        if !activity.speech {
            return;
        }
        segments.heard = true;
        trace_segment("send");
        let (stt, config, domain) = (
            self.services.stt.clone(),
            self.config.clone(),
            self.domain_hints(),
        );
        let keep = speech::trim(
            &audio,
            &activity.voiced,
            index == 0,
            last,
            config.recording.min_ms,
        );
        self.segments.sent.spawn(async move {
            // Every upload is 16 kHz mono anyway; holding that copy for a retry instead
            // of native-rate audio cuts in-flight memory 3–6x.
            let audio = tokio::task::spawn_blocking(move || {
                audio.samples.truncate(keep.end);
                audio.samples.drain(..keep.start);
                xflow_providers::upload_audio(&audio).unwrap_or(audio)
            })
            .await
            .expect("audio conversion panicked");
            (
                index,
                transcribe_segment(&*stt, audio, activity.low_energy, &config, domain).await,
            )
        });
    }
    fn transcribe(
        &mut self,
        mut sent: tokio::task::JoinSet<(usize, Piece)>,
        live: usize,
        warning: Option<String>,
        stopped: Instant,
        keep: Keep,
        completion: mpsc::Sender<Completion>,
    ) {
        let services = self.services.clone();
        let config = self.config.clone();
        let domain = self.domain_hints();
        let store = self.store.clone();
        let generation = self.generation;
        let epoch = self.epoch.clone();
        let session_mode = self.mode;
        let hands_free = self.hands_free;
        let mut context = self.context_job.take();
        let mut timings = self.timings.clone();
        // Read where the text lands while transcription runs. A read past 100 ms is
        // dropped; its blocking AX thread finishes on its own within the AX timeouts.
        let around = (session_mode == Mode::Dictation
            && self.delivery == Delivery::Inject
            && config.injection.smart_spacing)
            .then(|| {
                let desktop = services.desktop.clone();
                tokio::spawn(async move {
                    tokio::time::timeout(Duration::from_millis(100), desktop.neighbors())
                        .await
                        .ok()
                        .flatten()
                })
            });
        self.job = Some(tokio::spawn(async move {
            // A retry with nothing left to transcribe.
            let mut recovered = false;
            let result = async {
                let (mut pieces, mut failed) = (Vec::with_capacity(sent.len()), None);
                while let Some(joined) = sent.join_next().await {
                    match joined {
                        Ok(piece) => pieces.push(piece),
                        Err(e) => {
                            failed.get_or_insert(anyhow::Error::new(e).context("transcription task failed"));
                        }
                    }
                }
                pieces.sort_by_key(|(index, _)| *index);
                // A segmented dictation retries each failed segment once and keeps the rest:
                // one bad upload never discards minutes of transcribed speech. A single
                // short clip still fails fast. Exhausted failover chains are never repeated.
                let segmented = live > 0 || pieces.len() + usize::from(failed.is_some()) > 1;
                if let Some(e) = failed.take_if(|_| !segmented) {
                    return Err(e);
                }
                let mut providers = Vec::new();
                let (mut parts, mut language) = (Vec::with_capacity(pieces.len()), None);
                // Every part in order, while a failure can still keep them (see `retry`).
                let mut kept = Vec::new();
                for (index, piece) in pieces {
                    let piece = match piece {
                        Err((error, audio, low_energy)) if segmented && !error.is::<xflow_providers::FailoverExhausted>() => {
                            transcribe_segment(&*services.stt, audio, low_energy, &config, domain.clone()).await
                        }
                        piece => piece,
                    };
                    match piece {
                        Ok(transcript) => {
                            let provider = transcript.provider.unwrap_or_else(|| services.stt.name().into());
                            language = language.or(transcript.language);
                            let delivered = keep.delivered.get(index).copied().unwrap_or(false);
                            if keep.dir.is_some() {
                                kept.push((Part::Text { text: transcript.text.clone(), delivered, provider: Some(provider.clone()) }, None));
                            }
                            // Credit only the providers whose text this session delivers.
                            if !delivered || keep.full {
                                if !providers.contains(&provider) { providers.push(provider); }
                                parts.push(transcript.text);
                            }
                        }
                        Err((e, audio, low_energy)) => {
                            failed.get_or_insert(e);
                            if keep.dir.is_some() {
                                kept.push((Part::Audio { low_energy }, Some(audio)));
                            }
                        }
                    }
                }
                timings.stt_ms = ms(stopped.elapsed());
                let raw = join_parts(&parts, language.as_deref().or(config.stt.language.as_deref()));
                let note = match keep.dir.clone() {
                    Some(dir) if failed.is_some() => {
                        let delivering = !raw.is_empty();
                        let (parts, audio): (Vec<_>, Vec<_>) = kept
                            .into_iter()
                            .map(|(part, audio)| match part {
                                Part::Text { text, delivered, provider } => (Part::Text { text, delivered: delivered || delivering, provider }, audio),
                                part => (part, audio),
                            })
                            .unzip();
                        let audio = audio.into_iter().flatten().collect();
                        let (epoch, created_at) = (epoch.clone(), keep.created_at);
                        #[cfg(test)]
                        let hook = keep.hook.clone();
                        let saved = tokio::task::spawn_blocking(move || {
                            let staged = retry::stage(&dir, retry::session(generation), created_at, parts, audio)?;
                            #[cfg(test)]
                            if let Some(hook) = &hook { hook("staged"); }
                            // A canceled or cleared session keeps nothing.
                            retry::commit(staged, || epoch.load(Ordering::SeqCst) == generation)?;
                            #[cfg(test)]
                            if let Some(hook) = &hook { hook("committed"); }
                            anyhow::Ok(())
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.into()));
                        match saved {
                            Ok(()) => "; its audio is kept for 24 h: run xflow retry",
                            Err(e) => {
                                log("warn", &format!("could not keep the failed audio: {e:#}"));
                                ""
                            }
                        }
                    }
                    _ => "",
                };
                recovered = keep.retry && failed.is_none();
                if raw.len() > 32 * 1024 {
                    bail!("transcript exceeds 32 KiB safety limit");
                }
                if raw.is_empty() {
                    return failed.map_or(Ok(None), |e| Err(anyhow::anyhow!("{e:#}{note}")));
                }
                let mut warning = warning;
                for provider in providers.iter().filter(|id| id.as_str() != services.stt.name()) {
                    add_warning(&mut warning, &format!("Transcribed by {provider} ({} didn't answer)", services.stt.name()));
                }
                let provider = providers.join(", ");
                let model = if provider == services.stt.name() {
                    Some(services.stt.model().to_owned()).filter(|s| !s.is_empty())
                } else {
                    xflow_providers::find_provider(xflow_providers::ProviderKind::Stt, &provider)
                        .map(|p| p.default_model.to_owned())
                };
                let (mut target, terms) = if let Some(ref mut job) = context {
                    match tokio::time::timeout(Duration::from_secs(2), &mut *job).await {
                        Ok(Ok(c)) => c,
                        _ => {
                            job.abort();
                            Default::default()
                        }
                    }
                } else {
                    Default::default()
                };
                let terminal = xflow_platform::terminal_context(&target);
                // Title names are for this dictation's local formatting only (see `title::apply`).
                let titled = (!terms.is_empty()).then(|| {
                    let mut titled = config.clone();
                    titled.title_terms = terms;
                    titled
                });
                let (local, backtrack) = xflow_core::text::prepare(
                    &raw,
                    titled.as_ref().unwrap_or(&config),
                    terminal,
                    target.app_id.as_deref(),
                );
                // The dictation ended with "scratch that" (#75): nothing is cleaned or pasted.
                let discarded = backtrack == xflow_core::backtrack::Outcome::Discarded;
                let style = xflow_core::text::style(&config, target.app_id.as_deref());
                let mode = style.and_then(|s| s.mode).unwrap_or(config.cleanup.mode);
                let instructions = style
                    .and_then(|s| s.prompt.as_deref())
                    .or(config.cleanup.prompt.as_deref());
                // A short hold over a selection rewrites it (only a dictation's context job reads
                // one). Longer speech is replacement text: it pastes as dictated.
                let mut rewrite = session_mode == Mode::Dictation
                    && !hands_free
                    && !discarded
                    && mode != CleanupMode::Raw
                    && local.split_whitespace().count() <= REWRITE_WORDS
                    && target.selected_text.as_deref().is_some_and(|s| !s.trim().is_empty());
                if rewrite && target.selected_text.as_deref().is_some_and(|s| s.chars().count() > MAX_REWRITE_CHARS) {
                    rewrite = false;
                    add_warning(&mut warning, "Selection is over 6,000 characters; dictated instead of rewriting");
                }
                if session_mode == Mode::Dictation && !rewrite {
                    target.selected_text = None;
                }
                // A failed rewrite leaves the selection alone: the dictation goes to the clipboard.
                let mut copy = false;
                if let Some(e) = failed {
                    // A partial instruction must never run against the selection.
                    if session_mode == Mode::Command {
                        return Err(e.context("part of the command could not be transcribed; selection left untouched"));
                    }
                    if rewrite {
                        (rewrite, copy) = (false, true);
                        add_warning(&mut warning, REWRITE_FAILED);
                    }
                    add_warning(&mut warning, &format!("Part of the dictation could not be transcribed ({e:#}); the rest was delivered{note}"));
                }
                let command = session_mode == Mode::Command || rewrite;
                let selection = target.selected_text.as_deref().unwrap_or("");
                if selection.len() > MAX_MESSAGE_BYTES {
                    bail!("selected text exceeds safety limit");
                }
                let dictation = !command;
                let cleanup = services
                    .transformer
                    .filter(|_| !discarded && (!dictation || mode != CleanupMode::Raw));
                // Skip the round trip when it can't help (#68). Terminals keep their
                // deterministic text unless a style asks: a model adds periods,
                // capitalizes `git` and "fixes" flags.
                timings.cleanup_skipped = cleanup.is_some()
                    && dictation
                    && (terminal && style.and_then(|s| s.mode).is_none()
                        || mode == CleanupMode::Light && !xflow_providers::cleanup_can_help(&local));
                let mut outcome = if discarded {
                    Some(CleanupOutcome::Discarded)
                } else {
                    timings.cleanup_skipped.then_some(CleanupOutcome::Skipped)
                };
                let text = if let Some(transformer) = cleanup.filter(|_| !timings.cleanup_skipped) {
                    let cleanup_start = Instant::now();
                    let transformed = transformer
                        .transform(TransformRequest {
                            text: if command { selection } else { &local },
                            mode: if command && mode == CleanupMode::Raw {
                                CleanupMode::Light
                            } else {
                                mode
                            },
                            instructions,
                            command: command.then_some(local.as_str()),
                            app_id: target
                                .app_id
                                .as_deref()
                                .filter(|_| config.cleanup.app_context),
                            vocabulary: &config.dictionary.words,
                        })
                        ;
                    // Dictation never waits long on formatting: past a length-scaled
                    // budget (Groq takes ~0.2 s per 40 words) the local text is pasted.
                    // A rewrite gets the same budget, scaled by the selection it returns.
                    let words = local.split_whitespace().count() + selection.split_whitespace().count();
                    let budget = Duration::from_millis(3000 + 25 * words as u64);
                    let transformed = if dictation || rewrite {
                        tokio::time::timeout(budget, transformed).await.ok()
                    } else {
                        Some(transformed.await)
                    };
                    timings.cleanup_ms = Some(ms(cleanup_start.elapsed()));
                    match transformed.map(|r| r.ok().filter(|s| !s.trim().is_empty() && s.len() <= 32 * 1024)) {
                        // The model answered or rewrote instead of formatting.
                        Some(Some(s)) if dictation && xflow_providers::cleanup_rewrote(&local, &s, mode, &config.dictionary.words) => {
                            timings.cleanup_rejected = true;
                            outcome = Some(CleanupOutcome::Rejected);
                            add_warning(&mut warning, "Cleanup changed too much; original transcript retained");
                            local
                        }
                        // A rewrite that reads as a reply is never pasted over the selection.
                        Some(Some(s)) if !rewrite || !xflow_providers::command_answered(selection, &s) => {
                            outcome = Some(CleanupOutcome::Applied);
                            s
                        }
                        result if rewrite => {
                            (rewrite, copy) = (false, true);
                            outcome = Some(match result {
                                Some(Some(_)) => CleanupOutcome::Rejected,
                                None => CleanupOutcome::Slow,
                                Some(None) => CleanupOutcome::Failed,
                            });
                            add_warning(&mut warning, REWRITE_FAILED);
                            local
                        }
                        _ if !dictation => {
                            bail!("command transformation failed; selection left untouched")
                        }
                        None => {
                            outcome = Some(CleanupOutcome::Slow);
                            add_warning(&mut warning, "Cleanup was too slow; original transcript retained");
                            local
                        }
                        _ => {
                            outcome = Some(CleanupOutcome::Failed);
                            add_warning(&mut warning, "Cleanup failed; original transcript retained");
                            local
                        }
                    }
                } else {
                    local
                };
                let text = xflow_core::text::after_cleanup(&text, &config);
                let text = if terminal {
                    xflow_core::text::terminal_plain(&text)
                } else if rewrite && !text.trim().is_empty() {
                    // Replace the selection exactly, keeping only its own edge whitespace.
                    let lead = &selection[..selection.len() - selection.trim_start().len()];
                    let trail = &selection[selection.trim_end().len()..];
                    format!("{lead}{}{trail}", text.trim())
                } else {
                    text
                };
                if text.trim().is_empty() && !discarded {
                    bail!("no speech recognized after formatting");
                }
                validate_last_frame(&text)?;
                let entry = HistoryEntry {
                    text: text.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                    raw_text: (raw != text).then_some(raw),
                    app_id: target.app_id.clone(),
                    language: language.or(config.stt.language),
                    duration_ms: Some(timings.audio_ms as u64),
                    mode: if rewrite { Mode::Command } else { session_mode },
                    cleanup: outcome,
                    ..HistoryEntry::default()
                };
                // A generation check under the SQLite lock prevents a canceled save from
                // resurrecting history after clear_history has returned.
                let entry = match store.append_current(entry, epoch, generation).await {
                    Ok(entry) => entry,
                    Err(_) => {
                        add_warning(&mut warning, "History save failed; transcript available via last");
                        None
                    }
                };
                // A transcript that is ready first waits at most 20 ms, then goes unadjusted.
                let around = match around {
                    // Nothing is pasted after a discard, and a rewrite replaces the selection.
                    Some(job) if discarded || command => {
                        job.abort();
                        None
                    }
                    Some(job) => tokio::time::timeout(Duration::from_millis(20), job)
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .flatten(),
                    None => None,
                };
                Ok(Some(Processed {
                    rewrite,
                    copy,
                    provider,
                    model,
                    text,
                    around,
                    target,
                    entry,
                    timings,
                    stopped,
                    warning,
                }))
            }
            .await;
            // Only once the text is safe in `last` (and history): a later error keeps it.
            if let Some(dir) = keep.dir.as_deref().filter(|_| recovered && result.is_ok()) {
                if let Err(e) = retry::clear(dir) {
                    log(
                        "warn",
                        &format!("could not delete kept failed audio: {e:#}"),
                    );
                }
            }
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
        let processing = (self.state == State::Processing).then_some(self.generation);
        self.advance();
        if let Some(job) = self.job.take() {
            job.abort();
        }
        // A save the session committed just before the cancel goes with it.
        if let Some(generation) = processing {
            self.discard_session(generation);
        }
        self.segments = Segments::default();
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
        if matches!(
            request,
            Request::Status
                | Request::Subscribe
                | Request::Start { .. }
                | Request::Toggle { .. }
                | Request::Allow { .. }
        ) {
            if let Some(probe) = self.permission_probe {
                let permissions = probe();
                // Input Monitoring is the fallback for a tap Accessibility didn't make
                // hear keys: once it arrives, that tap must be replaced too.
                if permissions.input_monitoring
                    && !self
                        .permissions
                        .as_ref()
                        .is_some_and(|p| p.input_monitoring)
                {
                    self.hotkeys_blind = true;
                }
                self.rearm(permissions.events(), completion);
                self.permissions = Some(permissions);
            }
        }
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
                ..
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
                ..
            } => {
                if self.state == State::Listening {
                    self.stop(completion.clone()).await?;
                } else {
                    self.start(mode, context, t0_us, delivery, completion)?;
                    return Ok(None);
                }
            }
            // Canceling a retry discards the kept dictation; shutdown keeps it.
            Request::Cancel | Request::Retry { discard: true, .. } => {
                let retrying = self.retrying && self.active();
                if retrying || matches!(request, Request::Cancel) {
                    self.cancel().await?;
                }
                if retrying || matches!(request, Request::Retry { .. }) {
                    self.discard_kept()?;
                }
            }
            Request::Retry { full, delivery, .. } => self.retry(full, delivery, completion)?,
            Request::Abandon { session } => {
                if session == self.generation && self.active() {
                    self.cancel().await?;
                }
            }
            Request::Last => {
                let mut r = self.status();
                r.text = self.last.clone();
                return Ok(Some(r));
            }
            #[cfg(not(target_os = "macos"))]
            Request::Allow { .. } => bail!("allow is macOS only"),
            #[cfg(target_os = "macos")]
            Request::Allow { grant } => {
                if let Some(request) = self.permission_request {
                    // TCC may wait on its prompt; keep it off the actor.
                    let _ = std::thread::Builder::new().spawn(move || request(grant));
                }
            }
            Request::Status => {
                let mut r = self.status();
                r.learning = Some(self.learning());
                return Ok(Some(r));
            }
            Request::Subscribe | Request::Shutdown => (),
            _ => bail!("invalid actor request"),
        }
        Ok(Some(self.status()))
    }
    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let (completion_tx, mut completion_rx) = mpsc::channel(8);
        let mut levels = tokio::time::interval(Duration::from_millis(40));
        levels.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut drains = tokio::time::interval(DRAIN_EVERY);
        drains.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending: Option<Processed> = None;
        let mut queries = tokio::task::JoinSet::new();
        let mut deliveries = tokio::task::JoinSet::new();
        let mut desktop_ops = tokio::task::JoinSet::new();
        loop {
            // Catches transitions made without `set_state`, e.g. the final Success.
            self.track();
            let deadline = self
                .started
                .map(|s| s + Duration::from_secs(self.config.recording.max_seconds as u64));
            // Kept audio is deleted when it expires, even with no client asking.
            let expiry = self.kept.as_ref().filter(|_| !self.active()).map(|k| {
                tokio::time::Instant::now()
                    + Duration::from_secs((k.expires_at - retry::now()).max(0) as u64 + 1)
            });
            // First the waiting message, then the start grace.
            let opening = self
                .opening
                .as_ref()
                .filter(|o| !o.abandoned)
                .map(|o| o.since + if o.waiting { START_GRACE } else { MIC_SLOW });
            tokio::select! { biased;
                command = commands.recv() => {
                    let Some(Command { request, reply }) = command else { break; };
                    if reply.is_closed() && !matches!(request, Request::UndoLearned { .. }) { continue; }
                    while queries.try_join_next().is_some() {}
                    while desktop_ops.try_join_next().is_some() {}
                    if (is_query(&request) || matches!(request, Request::Reload | Request::UndoLearned { .. })) && queries.len() + desktop_ops.len() >= 32 { let _ = reply.send(Response::error(self.state, "daemon request capacity reached")); continue; }
                    if let Request::HistoryUpdate { id, text } = request {
                        let status = self.status(); let store = self.store.clone(); let path = self.config_path.clone(); let tx = completion_tx.clone(); let clock = self.dictionary_clock.clone();
                        queries.spawn(async move { let state = status.state; let response = edit_history(status, store, id, text, path, tx, clock).await.unwrap_or_else(|e| Response::error(state, format!("{e:#}"))); let _ = reply.send(response); });
                        continue;
                    }
                    if let Request::UndoLearned { op } = request {
                        // Taken before the work starts: a second click or a replay finds nothing.
                        let Some(change) = self.change.take_if(|c| op.is_none_or(|op| op == c.id)) else {
                            let _ = reply.send(Response::error(self.state, "That dictionary change can no longer be undone"));
                            continue;
                        };
                        self.set_change(None);
                        let status = self.status(); let path = self.config_path.clone(); let tx = completion_tx.clone(); let clock = self.dictionary_clock.clone();
                        queries.spawn(async move { let state = status.state; let response = undo(status, path, change, clock, tx).await.unwrap_or_else(|e| Response::error(state, format!("{e:#}"))); let _ = reply.send(response); });
                        continue;
                    }
                    if matches!(request, Request::Reload) {
                        if self.reloading {
                            // Its read may predate the write this request follows: one
                            // more reload after it answers every request that waited.
                            if self.reload_waiters.len() < MAX_RELOAD_WAITERS { self.reload_waiters.push(reply); }
                            else { let _ = reply.send(Response::error(self.state, "too many reloads queued; try again")); }
                        } else if let Some(work) = self.reload(vec![reply], &completion_tx) {
                            queries.spawn(work);
                        }
                        continue;
                    }
                    if matches!(request, Request::Allow { grant: Grant::Microphone }) && self.permission_probe.is_some() {
                        let status = self.status();
                        queries.spawn(async move { let _ = reply.send(microphone(status).await); });
                        continue;
                    }
                    if is_query(&request) {
                        if is_desktop_query(&request) && (!desktop_ops.is_empty() || self.delivering) { let _ = reply.send(Response::error(self.state, "desktop delivery is busy")); continue; }
                        if matches!(request, Request::PasteLast | Request::HistoryPaste { .. }) && self.active() { let _ = reply.send(Response::error(self.state, "wait for the current recording before pasting")); continue; }
                        if matches!(request, Request::ClearHistory) {
                            if self.active() { let _ = self.cancel().await; } else { self.advance(); }
                            self.last = None; self.last_timings = None; pending = None; lock(&self.recent).clear();
                            if let Err(e) = self.discard_kept() { log("warn", &format!("could not delete kept failed audio: {e:#}")); }
                        }
                        let status = self.status(); let store = self.store.clone(); let desktop = self.services.desktop.clone(); let last = self.last.clone();
                        let desktop_query = is_desktop_query(&request);
                        let work = async move { let state = status.state; let response = query(status, request, store, desktop, last).await.unwrap_or_else(|e| Response::error(state, format!("{e:#}"))); let _ = reply.send(response); };
                        if desktop_query { desktop_ops.spawn(work); } else { queries.spawn(work); }
                        continue;
                    }
                    if matches!(request, Request::Start { .. } | Request::Toggle { .. } | Request::Retry { discard: false, .. }) && !self.active() && !desktop_ops.is_empty() { let _ = reply.send(Response::error(self.state, "desktop delivery is busy")); continue; }
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
                        if self.keeping().is_some() { self.load_kept(); }
                        match result { Ok(Some(processed)) if processed.text.is_empty() => { self.abort_context(); self.discard_session(generation); self.set_state(State::Idle, Some(DISCARDED.into())); }, Ok(Some(mut processed)) => { if processed.rewrite { self.mode = Mode::Command; } self.last = Some(processed.text.clone()); if let Some((before, after)) = processed.around.take() { processed.text = xflow_core::text::adjust(&before, &after, &processed.text, &self.config.dictionary.words); } pending = Some(processed); }, Ok(None) => { self.abort_context(); self.set_state(State::Idle, Some(NOT_CAUGHT.into())); }, Err(e) => { self.abort_context(); self.set_state(State::Error, Some(format!("{e:#}"))); } }
                    }
                    // Late, from a canceled session: nothing it kept may stay.
                    Completion::Ready { generation, .. } => { self.discard_session(generation); self.publish(); }
                    Completion::Finished { generation, mut processed, result } => {
                        self.delivering = false;
                        if generation != self.generation { continue; }
                        self.abort_context();
                        match result {
                            Ok(outcome) => {
                                if outcome == Some(InjectionOutcome::ClipboardOnly) { self.notify("XFlow transcript copied", "Paste manually to insert your dictation."); }
                                if outcome == Some(InjectionOutcome::ClipboardOnly)
                                    && xflow_platform::terminal_controls(&processed.text, &processed.target) {
                                    add_warning(&mut processed.warning, "Terminal paste capability unverified");
                                }
                                let message = delivery_message(processed.warning.take(), outcome);
                                // One final success event: subscribers can trust its complete payload.
                                self.last_timings = Some(processed.timings.clone());
                                self.served = Some((processed.provider.clone(), processed.model.clone()));
                                if matches!(outcome, Some(InjectionOutcome::Pasted | InjectionOutcome::Typed)) && self.mode == Mode::Dictation && self.config.dictionary.auto_learn {
                                    if let Some(path) = self.config_path.clone() {
                                        let (finalize, finalized) = oneshot::channel();
                                        self.observing = Some(finalize);
                                        tokio::spawn(observe(self.services.desktop.clone(), processed.target.clone(), processed.text.clone(), finalized, path, completion_tx.clone(), self.dictionary_clock.clone(), self.learning.clone()));
                                    }
                                }
                                self.state = State::Success; self.message = message; let mut r = self.status();
                                r.text = Some(processed.text); r.injection = outcome; r.entry = processed.entry; r.timings = Some(processed.timings);
                                fit_final(&mut r);
                                // Pills alert only on a failed paste, never on an asked-for copy.
                                let _ = self.desktop_events.send(DesktopEvent { state: self.state, level: 0.0, message: self.message.clone().filter(|_| self.delivery == Delivery::Inject), mode: self.mode });
                                let _ = self.events.send(r);
                            }
                            Err(e) => self.set_state(State::Error, Some(format!("{INJECTION_FAILED}: {e}; transcript available via last"))),
                        }
                    }
                    Completion::Reloaded { result, replies, seq } => {
                        let r = self.reloaded(*result, seq);
                        for reply in replies { let _ = reply.send(r.clone()); }
                        // Requests that arrived meanwhile get a read of their own.
                        if !self.reload_waiters.is_empty() {
                            let replies = std::mem::take(&mut self.reload_waiters);
                            if let Some(work) = self.reload(replies, &completion_tx) { queries.spawn(work); }
                        }
                    }
                    Completion::Rearmed(result) => {
                        self.rearming = false;
                        self.hotkeys_blind = false; // a failure is not retried until reload
                        match result {
                            Ok(hotkeys) => { retire(std::mem::replace(&mut self.hotkeys, hotkeys)); if self.hotkey_warning.take().is_some() && self.state == State::Error && self.message.is_none() { self.state = State::Idle; } }
                            Err(warning) => self.hotkey_warning = Some(warning),
                        }
                        self.publish();
                    }
                    Completion::Learned { dictionary, seq, message, change } => self.learned(dictionary, seq, message, change),
                },
                _ = async {}, if pending.is_some() => {
                    let mut processed = pending.take().unwrap();
                    if self.state != State::Processing { continue; }
                    let generation = self.generation; let delivery = self.delivery; let desktop = self.services.desktop.clone(); let store = self.store.clone(); let tx = completion_tx.clone();
                    let recent = (self.config.privacy.history && self.config.stt.domain_hints).then(|| self.recent.clone());
                    self.delivering = true;
                    deliveries.spawn(async move {
                        let start = Instant::now();
                        // The selection must still be the one rewritten: same app, window and text.
                        let moved = delivery == Delivery::Inject && processed.rewrite && desktop.context_with_selection().await.ok().as_ref() != Some(&processed.target);
                        if moved { add_warning(&mut processed.warning, "Selection changed; rewrite copied"); }
                        let result = match delivery { Delivery::Inject if processed.copy || moved => desktop.copy(&processed.text).await.map(|_| Some(InjectionOutcome::ClipboardOnly)), Delivery::Inject => desktop.inject(&processed.text, &processed.target).await.map(Some), Delivery::Clipboard => desktop.copy(&processed.text).await.map(|_| Some(InjectionOutcome::ClipboardOnly)), Delivery::None => Ok(None) };
                        if delivery != Delivery::None { processed.timings.inject_ms = Some(ms(start.elapsed())); }
                        processed.timings.total_ms = ms(processed.stopped.elapsed());
                        if let Some(entry) = &mut processed.entry { entry.latency_ms = Some(processed.timings.total_ms as u64); if store.set_latency(entry.id, processed.timings.total_ms as u64).await.is_err() { add_warning(&mut processed.warning, "History latency update failed"); } }
                        // After delivery: the terms it mentions lead the next dictation's hints.
                        let text = recent.as_ref().map(|_| processed.text.clone());
                        let _ = tx.send(Completion::Finished { generation, processed, result }).await;
                        if let (Some(recent), Some(text)) = (recent, text) { lock(&recent).note(&text); }
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
                                if action == HotkeyAction::HandsFree { self.hands_free = true; self.message = Some(HANDS_FREE.into()); }
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
                                HotkeyAction::Start => Request::Start { mode: Mode::Dictation, context: None, t0_us: Some(monotonic_us()), delivery: Delivery::Inject, hold: false },
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
                    let level = self.services.audio.level();
                    if level.is_finite() && level > self.config.recording.silence_threshold { self.silent_since = None; }
                    else { self.silent_since.get_or_insert_with(tokio::time::Instant::now); }
                    self.publish();
                    if self.config.recording.auto_stop_secs > 0 && self.silent_since.is_some_and(|s| s.elapsed() >= Duration::from_secs(self.config.recording.auto_stop_secs as u64)) {
                        if let Err(e) = self.stop(completion_tx.clone()).await { self.set_state(State::Error, Some(format!("{e:#}"))); }
                        self.reset_hotkeys();
                    }
                }
                _ = drains.tick(), if self.state == State::Listening => self.drain().await,
                _ = async { if let Some(at) = opening { tokio::time::sleep_until(at.into()).await; } }, if opening.is_some() => {
                    if let Some(opening) = &mut self.opening {
                        if !std::mem::replace(&mut opening.waiting, true) {
                            self.set_state(State::Idle, Some(MIC_WAITING.into()));
                        } else {
                            opening.abandoned = true;
                            if let Some(reply) = opening.reply.take() {
                                let _ = reply.send(Response::error(self.state, format!("{MIC_WAITING}. Allow xflowd to use the microphone, then start again")));
                            }
                        }
                    }
                }
                _ = async { if let Some(at) = expiry { tokio::time::sleep_until(at).await; } }, if expiry.is_some() => {
                    self.load_kept();
                    self.publish();
                }
                _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } }, if deadline.is_some() => {
                    if let Err(e) = self.stop(completion_tx.clone()).await { self.set_state(State::Error, Some(format!("{e:#}"))); }
                    self.reset_hotkeys();
                }
            }
        }
        drop(completion_rx);
        queries.abort_all();
        // The undoable change lives in memory only.
        self.set_change(None);
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
/// Microphone status (and the system prompt while undecided) comes from a
/// short-lived `xflowd --microphone` child, so AVFoundation never loads here.
async fn microphone(r: Response) -> Response {
    #[cfg(target_os = "macos")]
    {
        use std::sync::atomic::AtomicBool;
        use tokio::io::AsyncBufReadExt;
        let mut r = r;
        // One child asks at a time; the others only read the status.
        static ASKING: AtomicBool = AtomicBool::new(false);
        let ask = !ASKING.swap(true, Ordering::AcqRel);
        let result = async {
            let mut child = tokio::process::Command::new(std::env::current_exe()?)
                .args(["--microphone", if ask { "request" } else { "check" }])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            let stdout = child.stdout.take().context("no child output")?;
            let mut line = String::new();
            tokio::time::timeout(
                Duration::from_secs(5),
                tokio::io::BufReader::new(stdout).read_line(&mut line),
            )
            .await
            .context("microphone check timed out")??;
            anyhow::Ok((child, line.trim().to_string()))
        }
        .await;
        match result {
            Ok((mut child, status)) => {
                if let Some(permissions) = &mut r.permissions {
                    permissions.microphone = match status.as_str() {
                        "authorized" => Some(true),
                        "not determined" => None,
                        _ => Some(false),
                    };
                }
                if ask {
                    // The asking child waits for the user's answer; free the slot after.
                    tokio::spawn(async move {
                        let _ = child.wait().await;
                        ASKING.store(false, Ordering::Release);
                    });
                }
            }
            Err(e) => {
                if ask {
                    ASKING.store(false, Ordering::Release);
                }
                return Response::error(r.state, format!("{e:#}"));
            }
        }
        r
    }
    #[cfg(not(target_os = "macos"))]
    r
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
/// Learn from the user's corrections of a pasted dictation, then count the
/// outcome for status and doctor and log it at debug level. Neither contains
/// field text.
#[allow(clippy::too_many_arguments)]
async fn observe(
    desktop: Arc<dyn Desktop>,
    target: AppContext,
    inserted: String,
    finalize: oneshot::Receiver<()>,
    path: PathBuf,
    completion: mpsc::Sender<Completion>,
    clock: Arc<AtomicU64>,
    learning: Arc<std::sync::Mutex<Learning>>,
) {
    let mut counts = Counts::default();
    let watched = watch_counted(&*desktop, &target, &inserted, finalize, &mut counts).await;
    let outcome: String = match watched {
        Ok((original, corrected)) => {
            match learn(path, original, corrected, completion, clock, true).await {
                Ok(Ok((outcome, _))) => outcome.into(),
                Ok(Err(reason)) => format!("filtered:{reason}"),
                Err(_) => {
                    log("warn", "could not save learned dictionary words");
                    "save-failed".into()
                }
            }
        }
        Err(Some(outcome)) => outcome.into(),
        // The daemon is stopping or another dictation replaced this one.
        Err(None) => "cancelled".into(),
    };
    let mut total = lock(&learning);
    total.watches = total.watches.saturating_add(1);
    // Outcome names come from a fixed set; the bound only guards the map.
    if total.outcomes.len() < 64 || total.outcomes.contains_key(&outcome) {
        let count = total.outcomes.entry(outcome.clone()).or_default();
        *count = count.saturating_add(1);
    }
    total.reads = total.reads.saturating_add(counts.reads);
    total.unread_max = total.unread_max.max(counts.unread_max);
    total.late = total.late.saturating_add(counts.late);
    total.merged = total.merged.saturating_add(counts.merged);
    total.editing = total.editing.saturating_add(counts.editing);
    drop(total);
    log("debug", &format!("auto-learn: {outcome}"));
}
/// Minimum gap between field reads; reads follow every change notification.
const FIELD_READ_GAP: Duration = Duration::from_millis(20);
/// A changed span is re-read after this pause. When the dictation then leaves
/// the field, its last span is trusted only if it was read twice in a row, or
/// if the clear was the only change since that read, so a word still being
/// typed is never learned. Notifications carry no text: when more than one
/// change happened between two reads, the states in between are unknown (a
/// clear, then a new message) and nothing is learned from that field. A change
/// with no notification at all (an app that doesn't report edits, or a missed
/// one) is unknown in the same way: only announced edits are ever learned.
/// The same pause is a grace after the field is found empty: a change folded
/// into the clear, or one arriving within it, means the last read may not be
/// the last spelling, so nothing is learned. That narrows truncated saves to
/// apps that coalesce or drop notifications, which no reader can detect; the
/// notice showing the exact saved word, and its undo, are the backstop.
const FIELD_SETTLE: Duration = Duration::from_millis(60);
/// The span to trust when the dictation has left the field: the last one read,
/// if the clear was the only change after it. With no unread change the clear's
/// own notification is still in flight, so the edits before it are unknown too.
fn trusted_before_clear(unread: usize, last: &str) -> String {
    if unread == 1 {
        last.to_owned()
    } else {
        String::new()
    }
}
/// Pin the field a dictation was pasted into and return (dictated, corrected)
/// spans. The field is read ~120 ms after the paste to anchor the span, then
/// after each change notification, keeping the last span that is still
/// recognisably the dictation. A read that no longer looks like it, while the
/// user is visibly retyping part of it (see `Receipt::editing`), keeps the
/// watch going; if the watch ends there, nothing is learned. Once the
/// dictation leaves the field (a chat app's Enter, a clear, a rewrite) the
/// observation is over: its last span is learned only if nothing typed after
/// it went unread (see FIELD_SETTLE), and nothing typed there later is read.
/// Otherwise the window ends when focus leaves the field (after a 250 ms
/// grace), the next dictation starts, or after 60 s, with one last read. Only
/// the span and 16 characters each side stay in memory. Err holds the outcome
/// when there is nothing to learn, None when cancelled.
#[cfg(test)]
async fn watch_edit(
    desktop: &dyn Desktop,
    target: &AppContext,
    inserted: &str,
    finalize: oneshot::Receiver<()>,
) -> std::result::Result<(String, String), Option<&'static str>> {
    watch_counted(desktop, target, inserted, finalize, &mut Counts::default()).await
}
/// `watch_edit`, counting reads and notifications into `counts`.
async fn watch_counted(
    desktop: &dyn Desktop,
    target: &AppContext,
    inserted: &str,
    mut finalize: oneshot::Receiver<()>,
    counts: &mut Counts,
) -> std::result::Result<(String, String), Option<&'static str>> {
    use tokio::time::{sleep_until, timeout, Instant};
    // Too long to ever compare (see corrections): no reads at all.
    if inserted.split_whitespace().count() > xflow_core::learn::MAX_DIFF_WORDS {
        return Err(Some("too-long"));
    }
    async fn read(field: &mut Box<dyn xflow_core::FieldWatch>) -> Option<xflow_core::FieldText> {
        timeout(Duration::from_millis(250), field.read())
            .await
            .ok()
            .flatten()
    }
    // Takes the notifications already queued, without waiting: a change that
    // arrived during a read, or behind a finalize, is still a change. Returns
    // (changes, merged changes).
    fn drain(
        field: &mut Box<dyn xflow_core::FieldWatch>,
        recheck: &mut Option<Instant>,
    ) -> (usize, usize) {
        let (mut changed, mut merged) = (0, 0);
        while let Some(event) = field.try_next() {
            match event {
                FieldEvent::Changed => changed += 1,
                FieldEvent::Merged => merged += 1,
                FieldEvent::Focus => *recheck = Some(Instant::now() + Duration::from_millis(250)),
            }
        }
        (changed, merged)
    }
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    tokio::time::sleep(Duration::from_millis(120)).await;
    let started = Instant::now();
    let mut field = timeout(Duration::from_secs(1), desktop.watch_field(target))
        .await
        .ok()
        .flatten()
        .ok_or(Some("unreadable"))?;
    let deadline = started + Duration::from_secs(60);
    let mut receipt: Option<xflow_core::learn::Receipt> = None;
    // The last span read, and the last one read twice in a row.
    let (mut good, mut settled) = (String::new(), String::new());
    let (mut dirty, mut left, mut lost) = (true, false, false);
    // The last read showed the user retyping part of the dictation.
    let mut editing = false;
    // Whether the field has announced any change since the paste.
    let mut notified = false;
    // Change notifications handled since the last read; events that queued
    // during a read count too, which only errs towards not learning.
    let mut changes = 0;
    // Changes folded into another since the last read (`FieldEvent::Merged`).
    let mut merged = 0;
    let (mut last_read, mut next_read) = (started, started);
    let mut recheck: Option<Instant> = None;
    loop {
        tokio::select! {
            biased;
            finalized = &mut finalize => {
                if finalized.is_err() {
                    return Err(None);
                }
                break;
            }
            _ = sleep_until(deadline) => break,
            _ = sleep_until(recheck.unwrap_or(deadline)), if recheck.is_some() => {
                recheck = None;
                if !field.focused().await {
                    left = true;
                    break;
                }
            }
            // Queued notifications are counted before any read (see `changes`).
            event = field.next() => match event {
                Some(FieldEvent::Changed) => {
                    notified = true;
                    changes += 1;
                    dirty = true;
                    next_read = last_read + FIELD_READ_GAP;
                }
                Some(FieldEvent::Merged) => {
                    merged += 1;
                    counts.merged = counts.merged.saturating_add(1);
                }
                Some(FieldEvent::Focus) => recheck = Some(Instant::now() + Duration::from_millis(250)),
                None => break,
            },
            _ = sleep_until(next_read), if dirty => {
                dirty = false;
                let unread = std::mem::take(&mut changes);
                let folded = std::mem::take(&mut merged);
                let text = read(&mut field).await;
                last_read = Instant::now();
                counts.reads = counts.reads.saturating_add(1);
                counts.unread_max = counts.unread_max.max(count(unread));
                let Some(anchored) = &receipt else {
                    let readable = text.is_some();
                    receipt = text.and_then(|t| xflow_core::learn::Receipt::new(&t.value, inserted, t.caret));
                    match &receipt {
                        Some(r) => {
                            good = r.inserted().to_owned();
                            settled = good.clone();
                        }
                        // Electron and Chromium can show the paste late.
                        None if started.elapsed() < Duration::from_secs(1) => {
                            dirty = true;
                            next_read = last_read + Duration::from_millis(50);
                        }
                        None => return Err(Some(if readable { "no-receipt" } else { "unreadable" })),
                    }
                    continue;
                };
                let Some(text) = text else {
                    // A failed read saw nothing; its changes are still unread.
                    changes += unread;
                    merged += folded;
                    continue;
                };
                if unread > 1 {
                    return Err(Some("unsettled"));
                }
                // Changes announced while the read ran: after its value, so they
                // are carried to the next read, but they count if this read
                // finds the dictation gone.
                let (late, late_merged) = drain(&mut field, &mut recheck);
                counts.late = counts.late.saturating_add(count(late));
                counts.merged = counts.merged.saturating_add(count(late_merged));
                merged += late_merged;
                if late > 0 {
                    notified = true;
                    changes += late;
                }
                match anchored.corrected(&text.value) {
                    Some(span) if span == good => {
                        settled = good.clone();
                        editing = false;
                    }
                    // The span changed without a notification for it.
                    Some(_) if unread + late == 0 => return Err(Some("unsettled")),
                    Some(span) => {
                        good = span.to_owned();
                        editing = false;
                        dirty = true;
                        next_read = last_read + FIELD_SETTLE;
                    }
                    // Retyping part of the dictation: keep the last good span.
                    None if anchored.editing(&text.value, &good) => {
                        if unread + late == 0 {
                            return Err(Some("unsettled"));
                        }
                        editing = true;
                        counts.editing = counts.editing.saturating_add(1);
                    }
                    // Sent or cleared mid-edit: its last spelling was never read.
                    None if editing => return Err(Some("editing")),
                    None => {
                        // A change folded into the clear may be the last letter.
                        settled = if folded + late_merged > 0 {
                            String::new()
                        } else {
                            trusted_before_clear(unread + late, &good)
                        };
                        lost = true;
                        break;
                    }
                }
                if late > 0 {
                    dirty = true;
                    next_read = last_read + FIELD_READ_GAP;
                }
            }
        }
    }
    let receipt = receipt.ok_or(Some("no-receipt"))?;
    // The user moved on: what the field shows now is final, under the same
    // rules. A field that never announced a change is not read again: in an
    // app that doesn't report edits, the dictation may already be sent and
    // the field may hold the next message.
    // Finalize, focus leave and the deadline win over queued notifications in
    // the select above; count those before deciding.
    if !lost {
        let (pending, pending_merged) = drain(&mut field, &mut recheck);
        notified |= pending > 0;
        changes += pending;
        merged += pending_merged;
        counts.merged = counts.merged.saturating_add(count(pending_merged));
    }
    if !lost && notified {
        let unread = changes;
        counts.unread_max = counts.unread_max.max(count(unread));
        if unread > 1 {
            return Err(Some("unsettled"));
        }
        let text = read(&mut field).await;
        counts.reads = counts.reads.saturating_add(1);
        // Still changing while the final read ran: its value is not final.
        let (late, late_merged) = drain(&mut field, &mut recheck);
        if late + late_merged > 0 {
            counts.late = counts.late.saturating_add(count(late));
            return Err(Some("unsettled"));
        }
        match text {
            Some(text) => match receipt.corrected(&text.value) {
                Some(span) if span == good => settled = good.clone(),
                Some(_) if unread == 0 => return Err(Some("unsettled")),
                Some(span) => {
                    good = span.to_owned();
                    settled = good.clone();
                }
                None if editing || receipt.editing(&text.value, &good) => {
                    return Err(Some("editing"))
                }
                None => {
                    settled = if merged > 0 {
                        String::new()
                    } else {
                        trusted_before_clear(unread, &good)
                    };
                    lost = true;
                }
            },
            // The last read showed a retype in progress, not a spelling.
            None if editing => return Err(Some("editing")),
            // Nothing changed since the last read: that span is final.
            None if unread == 0 => settled = good.clone(),
            None => return Err(Some("unsettled")),
        }
    }
    // A change arriving just after the field emptied may be the last letter's
    // (its notification lagged the read), or the clear's own after a letter
    // that was never read: either way the last read was not final.
    if lost && !settled.is_empty() && good != receipt.inserted() {
        tokio::time::sleep(FIELD_SETTLE).await;
        let (late, late_merged) = drain(&mut field, &mut recheck);
        if late + late_merged > 0 {
            counts.late = counts.late.saturating_add(count(late));
            counts.merged = counts.merged.saturating_add(count(late_merged));
            settled.clear();
        }
    }
    drop(field);
    if good == receipt.inserted() {
        return Err(Some(if lost {
            "anchors-lost"
        } else if left {
            "focus-left-before-edit"
        } else {
            "no-edit"
        }));
    }
    // The dictation left the field while its latest spelling was still changing.
    if good != settled {
        return Err(Some("unsettled"));
    }
    Ok((receipt.inserted().to_owned(), good))
}
fn quoted(words: &[String]) -> String {
    words
        .iter()
        .map(|w| format!("'{w}'"))
        .collect::<Vec<_>>()
        .join(", ")
}
/// The one learning path for corrected pastes and history edits: find close word
/// corrections, save them to config.toml, and hand the new dictionary to the actor.
/// A term the dictionary already had moves to the front (hint limits keep it);
/// a second correction of one may add a narrow replacement (see
/// `DictionaryConfig::derive`). Every save keeps the file's exact previous bytes
/// so it can be undone once; `notice`: the pill may offer that.
/// Returns the outcome and a user-facing message, or why nothing was learned.
async fn learn(
    path: PathBuf,
    original: String,
    corrected: String,
    completion: mpsc::Sender<Completion>,
    clock: Arc<AtomicU64>,
    notice: bool,
) -> Result<std::result::Result<(&'static str, String), &'static str>> {
    let saved = tokio::task::spawn_blocking(move || -> Result<_> {
        let _lock = crate::config_edit::lock_within(&path, LOCK_WAIT)?;
        let (mut file, before) = ConfigFile::load_exact(&path)?;
        let mut dictionary = file.config()?.dictionary;
        if !dictionary.auto_learn {
            return Ok(Err("disabled"));
        }
        let found = match xflow_core::learn::review(&original, &corrected, &dictionary) {
            Ok(found) => found,
            Err(reason) => return Ok(Err(reason)),
        };
        let promoted = dictionary.promote(&found.known);
        let rules = dictionary.derive(&found.known);
        let added = dictionary.learn(&found.new);
        if added.is_empty() && rules.is_empty() && promoted.is_empty() {
            return Ok(Err(if found.new.is_empty() {
                found.rejected
            } else {
                "dictionary-full"
            }));
        }
        // An edit that couldn't be undone exactly is not made automatically.
        if before
            .as_ref()
            .is_some_and(|b| b.len() > crate::config_edit::MAX_UNDO_BYTES)
        {
            return Ok(Err("too-large"));
        }
        file.set_typed("dictionary.words", &dictionary.words)?;
        file.set_typed("dictionary.learned", &dictionary.learned)?;
        if !rules.is_empty() {
            file.set_typed("dictionary.replacements", &dictionary.replacements)?;
        }
        let revision = file.save_tracked()?;
        // Numbered while the lock is held, after the rename (see `Completion::Learned`).
        let seq = clock.fetch_add(1, Ordering::SeqCst) + 1;
        let undo = Undo::new(&path, before, revision);
        Ok(Ok((dictionary, seq, undo, added, rules, promoted)))
    })
    .await??;
    let (dictionary, seq, undo, added, rules, promoted) = match saved {
        Ok(saved) => saved,
        Err(reason) => return Ok(Err(reason)),
    };
    let rules: Vec<String> = rules.into_iter().map(|r| r.to).collect();
    let mut notice_words = Vec::new();
    for word in added.iter().chain(&rules).chain(&promoted) {
        if !notice_words.contains(word) {
            notice_words.push(word.clone());
        }
    }
    let (outcome, message, words) = if !added.is_empty() {
        (
            "learned",
            format!("Added {} to your dictionary", quoted(&added)),
            notice_words,
        )
    } else if !rules.is_empty() {
        (
            "corrected",
            format!("Saved your correction to {}", quoted(&rules)),
            notice_words,
        )
    } else {
        (
            "promoted",
            format!("{} is already in your dictionary", quoted(&promoted)),
            notice_words,
        )
    };
    // A reorder alone is not announced or offered for undo.
    let announce = outcome != "promoted";
    let change = announce.then(|| Change {
        // Opaque: never derived from the words.
        id: std::collections::hash_map::RandomState::new().hash_one(seq) | 1,
        undo,
        correction: outcome == "corrected",
        words,
        notice,
    });
    let _ = completion
        .send(Completion::Learned {
            dictionary,
            seq,
            message: announce.then(|| message.clone()),
            change,
        })
        .await;
    Ok(Ok((outcome, message)))
}
/// Undo one automatic dictionary change (see `config_edit::Undo`) under the
/// config lock, then hand the restored dictionary to the actor like a learn.
async fn undo(
    mut r: Response,
    path: Option<PathBuf>,
    change: Change,
    clock: Arc<AtomicU64>,
    completion: mpsc::Sender<Completion>,
) -> Result<Response> {
    let path = path.context("config path is unavailable")?;
    let Change { undo, words, .. } = change;
    let (dictionary, seq) = tokio::task::spawn_blocking(move || -> Result<_> {
        let _lock = crate::config_edit::lock_within(&path, LOCK_WAIT)?;
        let config = undo.restore(&path)?;
        Ok((config.dictionary, clock.fetch_add(1, Ordering::SeqCst) + 1))
    })
    .await??;
    let _ = completion
        .send(Completion::Learned {
            dictionary,
            seq,
            message: None,
            change: None,
        })
        .await;
    r.message = Some(format!(
        "Undid the dictionary change for {}",
        quoted(&words)
    ));
    Ok(r)
}
/// Save a corrected history entry, then learn from the correction.
async fn edit_history(
    mut r: Response,
    store: Store,
    id: i64,
    text: String,
    path: Option<PathBuf>,
    completion: mpsc::Sender<Completion>,
    clock: Arc<AtomicU64>,
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
        r.message = match learn(path, original, text, completion, clock, false).await {
            Ok(learned) => learned.ok().map(|(_, message)| message),
            Err(e) => Some(format!("Transcript saved; dictionary not updated: {e:#}")),
        };
    }
    fit_final(&mut r);
    Ok(r)
}
pub(crate) const INJECTION_FAILED: &str = "Injection failed";
/// Nothing above `recording.silence_threshold` since listening began: muted or wrong mic.
pub(crate) const NO_SIGNAL: &str = "Can't hear you — check mic";
const NO_SIGNAL_AFTER: Duration = Duration::from_secs(2);
/// Shown for the last minute before `recording.max_seconds` stops capture.
pub(crate) const ENDS_SOON: &str = "Recording stops in 1 min";
const ENDS_SOON_SECS: u64 = 60;
/// The dictation ended with "scratch that" (#75); the pill shows it too.
pub(crate) const DISCARDED: &str = "Discarded";
#[cfg(target_os = "macos")]
pub(crate) const COPIED: &str = "Copied — press ⌘V to paste";
#[cfg(not(target_os = "macos"))]
pub(crate) const COPIED: &str = "Copied — press Ctrl+V to paste";

/// Warnings accumulate: a later failure must never hide an earlier lost segment.
fn add_warning(warning: &mut Option<String>, message: &str) {
    *warning = Some(bounded_message(&match warning.take() {
        Some(earlier) => format!("{earlier}; {message}"),
        None => message.into(),
    }));
}
pub(crate) const UNCERTAIN: &str = "Pasted — check it landed";

fn delivery_message(warning: Option<String>, outcome: Option<InjectionOutcome>) -> Option<String> {
    let delivery = match outcome {
        Some(InjectionOutcome::ClipboardOnly) => Some(COPIED),
        Some(InjectionOutcome::Uncertain) => Some(UNCERTAIN),
        _ => None,
    };
    if let Some(delivery) = delivery {
        Some(bounded_message(&match warning {
            Some(warning) => format!("{delivery}; {warning}"),
            None => delivery.into(),
        }))
    } else {
        warning.map(|s| bounded_message(&s))
    }
}
fn native_services(old: &Config, config: &Config, services: Services) -> Result<Services> {
    let stt = xflow_providers::build_stt(&config.stt, config.privacy.offline)?;
    let transformer = build_transformer(config)?;
    #[cfg(feature = "test-support")]
    if let Some((audio, desktop)) = crate::test_support::adapters()? {
        return Ok(Services {
            audio,
            desktop,
            stt,
            transformer,
        });
    }
    let audio = if old.recording != config.recording {
        native_audio(&config.recording)?
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
async fn transcribe_segment(
    stt: &dyn SpeechToText,
    audio: AudioClip,
    low_energy: bool,
    config: &Config,
    domain: Vec<&'static str>,
) -> Piece {
    let options = TranscriptionOptions {
        vocabulary: vocabulary(config),
        domain: domain.clone(),
        ..TranscriptionOptions::default()
    };
    match stt.transcribe(audio.clone(), options).await {
        // Near-silent audio: drop the provider's stock hallucinations and hint echoes.
        Ok(mut transcript) if low_energy && speech::suspect(&transcript.text, config, &domain) => {
            transcript.text.clear();
            Ok(transcript)
        }
        Ok(transcript) => Ok(transcript),
        Err(error) => Err((error, audio, low_energy)),
    }
}
fn join_parts(parts: &[String], language: Option<&str>) -> String {
    // Chinese, Japanese and Thai don't put spaces between words.
    let separator = match language.map(str::to_ascii_lowercase).as_deref() {
        Some("zh" | "ja" | "th" | "chinese" | "japanese" | "thai") => "",
        _ => " ",
    };
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(separator)
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
/// A poisoned lock still holds usable hint state.
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
/// Test-support builds record segment sends against capture stop. The e2e
/// test compares that order; production builds never write it.
#[cfg(feature = "test-support")]
fn trace_segment(event: &str) {
    if std::env::var("XFLOW_TEST_AUDIO").ok().as_deref() != Some("wav") {
        return;
    }
    let Ok(root) = std::env::var("XFLOW_TEST_ROOT") else {
        return;
    };
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::path::Path::new(&root).join("segments.trace"))
    {
        Ok(file) => file,
        Err(_) => return,
    };
    let _ = std::io::Write::write_all(
        &mut file,
        format!("{event} {}\n", monotonic_us()).as_bytes(),
    );
}
#[cfg(not(feature = "test-support"))]
fn trace_segment(_: &str) {}

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
    let test_adapters = crate::test_support::adapters()?;
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
    // Under the daemon lock, so two starts never race on the file.
    let marker = crate::paths::data_dir()?.join("migrated-recording-limit");
    let config = match crate::config_edit::migrate_recording_limit(&config_path, &marker) {
        Ok(true) => Config::load(&config_path)?,
        Ok(false) => config,
        Err(error) => {
            log(
                "warn",
                &format!("could not migrate the legacy recording limit: {error:#}"),
            );
            config
        }
    };
    let listener = UnixListener::bind(&path)?;
    let _guard = SocketGuard { path, _lock: lock };
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&_guard.path, std::fs::Permissions::from_mode(0o600))?;
    let stt = xflow_providers::build_stt(&config.stt, config.privacy.offline)?;
    let transformer = build_transformer(&config)?;
    let native_adapters = || -> Result<(Arc<dyn AudioCapture>, Arc<dyn Desktop>)> {
        Ok((
            native_audio(&config.recording)?,
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
    {
        let store = store.clone();
        tokio::spawn(async move {
            if store.upgrade_search_index().await.is_err() {
                log(
                    "warn",
                    "history search index upgrade failed; history remains available",
                );
            }
        });
    }
    let (commands_tx, commands_rx) = mpsc::channel(32);
    let (events_tx, _) = broadcast::channel(64);
    let (desktop_tx, desktop_rx) = broadcast::channel(32);
    // The pill's notice of the latest undoable dictionary change, latest only.
    let (notice_tx, _) = watch::channel(None);
    #[cfg(target_os = "macos")]
    let pill = {
        let (events, notices, config) = (
            desktop_tx.subscribe(),
            notice_tx.subscribe(),
            config_path.clone(),
        );
        tokio::spawn(async move {
            if native {
                crate::overlay_pill::run(events, notices, config).await;
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
    engine.notice = notice_tx;
    engine.persistence_warning = persistence_warning;
    engine.kept_dir = Some(crate::paths::data_dir()?.join("failed"));
    engine.load_kept();
    #[cfg(target_os = "macos")]
    {
        if native {
            let permissions = xflow_platform::permissions();
            if let Some(warning) = permissions.warning() {
                log("warn", warning);
            }
            // Nothing is asked here (a prompt mid-install is a surprise): grants are
            // requested only when onboarding or Check-up sends Allow.
            engine.hotkeys_blind = !permissions.events() && engine.config.hotkey.enabled;
            engine.permission_probe = Some(xflow_platform::permissions);
            engine.permission_request = Some(xflow_platform::request_access);
            engine.permissions = Some(permissions);
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
    let live = engine.live.subscribe();
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
                let commands = commands_tx.clone(); let events = events_tx.clone(); let live = live.clone();
                clients.spawn(async move { let _permit = permit; let _ = connection(stream, commands, events, live).await; });
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
    mut live: watch::Receiver<Option<u64>>,
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
    let hold = matches!(
        request,
        Request::Start { hold: true, .. } | Request::Toggle { hold: true, .. }
    );
    // Subscribe first, then get an actor snapshot, avoiding missed state transitions.
    let mut receiver = events.subscribe();
    let (reply_tx, mut reply_rx) = oneshot::channel();
    let closed = read_frame::<Request, _>(&mut reader);
    tokio::pin!(closed);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(9);
    let answer = tokio::time::timeout_at(deadline, async {
        commands
            .send(Command {
                request,
                reply: reply_tx,
            })
            .await?;
        tokio::select! { biased;
            result = &mut closed => { let _ = result?; Ok(None) }
            reply = &mut reply_rx => reply.context("daemon is shutting down").map(Some),
        }
    })
    .await;
    if !matches!(answer, Ok(Ok(Some(_)))) {
        // The actor may have started capture just as the requester left; take that reply.
        reply_rx.close();
        if let Ok(response) = reply_rx.try_recv() {
            abandon(&commands, response.session).await;
        }
    }
    // A live but busy daemon says so; closing silently reads as "daemon not running".
    let Some(response) = answer.unwrap_or_else(|_| {
        let mut r = Response::error(
            State::Idle,
            "xflowd is running but busy and did not answer in time; try again in a moment",
        );
        r.protocol = Some(PROTOCOL_VERSION);
        Ok(Some(r))
    })?
    else {
        return Ok(());
    };
    if let Err(e) = write_frame(&mut write, &response).await {
        abandon(&commands, response.session).await;
        return Err(e);
    }
    if let Some(session) = response.session.filter(|_| hold) {
        // The requester owns its session: whatever ends the client (Ctrl-C, SIGHUP,
        // kill -9) closes this socket, and the recording ends with it.
        tokio::select! {
            _ = &mut closed => abandon(&commands, Some(session)).await,
            // Returning closes the socket: the session is over, so the hold is too.
            _ = async { live.wait_for(|live| *live != Some(session)).await.is_ok() } => (),
        }
    }
    if subscribe {
        // Keep the read future alive: canceling read_line can discard partial frames.
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
/// Cancels a session whose start reply never reached its requester.
async fn abandon(commands: &mpsc::Sender<Command>, session: Option<u64>) {
    let Some(session) = session else { return };
    let (reply, response) = oneshot::channel();
    // Keep the reply receiver: the actor skips commands whose requester has gone.
    if commands
        .send(Command {
            request: Request::Abandon { session },
            reply,
        })
        .await
        .is_ok()
    {
        let _ = response.await;
    }
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

    #[tokio::test]
    async fn test_audio_environment_is_debug_only() {
        const CHILD: &str = "XFLOW_AUDIO_GATE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let expected = if cfg!(debug_assertions) {
                Some(PathBuf::from("/nonexistent/qa.wav"))
            } else {
                None
            };
            assert_eq!(test_audio_path(), expected);
            #[cfg(debug_assertions)]
            {
                let audio = native_audio(&Default::default()).unwrap();
                let error = audio.start().await.unwrap_err();
                assert!(format!("{error:#}").contains("XFLOW_TEST_AUDIO"));
                #[cfg(feature = "test-support")]
                assert!(crate::test_support::adapters().unwrap().is_none());
            }
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon::tests::test_audio_environment_is_debug_only",
            ])
            .env(CHILD, "1")
            .env("XFLOW_TEST_AUDIO", "/nonexistent/qa.wav")
            .env_remove("XFLOW_TEST_SINK")
            .env_remove("XFLOW_TEST_ROOT")
            .status()
            .unwrap();
        assert!(status.success());
    }

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
        fn microphone(&self) -> Option<String> {
            Some("Fixture mic (Bluetooth headphones kept in high quality)".into())
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
                provider: None,
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
        context: Option<AppContext>,
        guard_terminal: bool,
        copies: Mutex<Vec<String>>,
        injections: Mutex<Vec<String>>,
        block_context: Option<Arc<Notify>>,
        empty_selection: bool,
        /// The focused text field; pastes append to it.
        field: Arc<Mutex<Option<String>>>,
        /// Scripted notifications for the next field watch.
        field_events: Mutex<Option<mpsc::UnboundedReceiver<FieldEvent>>>,
        /// Focus has left the watched field.
        field_left: Arc<AtomicBool>,
        /// Field watches started (one per observed paste).
        watches: AtomicUsize,
        /// Text around the caret, for smart spacing, and how long reading it takes.
        around: Option<(&'static str, &'static str)>,
        around_delay: Duration,
        outcome: Option<InjectionOutcome>,
        /// The focused window's title, reads of it, and how long one takes.
        title: Option<&'static str>,
        title_reads: AtomicUsize,
        title_delay: Duration,
        /// The editable selection a hold may rewrite; a paste over it replaces it.
        rewrite: Option<String>,
        /// The focused field is secure: delivery is refused.
        refuse: bool,
        /// The selection is gone after the first read.
        selection_moves: bool,
        selection_reads: AtomicUsize,
    }
    struct ScriptedField {
        field: Arc<Mutex<Option<String>>>,
        events: Option<mpsc::UnboundedReceiver<FieldEvent>>,
        left: Arc<AtomicBool>,
    }
    #[async_trait]
    impl xflow_core::FieldWatch for ScriptedField {
        async fn next(&mut self) -> Option<FieldEvent> {
            match &mut self.events {
                Some(events) => events.recv().await,
                None => std::future::pending().await,
            }
        }
        fn try_next(&mut self) -> Option<FieldEvent> {
            self.events.as_mut()?.try_recv().ok()
        }
        async fn read(&mut self) -> Option<xflow_core::FieldText> {
            let value = self.field.lock().unwrap().clone()?;
            let caret = Some(value.encode_utf16().count());
            Some(xflow_core::FieldText { value, caret })
        }
        async fn focused(&mut self) -> bool {
            !self.left.load(Ordering::SeqCst)
        }
    }
    #[async_trait]
    impl Desktop for RecordingDesktop {
        async fn context(&self) -> Result<AppContext> {
            self.contexts.fetch_add(1, Ordering::SeqCst);
            if let Some(block) = &self.block_context {
                block.notified().await;
            }
            Ok(self.context.clone().unwrap_or_else(|| AppContext {
                app_id: Some("Org.Editor".into()),
                ..AppContext::default()
            }))
        }
        async fn selection(&self) -> Result<Option<String>> {
            Ok((!self.empty_selection).then(|| "selected draft".into()))
        }
        async fn context_with_selection(&self) -> Result<AppContext> {
            let mut context = self.context().await?;
            let moved =
                self.selection_moves && self.selection_reads.fetch_add(1, Ordering::SeqCst) > 0;
            context.selected_text = self.rewrite.clone().filter(|_| !moved);
            Ok(context)
        }
        async fn copy(&self, text: &str) -> Result<()> {
            self.copies.lock().unwrap().push(text.into());
            Ok(())
        }
        async fn inject(&self, text: &str, target: &AppContext) -> Result<InjectionOutcome> {
            if self.refuse {
                bail!("refusing delivery into a secure field");
            }
            if let (Some(selected), Some(field)) =
                (&target.selected_text, self.field.lock().unwrap().as_mut())
            {
                self.injections.lock().unwrap().push(text.into());
                *field = field.replacen(selected.as_str(), text, 1);
                return Ok(InjectionOutcome::Pasted);
            }
            if self.guard_terminal && xflow_platform::terminal_controls(text, target) {
                self.copy(text).await?;
                return Ok(InjectionOutcome::ClipboardOnly);
            }
            self.injections.lock().unwrap().push(text.into());
            if let Some(field) = self.field.lock().unwrap().as_mut() {
                field.push_str(text);
            }
            Ok(self.outcome.unwrap_or(InjectionOutcome::Pasted))
        }
        async fn watch_field(&self, _: &AppContext) -> Option<Box<dyn xflow_core::FieldWatch>> {
            self.field.lock().unwrap().as_ref()?;
            self.watches.fetch_add(1, Ordering::SeqCst);
            Some(Box::new(ScriptedField {
                field: self.field.clone(),
                events: self.field_events.lock().unwrap().take(),
                left: self.field_left.clone(),
            }))
        }
        async fn neighbors(&self) -> Option<(String, String)> {
            tokio::time::sleep(self.around_delay).await;
            self.around.map(|(b, a)| (b.into(), a.into()))
        }
        async fn window_title(&self, _: &AppContext) -> Option<String> {
            self.title_reads.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.title_delay).await;
            self.title.map(Into::into)
        }
    }
    struct ImmediateStt {
        calls: Arc<AtomicUsize>,
        text: &'static str,
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
                provider: None,
                text: self.text.into(),
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
    /// Records each request; replies with `.1`, or "my signature".
    struct RecordingTransformer(Mutex<Vec<TransformCall>>, Option<&'static str>);
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
            Ok(self.1.unwrap_or("my signature").into())
        }
    }
    /// Opted in, with light cleanup.
    fn rewriting() -> Config {
        let mut config = Config::default();
        config.cleanup.rewrite_selection = true;
        config.cleanup.mode = CleanupMode::Light;
        config
    }
    /// One hold saying `said` over "Meet on Tuesday" with `desktop`'s selection, until it
    /// succeeds, fails or is discarded.
    async fn hold_over(
        config: Config,
        said: &'static str,
        desktop: RecordingDesktop,
        transformer: Arc<dyn TextTransformer>,
    ) -> (Arc<RecordingDesktop>, Response) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db"), true, 5).unwrap();
        let desktop = Arc::new(RecordingDesktop {
            field: Arc::new(Mutex::new(Some("Meet on Tuesday".into()))),
            ..desktop
        });
        let (mut engine, tx, rx, mut events, _) = fixture(config, desktop.clone(), store);
        engine.services.transformer = Some(transformer);
        engine.services.stt = Arc::new(ImmediateStt {
            calls: Arc::default(),
            text: said,
        });
        let task = tokio::spawn(engine.run(rx));
        assert_eq!(ask(&tx, Request::start()).await.state, State::Listening);
        assert_eq!(ask(&tx, Request::Stop).await.state, State::Processing);
        let r = settled(&mut events).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
        (desktop, r)
    }
    async fn settled(events: &mut broadcast::Receiver<Response>) -> Response {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let r = events.recv().await.unwrap();
                if matches!(r.state, State::Success | State::Error | State::Idle) {
                    return r;
                }
            }
        })
        .await
        .unwrap()
    }
    const SAID: &str = "um post gres is ready";
    #[tokio::test]
    async fn a_hold_over_a_selection_rewrites_it_and_history_records_it() {
        // The reply's edge whitespace is dropped; the selection's own is kept.
        for selection in ["Tuesday", " Tuesday"] {
            let transformer = Arc::new(RecordingTransformer(Mutex::default(), Some(" Monday ")));
            let desktop = RecordingDesktop {
                rewrite: Some(selection.into()),
                ..RecordingDesktop::default()
            };
            let (desktop, r) = hold_over(rewriting(), SAID, desktop, transformer.clone()).await;
            assert_eq!(r.state, State::Success, "{:?}", r.message);
            assert_eq!(
                desktop.field.lock().unwrap().as_deref(),
                Some("Meet on Monday")
            );
            let calls = transformer.0.lock().unwrap().clone();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, selection);
            assert!(calls[0].1.contains("is ready"), "{:?}", calls[0].1);
            let entry = r.entry.unwrap();
            assert_eq!(entry.mode, Mode::Command);
            assert_eq!(entry.text.trim(), "Monday");
            assert_eq!(entry.raw_text.as_deref(), Some(SAID));
            assert_eq!(
                desktop.watches.load(Ordering::SeqCst),
                0,
                "model text is never learned from"
            );
        }
    }
    #[tokio::test]
    async fn a_hold_dictates_without_a_usable_selection() {
        let terminal = AppContext {
            app_id: Some("Org.Editor".into()),
            terminal_input: true,
            ..AppContext::default()
        };
        let mut off = rewriting();
        off.cleanup.rewrite_selection = false;
        let mut raw = rewriting();
        raw.cleanup.mode = CleanupMode::Raw;
        let long = "x".repeat(6001);
        // Over the 64 KiB frame limit: still dictated and saved, never an error.
        let huge = "x".repeat(70 * 1024);
        for (case, config, rewrite, context, notice) in [
            ("no selection", rewriting(), None, None, false),
            (
                "blank selection",
                rewriting(),
                Some(" \n".to_owned()),
                None,
                false,
            ),
            (
                "terminal",
                rewriting(),
                Some("Tuesday".into()),
                Some(terminal),
                false,
            ),
            (
                "default off",
                Config::default(),
                Some("Tuesday".into()),
                None,
                false,
            ),
            ("turned off", off, Some("Tuesday".into()), None, false),
            ("raw mode", raw, Some("Tuesday".into()), None, false),
            ("too long", rewriting(), Some(long), None, true),
            ("over 64 KiB", rewriting(), Some(huge), None, true),
        ] {
            let transformer = Arc::new(RecordingTransformer::default());
            let desktop = RecordingDesktop {
                rewrite,
                context,
                ..RecordingDesktop::default()
            };
            let (desktop, r) = hold_over(config, SAID, desktop, transformer.clone()).await;
            assert_eq!(r.state, State::Success, "{case}: {:?}", r.message);
            assert!(transformer.0.lock().unwrap().is_empty(), "{case}");
            assert_eq!(desktop.injections.lock().unwrap().len(), 1, "{case}");
            assert_eq!(
                r.text.as_deref().map(str::trim_end),
                Some("post gres is ready"),
                "{case}"
            );
            assert_eq!(
                r.message.is_some_and(|m| m.contains("6,000")),
                notice,
                "{case}"
            );
            assert_eq!(r.entry.map(|e| e.mode), Some(Mode::Dictation), "{case}");
        }
    }
    #[tokio::test]
    async fn long_speech_over_a_selection_is_dictated_not_rewritten() {
        let said = "we should meet on monday at the office near the station after lunch";
        let transformer = Arc::new(RecordingTransformer::default());
        let desktop = RecordingDesktop {
            rewrite: Some("Tuesday".into()),
            ..RecordingDesktop::default()
        };
        let (desktop, r) = hold_over(rewriting(), said, desktop, transformer.clone()).await;
        assert_eq!(r.state, State::Success, "{:?}", r.message);
        // Only dictation cleanup ran: no command, the selection never sent.
        let calls = transformer.0.lock().unwrap().clone();
        assert!(
            calls.iter().all(|c| c.1.is_empty() && c.0 != "Tuesday"),
            "{calls:?}"
        );
        assert_eq!(desktop.injections.lock().unwrap().len(), 1);
        assert_eq!(r.entry.map(|e| e.mode), Some(Mode::Dictation));
    }
    #[tokio::test]
    async fn scratch_that_over_a_selection_pastes_nothing() {
        let transformer = Arc::new(RecordingTransformer::default());
        let desktop = RecordingDesktop {
            rewrite: Some("Tuesday ".into()),
            ..RecordingDesktop::default()
        };
        let (desktop, r) = hold_over(
            rewriting(),
            "Um, scratch that.",
            desktop,
            transformer.clone(),
        )
        .await;
        assert_eq!(
            (r.state, r.message.as_deref()),
            (State::Idle, Some(DISCARDED))
        );
        assert!(transformer.0.lock().unwrap().is_empty());
        assert!(desktop.injections.lock().unwrap().is_empty());
        assert!(desktop.copies.lock().unwrap().is_empty());
        assert_eq!(
            desktop.field.lock().unwrap().as_deref(),
            Some("Meet on Tuesday")
        );
    }
    #[tokio::test]
    async fn a_hands_free_session_never_rewrites() {
        let desktop = Arc::new(RecordingDesktop {
            rewrite: Some("Tuesday".into()),
            field: Arc::new(Mutex::new(Some("Meet on Tuesday".into()))),
            ..RecordingDesktop::default()
        });
        let (mut engine, tx, rx, mut events, _) =
            fixture(rewriting(), desktop.clone(), unused_store());
        let transformer = Arc::new(RecordingTransformer::default());
        engine.services.transformer = Some(transformer.clone());
        let (keys, key_events) = mpsc::channel(4);
        engine.hotkeys = Some(Box::new(FakeHotkeys(key_events)));
        let task = tokio::spawn(engine.run(rx));
        keys.send(HotkeyAction::Start).await.unwrap();
        keys.send(HotkeyAction::HandsFree).await.unwrap();
        let listening = hotkey_state(&mut events, State::Listening).await;
        assert_eq!(listening.message.as_deref(), Some(HANDS_FREE));
        ask(&tx, Request::Stop).await;
        assert_eq!(settled(&mut events).await.state, State::Success);
        assert!(transformer.0.lock().unwrap().is_empty());
        assert_eq!(desktop.injections.lock().unwrap().len(), 1);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    fn unused_store() -> Store {
        Store::open(std::path::Path::new("unused"), false, 10).unwrap()
    }
    #[tokio::test]
    async fn a_hold_in_a_secure_field_is_refused() {
        let transformer = Arc::new(RecordingTransformer::default());
        let desktop = RecordingDesktop {
            refuse: true,
            ..RecordingDesktop::default()
        };
        let (desktop, r) = hold_over(rewriting(), SAID, desktop, transformer.clone()).await;
        assert_eq!(r.state, State::Error);
        assert!(r.message.unwrap().contains("secure field"));
        assert!(transformer.0.lock().unwrap().is_empty());
        assert_eq!(
            desktop.field.lock().unwrap().as_deref(),
            Some("Meet on Tuesday")
        );
    }
    #[tokio::test]
    async fn a_failed_slow_or_answering_rewrite_leaves_the_selection_and_copies_the_dictation() {
        let answer: Arc<dyn TextTransformer> = Arc::new(RecordingTransformer(
            Mutex::default(),
            Some("Sure! Here is the rewritten text: Monday"),
        ));
        for (case, transformer, outcome) in [
            (
                "failed",
                Arc::new(FailingCleanup) as Arc<dyn TextTransformer>,
                CleanupOutcome::Failed,
            ),
            ("answered", answer, CleanupOutcome::Rejected),
            // The dictation budget (3 s plus 25 ms a word), not the 20 s provider timeout.
            ("slow", Arc::new(Stalled), CleanupOutcome::Slow),
        ] {
            let desktop = RecordingDesktop {
                rewrite: Some("Tuesday".into()),
                ..RecordingDesktop::default()
            };
            let started = Instant::now();
            let (desktop, r) = hold_over(rewriting(), SAID, desktop, transformer).await;
            assert!(started.elapsed() < Duration::from_secs(4), "{case}");
            assert_eq!(r.state, State::Success, "{case}: {:?}", r.message);
            let message = r.message.unwrap();
            assert!(
                message.starts_with(COPIED) && message.contains(REWRITE_FAILED),
                "{case}: {message}"
            );
            assert!(desktop.injections.lock().unwrap().is_empty(), "{case}");
            assert_eq!(
                desktop.field.lock().unwrap().as_deref(),
                Some("Meet on Tuesday"),
                "{case}"
            );
            assert_eq!(
                *desktop.copies.lock().unwrap(),
                ["post gres is ready "],
                "{case}"
            );
            let entry = r.entry.unwrap();
            assert_eq!(
                (entry.mode, entry.cleanup),
                (Mode::Dictation, Some(outcome)),
                "{case}"
            );
        }
    }
    #[tokio::test]
    async fn a_selection_changed_before_delivery_gets_the_rewrite_copied() {
        let transformer = Arc::new(RecordingTransformer(Mutex::default(), Some("Monday")));
        let desktop = RecordingDesktop {
            rewrite: Some("Tuesday".into()),
            selection_moves: true,
            ..RecordingDesktop::default()
        };
        let (desktop, r) = hold_over(rewriting(), SAID, desktop, transformer).await;
        assert_eq!(r.state, State::Success, "{:?}", r.message);
        assert!(r.message.unwrap().contains("Selection changed"));
        assert!(desktop.injections.lock().unwrap().is_empty());
        assert_eq!(
            desktop.field.lock().unwrap().as_deref(),
            Some("Meet on Tuesday")
        );
        assert_eq!(*desktop.copies.lock().unwrap(), ["Monday"]);
    }
    fn fixture(
        mut config: Config,
        desktop: Arc<dyn Desktop>,
        store: Store,
    ) -> (
        Engine,
        mpsc::Sender<Command>,
        mpsc::Receiver<Command>,
        broadcast::Receiver<Response>,
        Arc<AtomicUsize>,
    ) {
        // The fake transcript's "post gres" is a dev pack form; these tests
        // need it to reach delivery and learning unfixed.
        config.vocab.dev = false;
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
                    text: "um post gres is ready",
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
    struct BackupStt;
    #[async_trait]
    impl SpeechToText for BackupStt {
        fn name(&self) -> &str {
            "groq"
        }
        async fn transcribe(&self, _: AudioClip, _: TranscriptionOptions) -> Result<Transcript> {
            Ok(Transcript {
                text: "Recovered dictation.".into(),
                language: None,
                provider: Some("openai".into()),
            })
        }
    }
    #[tokio::test]
    async fn backup_provider_is_pasted_once_and_named_in_history_watch_and_status() {
        for history in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("history.db"), history, 10).unwrap();
            let desktop = Arc::new(RecordingDesktop::default());
            let (mut engine, tx, rx, mut events, _) =
                fixture(Config::default(), desktop.clone(), store.clone());
            engine.services.stt = Arc::new(BackupStt);
            let task = tokio::spawn(engine.run(rx));
            assert!(ask(&tx, Request::start()).await.ok);
            assert!(ask(&tx, Request::Stop).await.ok);
            let event = success(&mut events).await;
            assert_eq!(desktop.injections.lock().unwrap().len(), 1);
            assert_eq!(event.provider.as_deref(), Some("openai"));
            assert!(event
                .message
                .as_deref()
                .unwrap()
                .contains("Transcribed by openai (groq didn't answer)"));
            let watch_json = serde_json::to_value(&event).unwrap();
            assert_eq!(watch_json["provider"], "openai");
            let status = ask(&tx, Request::Status).await;
            assert_eq!(status.provider.as_deref(), Some("openai"));
            if history {
                assert_eq!(event.entry.unwrap().provider, "openai");
                let response = ask(
                    &tx,
                    Request::History {
                        limit: 10,
                        offset: 0,
                        query: None,
                    },
                )
                .await;
                assert_eq!(response.history[0].provider, "openai");
            } else {
                assert!(event.entry.is_none());
            }
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn smart_spacing_adjusts_the_paste_but_not_history_or_last() {
        // A read that misses the transcript is skipped, never waited out.
        for (smart, delay, pasted) in [
            (true, 0, "Post gres is ready "),
            (false, 0, "post gres is ready "),
            (true, 500, "post gres is ready "),
        ] {
            let desktop = Arc::new(RecordingDesktop {
                around: Some(("Done. ", "")),
                around_delay: Duration::from_millis(delay),
                ..RecordingDesktop::default()
            });
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
            let mut config = Config::default();
            config.injection.smart_spacing = smart;
            let (engine, tx, rx, mut events, _) = fixture(config, desktop.clone(), store);
            let task = tokio::spawn(engine.run(rx));
            ask(&tx, Request::start()).await;
            ask(&tx, Request::Stop).await;
            let done = success(&mut events).await;
            assert_eq!(*desktop.injections.lock().unwrap(), [pasted]);
            assert_eq!(done.entry.unwrap().text, "post gres is ready ");
            assert_eq!(
                ask(&tx, Request::Last).await.text.as_deref(),
                Some("post gres is ready ")
            );
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn a_retry_gets_smart_spacing_exactly_when_a_dictation_would() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        for (delivery, pasted, copied) in [
            (Delivery::Inject, Some("Post gres is ready "), None),
            (Delivery::Clipboard, None, Some("post gres is ready ")),
        ] {
            let desktop = Arc::new(RecordingDesktop {
                around: Some(("Done. ", "")),
                ..RecordingDesktop::default()
            });
            let mut config = Config::default();
            config.privacy.keep_failed_audio = true;
            let store = Store::open(std::path::Path::new("unused"), false, 5).unwrap();
            let (mut engine, tx, rx, mut events, _) = fixture(config, desktop.clone(), store);
            let clip = AudioClip {
                samples: vec![0.2; 16_000],
                sample_rate: 16_000,
                channels: 1,
            };
            let audio = vec![Part::Audio { low_energy: false }];
            retry::save(&dir, retry::now(), audio, vec![clip]).unwrap();
            engine.kept_dir = Some(dir.clone());
            engine.load_kept();
            let task = tokio::spawn(engine.run(rx));
            let request = Request::Retry {
                full: false,
                delivery,
                discard: false,
            };
            assert_eq!(ask(&tx, request).await.state, State::Processing);
            success(&mut events).await;
            assert_eq!(
                desktop
                    .injections
                    .lock()
                    .unwrap()
                    .first()
                    .map(String::as_str),
                pasted
            );
            assert_eq!(
                desktop.copies.lock().unwrap().first().map(String::as_str),
                copied
            );
            assert_eq!(
                ask(&tx, Request::Last).await.text.as_deref(),
                Some("post gres is ready ")
            );
            assert!(!dir.exists());
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn status_and_subscription_report_daemon_permissions_and_tap_evidence() {
        let (mut engine, tx, rx, _, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 5).unwrap(),
        );
        // A created tap does not prove the daemon's current signature is allowed.
        let (_keys, key_events) = mpsc::channel(4);
        engine.hotkeys = Some(Box::new(FakeHotkeys(key_events)));
        engine.permission_probe = Some(|| Permissions {
            accessibility: false,
            input_monitoring: false,
            keyboard_events: None,
            microphone: None,
        });
        let task = tokio::spawn(engine.run(rx));
        for request in [Request::Status, Request::Subscribe] {
            let response = ask(&tx, request).await;
            assert_eq!(
                response.microphone.as_deref(),
                Some("Fixture mic (Bluetooth headphones kept in high quality)")
            );
            let permissions = response.permissions.unwrap();
            assert!(!permissions.accessibility && !permissions.input_monitoring);
            assert!(response
                .message
                .unwrap()
                .contains(xflow_core::ipc::ACCESSIBILITY_FIX));
        }
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
        for (accessibility, input, received, warning) in [
            // Accessibility also lets the tap hear keys: no Input Monitoring needed.
            (true, false, Some(true), None),
            (
                true,
                false,
                Some(false),
                Some(xflow_core::ipc::UNVERIFIED_SHORTCUT),
            ),
            (
                false,
                true,
                Some(true),
                Some(xflow_core::ipc::ACCESSIBILITY_FIX),
            ),
            (
                true,
                true,
                Some(false),
                Some(xflow_core::ipc::UNVERIFIED_SHORTCUT),
            ),
            (true, true, Some(true), None),
            (true, true, None, None),
        ] {
            let permissions = Permissions {
                accessibility,
                input_monitoring: input,
                keyboard_events: received,
                microphone: None,
            };
            assert_eq!(permissions.warning(), warning);
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn granting_input_monitoring_rearms_a_tap_blind_despite_accessibility() {
        static GRANTED: AtomicBool = AtomicBool::new(false);
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        let (mut engine, tx, rx, _, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 5).unwrap(),
        );
        let (_keys, key_events) = mpsc::channel(4);
        engine.hotkeys = Some(Box::new(FakeHotkeys(key_events)));
        // Started with Accessibility on, so the tap wasn't marked blind; it heard
        // nothing anyway, and Input Monitoring is the fallback being granted.
        let probe: fn() -> Permissions = || Permissions {
            accessibility: true,
            input_monitoring: GRANTED.load(Ordering::SeqCst),
            keyboard_events: None,
            microphone: None,
        };
        engine.hotkeys_blind = false;
        engine.permissions = Some(probe());
        engine.permission_probe = Some(probe);
        engine.permission_request = Some(|grant| {
            assert_eq!(grant, Grant::InputMonitoring);
            ASKED.fetch_add(1, Ordering::SeqCst);
        });
        let created = Arc::new(AtomicUsize::new(0));
        let (new_keys, new_events) = mpsc::channel(4);
        let new_events = Mutex::new(Some(new_events));
        let count = created.clone();
        engine.hotkey_factory = Some(Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Box::new(FakeHotkeys(
                new_events.lock().unwrap().take().context("created twice")?,
            ))))
        }));
        let task = tokio::spawn(engine.run(rx));
        let grant = Grant::InputMonitoring;
        assert!(ask(&tx, Request::Allow { grant }).await.ok);
        ask(&tx, Request::Status).await;
        assert_eq!(created.load(Ordering::SeqCst), 0, "still denied");
        GRANTED.store(true, Ordering::SeqCst);
        // The next client probe replaces the tap; later probes leave it alone.
        for _ in 0..3 {
            ask(&tx, Request::Status).await;
        }
        new_keys.send(HotkeyAction::Start).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while ask(&tx, Request::Status).await.state != State::Listening
                || ASKED.load(Ordering::SeqCst) != 1
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(created.load(Ordering::SeqCst), 1);
        assert_eq!(ASKED.load(Ordering::SeqCst), 1);
        ask(&tx, Request::Cancel).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }

    struct ClipAudio(Vec<f32>);
    #[async_trait]
    impl AudioCapture for ClipAudio {
        async fn start(&self) -> Result<()> {
            Ok(())
        }
        async fn stop(&self) -> Result<AudioClip> {
            Ok(AudioClip {
                samples: self.0.clone(),
                sample_rate: 16_000,
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
    #[tokio::test]
    async fn speech_free_sessions_never_upload_save_or_deliver() {
        // Sine segments of (milliseconds, amplitude) at 16 kHz.
        let tone = |parts: &[(usize, f32)]| -> Vec<f32> {
            parts
                .iter()
                .flat_map(|&(ms, amplitude)| std::iter::repeat_n(amplitude, ms * 16))
                .enumerate()
                .map(|(i, a)| a * (i as f32 * std::f32::consts::TAU * 220.0 / 16_000.0).sin())
                .collect()
        };
        let silence = vec![0.0; 40_000];
        // #51's quiet-room median frame RMS was 0.00064–0.00154.
        let hiss: Vec<_> = (0..40_000)
            .map(|i| if i % 2 == 0 { 0.003 } else { -0.003 })
            .collect();
        let mut click = silence.clone();
        click[400..720].fill(0.5);
        // #51's silent sessions peaked near 0.03 RMS on a key thump; STT hallucinated on it.
        let thump = tone(&[(500, 0.001), (300, 0.04), (1700, 0.001)]);
        let soft = tone(&[(500, 0.001), (1000, 0.012), (1000, 0.001)]);
        let mut cases = vec![
            (silence, false, false, "unexpected"),
            (hiss, false, false, "unexpected"),
            (click, false, false, "unexpected"),
            (thump, true, false, "Thank you."),
            (soft.clone(), true, true, "soft speech passes"),
            (soft.clone(), true, true, "PostgreSQL is ready"),
            // Review of #55: speech with little surrounding quiet must not become the floor,
            (
                tone(&[(80, 0.001), (500, 0.015)]),
                true,
                true,
                "soft speech passes",
            ),
            // loud short phrases are not hallucinations regardless of duration or occupancy,
            (tone(&[(260, 0.2), (200, 0.001)]), true, true, "Thank you."),
            (tone(&[(400, 0.2), (5000, 0.001)]), true, true, "Thank you."),
            // and a brief word inside a longer clip reaches STT.
            (
                tone(&[(400, 0.001), (160, 0.2), (400, 0.001)]),
                true,
                true,
                "OK.",
            ),
        ];
        for text in [
            "Thank you.",
            "Thanks for watching!",
            ".",
            "O",
            "Fox jumps over the lazy dog.",
            "PostgreSQL",
            "PostgreSQL, Anupam",
            "custom hint",
        ] {
            cases.push((soft.clone(), true, false, text));
            if text != "." {
                cases.push((vec![0.2; 40_000], true, true, text));
            }
        }
        for (samples, upload, deliver, text) in cases {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("history.db"), true, 5).unwrap();
            let desktop = Arc::new(RecordingDesktop::default());
            let mut config = Config::default();
            config.dictionary.words = vec!["PostgreSQL".into(), "Anupam".into()];
            config.stt.vocabulary = vec!["custom hint".into()];
            let (mut engine, tx, rx, mut events, calls) =
                fixture(config, desktop.clone(), store.clone());
            engine.services.audio = Arc::new(ClipAudio(samples));
            engine.services.stt = Arc::new(ImmediateStt {
                calls: calls.clone(),
                text,
            });
            let task = tokio::spawn(engine.run(rx));
            ask(&tx, Request::start()).await;
            let stopped = ask(&tx, Request::Stop).await;
            if deliver {
                success(&mut events).await;
            } else {
                let empty = if upload {
                    hotkey_state(&mut events, State::Idle).await
                } else {
                    stopped
                };
                assert_eq!(empty.state, State::Idle);
                assert_eq!(empty.message.as_deref(), Some("Didn't catch anything"));
                assert!(ask(&tx, Request::Last).await.text.is_none());
            }
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(upload), "{text}");
            assert_eq!(
                store.stats().await.unwrap().sessions,
                u64::from(deliver),
                "{text}"
            );
            assert_eq!(
                desktop.injections.lock().unwrap().len(),
                usize::from(deliver),
                "{text}"
            );
            assert!(desktop.copies.lock().unwrap().is_empty());
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }

    /// A microphone whose first drain hands over `drained` samples; stop returns the rest.
    struct StreamAudio {
        samples: Vec<f32>,
        drained: usize,
        cursor: Mutex<usize>,
        stop_fails: bool,
        /// The stream failed: drain hands over the rest, as the capture worker does.
        failed: AtomicBool,
    }
    impl StreamAudio {
        /// A rising ramp, so each sample's value identifies its position.
        fn ramp(seconds: usize, drained_seconds: usize, stop_fails: bool) -> Self {
            let len = seconds * 16_000;
            Self {
                samples: (0..len)
                    .map(|i| 0.1 + 0.5 * i as f32 / len as f32)
                    .collect(),
                drained: drained_seconds * 16_000,
                cursor: Mutex::new(0),
                stop_fails,
                failed: AtomicBool::new(false),
            }
        }
        fn clip(samples: &[f32]) -> AudioClip {
            AudioClip {
                samples: samples.to_vec(),
                sample_rate: 16_000,
                channels: 1,
            }
        }
        /// Milliseconds into the recording where `clip` starts; the nearest sample
        /// for a clip kept as PCM16 (see `retry`).
        fn position(&self, clip: &AudioClip) -> usize {
            let first = clip.samples[0];
            let nearest = |s: &&f32| ((**s - first).abs() * 1e9) as u64;
            let sample = self.samples.iter().min_by_key(nearest).unwrap();
            self.samples.iter().position(|s| s == sample).unwrap() / 16
        }
    }
    #[async_trait]
    impl AudioCapture for StreamAudio {
        async fn start(&self) -> Result<()> {
            *self.cursor.lock().unwrap() = 0;
            Ok(())
        }
        async fn stop(&self) -> Result<AudioClip> {
            if self.stop_fails {
                bail!("microphone stream failed: device disconnected");
            }
            let mut cursor = self.cursor.lock().unwrap();
            let rest = Self::clip(&self.samples[*cursor..]);
            *cursor = self.samples.len();
            Ok(rest)
        }
        async fn cancel(&self) -> Result<()> {
            Ok(())
        }
        async fn drain(&self) -> Result<Option<AudioClip>> {
            let mut cursor = self.cursor.lock().unwrap();
            let end = if *cursor == 0 {
                self.drained
            } else if self.failed.load(Ordering::SeqCst) {
                self.samples.len()
            } else {
                return Ok(None);
            };
            let clip = Self::clip(&self.samples[*cursor..end]);
            *cursor = end;
            Ok((!clip.samples.is_empty()).then_some(clip))
        }
        fn level(&self) -> f32 {
            0.2
        }
    }
    /// Answers with the segment's start (ms); earlier segments answer later, the
    /// first `failures` calls fail and `hang` never answers; `words` replaces the
    /// answer. `hints` records each call's vocabulary.
    #[derive(Default)]
    struct SegmentStt {
        audio: Option<Arc<StreamAudio>>,
        clips: Mutex<Vec<AudioClip>>,
        hints: Mutex<Vec<Vec<String>>>,
        words: Option<fn(usize) -> &'static str>,
        failures: usize,
        failed_over: bool,
        calls: AtomicUsize,
        /// Segments (by start, ms) that fail on every attempt until `healed`.
        broken: Option<fn(usize) -> bool>,
        healed: AtomicBool,
        hang: bool,
        dropped: Arc<AtomicUsize>,
        /// Answers come from the backup provider.
        backup: AtomicBool,
    }
    struct DropCount(Arc<AtomicUsize>);
    impl Drop for DropCount {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[async_trait]
    impl SpeechToText for SegmentStt {
        fn name(&self) -> &str {
            "fixture"
        }
        async fn transcribe(
            &self,
            clip: AudioClip,
            options: TranscriptionOptions,
        ) -> Result<Transcript> {
            let _guard = DropCount(self.dropped.clone());
            let start = self.audio.as_ref().unwrap().position(&clip);
            self.clips.lock().unwrap().push(clip);
            self.hints.lock().unwrap().push(options.vocabulary);
            if self.hang {
                std::future::pending::<()>().await;
            }
            if self
                .broken
                .is_some_and(|broken| broken(start) && !self.healed.load(Ordering::SeqCst))
                || self.calls.fetch_add(1, Ordering::SeqCst) < self.failures
            {
                let error = anyhow::anyhow!("provider returned HTTP 500");
                return Err(if self.failed_over {
                    error.context(xflow_providers::FailoverExhausted)
                } else {
                    error
                });
            }
            tokio::time::sleep(Duration::from_millis(
                400u64.saturating_sub(start as u64 / 500),
            ))
            .await;
            Ok(Transcript {
                provider: self.backup.load(Ordering::SeqCst).then(|| "backup".into()),
                text: self
                    .words
                    .map_or_else(|| format!("at{start}"), |words| words(start).into()),
                language: Some("en".into()),
            })
        }
    }
    async fn wait_for(what: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !what() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    fn segment_fixture(
        audio: Arc<StreamAudio>,
        stt: Arc<SegmentStt>,
    ) -> (
        Engine,
        mpsc::Sender<Command>,
        mpsc::Receiver<Command>,
        broadcast::Receiver<Response>,
        Arc<RecordingDesktop>,
    ) {
        let desktop = Arc::new(RecordingDesktop::default());
        let store = Store::open(std::path::Path::new("unused"), false, 5).unwrap();
        let (mut engine, tx, rx, events, _) = fixture(Config::default(), desktop.clone(), store);
        engine.services.audio = audio;
        engine.services.stt = stt;
        (engine, tx, rx, events, desktop)
    }
    #[tokio::test]
    async fn long_dictations_upload_segments_while_recording_and_keep_every_sample_in_order() {
        // 5 minutes: 290 s drained while recording, 10 s returned by stop.
        let audio = Arc::new(StreamAudio::ramp(300, 290, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            failures: 1,
            ..SegmentStt::default()
        });
        let (engine, tx, rx, mut events, desktop) = segment_fixture(audio.clone(), stt.clone());
        let task = tokio::spawn(engine.run(rx));
        assert!(ask(&tx, Request::start()).await.ok);
        // Segments go out on the first drain, before the user stops.
        wait_for(|| stt.clips.lock().unwrap().len() >= 4).await;
        ask(&tx, Request::Stop).await;
        let done = success(&mut events).await;
        let mut clips = std::mem::take(&mut *stt.clips.lock().unwrap());
        let uploads = clips.len();
        clips.sort_by_key(|clip| audio.position(clip));
        clips.dedup_by_key(|clip| audio.position(clip));
        assert_eq!(
            uploads,
            clips.len() + 1,
            "the failed upload is retried once"
        );
        let rebuilt: Vec<f32> = clips.iter().flat_map(|c| c.samples.clone()).collect();
        assert_eq!(rebuilt, audio.samples, "segments tile the recording");
        assert!(clips.iter().all(|c| c.samples.len() <= 60 * 16_000));
        assert!(clips.len() >= 6, "{} segments", clips.len());
        let expected: Vec<String> = clips
            .iter()
            .map(|c| format!("at{}", audio.position(c)))
            .collect();
        // Earlier segments answered last, yet the text keeps recording order.
        assert_eq!(
            desktop.injections.lock().unwrap()[0].trim(),
            expected.join(" ")
        );
        assert_eq!(done.timings.unwrap().audio_ms, 300_000);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// `samples` at 16 kHz mono; stop returns what the first `drained_ms` didn't.
    fn stream(samples: Vec<f32>, drained_ms: usize) -> Arc<StreamAudio> {
        Arc::new(StreamAudio {
            samples,
            drained: drained_ms * 16,
            cursor: Mutex::new(0),
            stop_fails: false,
            failed: AtomicBool::new(false),
        })
    }
    #[tokio::test]
    async fn edge_silence_is_trimmed_before_upload_and_kept_audio_is_the_sent_clip() {
        // 1 s of silence, 2 s of tone, 1 s of silence.
        let samples: Vec<f32> = (0..64_000)
            .map(|i| match i {
                16_000..48_000 => 0.2 * (i as f32 * 0.07).sin(),
                _ => 0.0,
            })
            .collect();
        let audio = stream(samples.clone(), 0);
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            failures: 1,
            ..SegmentStt::default()
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let (mut engine, tx, rx, mut events, _) = segment_fixture(audio, stt.clone());
        engine.config.privacy.keep_failed_audio = true;
        engine.kept_dir = Some(dir.clone());
        let task = tokio::spawn(engine.run(rx));
        assert!(ask(&tx, Request::start()).await.ok);
        assert!(ask(&tx, Request::Stop).await.ok);
        let error = timeout_event_error(&mut events).await;
        // 400 ms of the silence stays on each side; the provider got nothing else.
        let sent = stt.clips.lock().unwrap()[0].clone();
        assert_eq!(sent.samples, samples[9_600..54_400]);
        // The kept clip (what retry and the backup see) is the one sent.
        assert_eq!(error.failed_audio.unwrap().audio_ms, 2_800);
        let kept = retry::audio(&dir, 0).unwrap();
        assert_eq!(kept.samples.len(), sent.samples.len());
        assert!(kept
            .samples
            .iter()
            .zip(&sent.samples)
            .all(|(k, s)| (k - s).abs() < 1e-4));
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_segmented_dictation_trims_only_its_outer_edges() {
        // 70 s: 2 s of silence, 66 s of speech, 2 s of silence; one segment is cut
        // while recording, the tail is sent at stop.
        let samples: Vec<f32> = (0..70 * 16_000)
            .map(|i| match i {
                32_000..1_088_000 => 0.1 + 0.5 * i as f32 / 1_120_000.0,
                _ => 0.001,
            })
            .collect();
        let audio = stream(samples.clone(), 65_000);
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            ..SegmentStt::default()
        });
        let (engine, tx, rx, mut events, _) = segment_fixture(audio, stt.clone());
        let task = tokio::spawn(engine.run(rx));
        assert!(ask(&tx, Request::start()).await.ok);
        wait_for(|| !stt.clips.lock().unwrap().is_empty()).await;
        ask(&tx, Request::Stop).await;
        let done = success(&mut events).await;
        let mut clips = std::mem::take(&mut *stt.clips.lock().unwrap());
        assert_eq!(clips.len(), 2);
        // Samples rise through the speech, so the first sample orders the clips.
        clips.sort_by(|a, b| a.samples[0].total_cmp(&b.samples[0]));
        let rebuilt: Vec<f32> = clips.iter().flat_map(|c| c.samples.clone()).collect();
        assert_eq!(
            rebuilt,
            samples[25_600..1_094_400],
            "no gap or overlap at the cut"
        );
        // The recording's length, not the upload's, is what was dictated.
        assert_eq!(done.timings.unwrap().audio_ms, 70_000);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn explicit_backup_does_not_retry_failed_over_segments_again() {
        let audio = Arc::new(StreamAudio::ramp(150, 140, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            failures: 1,
            failed_over: true,
            ..Default::default()
        });
        let (mut engine, tx, rx, mut events, desktop) = segment_fixture(audio.clone(), stt.clone());
        engine.config.stt.fallback = vec!["openai".into()];
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        wait_for(|| stt.clips.lock().unwrap().len() >= 2).await;
        ask(&tx, Request::Stop).await;
        let done = success(&mut events).await;
        assert!(done.message.unwrap().contains("Part of the dictation"));
        let mut starts: Vec<_> = stt
            .clips
            .lock()
            .unwrap()
            .iter()
            .map(|clip| audio.position(clip))
            .collect();
        let count = starts.len();
        starts.sort_unstable();
        starts.dedup();
        assert_eq!(
            starts.len(),
            count,
            "no segment is retried after explicit failover"
        );
        assert_eq!(desktop.injections.lock().unwrap().len(), 1);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn explicit_backup_preserves_retry_for_segments_that_never_failed_over() {
        let audio = Arc::new(StreamAudio::ramp(150, 140, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            failures: 1,
            ..Default::default()
        });
        let (mut engine, tx, rx, mut events, desktop) = segment_fixture(audio.clone(), stt.clone());
        engine.config.stt.fallback = vec!["openai".into()];
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        wait_for(|| stt.clips.lock().unwrap().len() >= 2).await;
        ask(&tx, Request::Stop).await;
        let done = success(&mut events).await;
        assert!(
            done.message.is_none(),
            "the failed segment was recovered: {:?}",
            done.message
        );
        let mut starts: Vec<_> = stt
            .clips
            .lock()
            .unwrap()
            .iter()
            .map(|clip| audio.position(clip))
            .collect();
        let count = starts.len();
        starts.sort_unstable();
        starts.dedup();
        assert_eq!(
            count,
            starts.len() + 1,
            "only the failed segment is retried"
        );
        assert_eq!(desktop.injections.lock().unwrap().len(), 1);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// Segments are joined before delivery: one paste of the joined text, and
    /// auto-learn watches that paste once rather than each segment.
    #[tokio::test]
    async fn a_segmented_dictation_is_pasted_and_observed_once() {
        let dir = tempfile::tempdir().unwrap();
        let audio = Arc::new(StreamAudio::ramp(150, 140, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            ..SegmentStt::default()
        });
        let (mut engine, tx, rx, mut events, desktop) = segment_fixture(audio.clone(), stt.clone());
        *desktop.field.lock().unwrap() = Some(String::new());
        engine.config_path = Some(dir.path().join("config.toml"));
        let task = tokio::spawn(engine.run(rx));
        assert!(ask(&tx, Request::start()).await.ok);
        wait_for(|| stt.clips.lock().unwrap().len() >= 2).await;
        ask(&tx, Request::Stop).await;
        let text = success(&mut events).await.text.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(stt.clips.lock().unwrap().len() >= 3, "segmented");
        assert_eq!(*desktop.injections.lock().unwrap(), [text.as_str()]);
        assert_eq!(
            desktop.field.lock().unwrap().as_deref(),
            Some(text.as_str())
        );
        assert_eq!(desktop.watches.load(Ordering::SeqCst), 1);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn cancel_aborts_segment_uploads_in_flight() {
        let audio = Arc::new(StreamAudio::ramp(200, 190, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            hang: true,
            ..SegmentStt::default()
        });
        let (engine, tx, rx, _events, desktop) = segment_fixture(audio, stt.clone());
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        wait_for(|| stt.clips.lock().unwrap().len() >= 3).await;
        let sent = stt.clips.lock().unwrap().len();
        assert_eq!(ask(&tx, Request::Cancel).await.state, State::Idle);
        wait_for(|| stt.dropped.load(Ordering::SeqCst) == sent).await;
        assert!(desktop.injections.lock().unwrap().is_empty());
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_lost_microphone_keeps_the_audio_captured_before_it() {
        // 140 s drained while recording; the last 10 s are still in the capture
        // buffer when the stream fails, and must not be lost with it.
        let audio = Arc::new(StreamAudio::ramp(150, 140, true));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            ..SegmentStt::default()
        });
        let (engine, tx, rx, mut events, desktop) = segment_fixture(audio.clone(), stt.clone());
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        wait_for(|| !stt.clips.lock().unwrap().is_empty()).await;
        audio.failed.store(true, Ordering::SeqCst);
        ask(&tx, Request::Stop).await;
        let done = success(&mut events).await;
        assert!(done
            .message
            .unwrap()
            .contains("device disconnected; transcribed the audio captured before it"));
        assert!(desktop.injections.lock().unwrap()[0].starts_with("at0 at"));
        assert_eq!(done.timings.unwrap().audio_ms, 150_000);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_segment_that_keeps_failing_loses_only_its_own_text() {
        // 5 minutes; segments start every ~30.1 s and the tail starts at 241 s.
        let tail: fn(usize) -> bool = |start| start >= 240_000;
        let middle: fn(usize) -> bool = |start| (60_000..61_000).contains(&start);
        for (broken, lost) in [(tail, "at241"), (middle, "at60")] {
            let audio = Arc::new(StreamAudio::ramp(300, 290, false));
            let stt = Arc::new(SegmentStt {
                audio: Some(audio.clone()),
                broken: Some(broken),
                ..SegmentStt::default()
            });
            let (engine, tx, rx, mut events, desktop) = segment_fixture(audio.clone(), stt.clone());
            let task = tokio::spawn(engine.run(rx));
            ask(&tx, Request::start()).await;
            wait_for(|| stt.clips.lock().unwrap().len() >= 4).await;
            ask(&tx, Request::Stop).await;
            let done = success(&mut events).await;
            let message = done.message.unwrap();
            assert!(
                message.contains("Part of the dictation could not be transcribed")
                    && message.contains("HTTP 500"),
                "{message}"
            );
            let text = desktop.injections.lock().unwrap()[0].clone();
            assert!(
                text.starts_with("at0 at30") && !text.contains(lost),
                "{text}"
            );
            let attempts = {
                let clips = stt.clips.lock().unwrap();
                clips.iter().filter(|c| broken(audio.position(c))).count()
            };
            assert_eq!(attempts, 2, "the failing segment is retried once");
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    type KeptFixture = (
        Engine,
        mpsc::Sender<Command>,
        mpsc::Receiver<Command>,
        broadcast::Receiver<Response>,
        Arc<RecordingDesktop>,
        Arc<SegmentStt>,
    );
    /// A segmented dictation whose provider is down; `kept` opts in to keeping it.
    fn kept_fixture(broken: fn(usize) -> bool, kept: Option<&std::path::Path>) -> KeptFixture {
        // 100 s: segments start at 0, ~30 and ~60 s.
        let audio = Arc::new(StreamAudio::ramp(100, 90, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            broken: Some(broken),
            words: Some(|start| ["one", "two", "three"][(start / 30_000).min(2)]),
            ..SegmentStt::default()
        });
        let (mut engine, tx, rx, events, desktop) = segment_fixture(audio, stt.clone());
        engine.config.privacy.keep_failed_audio = kept.is_some();
        engine.kept_dir = kept.map(Into::into);
        (engine, tx, rx, events, desktop, stt)
    }
    async fn dictate(tx: &mpsc::Sender<Command>, stt: &SegmentStt) {
        let sent = stt.clips.lock().unwrap().len();
        assert!(ask(tx, Request::start()).await.ok);
        wait_for(|| stt.clips.lock().unwrap().len() > sent).await;
        assert!(ask(tx, Request::Stop).await.ok);
    }
    fn retry(full: bool) -> Request {
        Request::Retry {
            full,
            delivery: Delivery::Inject,
            discard: false,
        }
    }
    #[tokio::test]
    async fn a_failed_dictation_is_kept_only_when_opted_in_and_survives_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let down: fn(usize) -> bool = |_| true;
        // Off by default: nothing reaches the disk.
        let (engine, tx, rx, mut events, _, stt) = kept_fixture(down, None);
        let task = tokio::spawn(engine.run(rx));
        dictate(&tx, &stt).await;
        let error = timeout_event_error(&mut events).await;
        assert!(!error.message.unwrap().contains("kept"));
        assert!(ask(&tx, Request::Status).await.failed_audio.is_none());
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
        assert!(!dir.exists());

        let (engine, tx, rx, mut events, _, stt) = kept_fixture(down, Some(&dir));
        let task = tokio::spawn(engine.run(rx));
        dictate(&tx, &stt).await;
        let error = timeout_event_error(&mut events).await;
        assert!(error
            .message
            .unwrap()
            .contains("HTTP 500; its audio is kept for 24 h: run xflow retry"));
        let kept = error.failed_audio.unwrap();
        assert_eq!((kept.audio_ms, kept.partial), (100_000, false));
        assert_eq!(kept.expires_at - kept.created_at, 24 * 3600);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();

        // A restarted daemon offers it; the retry sends only kept audio, in order.
        let (mut engine, tx, rx, mut events, desktop, stt) = kept_fixture(down, Some(&dir));
        engine.load_kept();
        assert_eq!(engine.status().failed_audio, Some(kept));
        stt.healed.store(true, Ordering::SeqCst);
        let task = tokio::spawn(engine.run(rx));
        assert_eq!(ask(&tx, retry(false)).await.state, State::Processing);
        let done = success(&mut events).await;
        assert_eq!(done.text.as_deref(), Some("one two three "));
        assert_eq!(*desktop.injections.lock().unwrap(), ["one two three "]);
        assert_eq!(stt.clips.lock().unwrap().len(), 3);
        assert!(done.failed_audio.is_none());
        assert!(!dir.exists(), "a successful retry deletes the audio");
        assert!(!ask(&tx, retry(false)).await.ok, "nothing left to retry");
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_discarded_dictation_keeps_no_failed_audio_and_pastes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        // The first segment fails, so its audio would be kept; the rest takes it all back.
        let audio = Arc::new(StreamAudio::ramp(100, 90, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            broken: Some(|start| start < 30_000),
            words: Some(|start| {
                ["Meet at noon.", "Bring notes.", "Scratch that."][(start / 30_000).min(2)]
            }),
            ..SegmentStt::default()
        });
        let (mut engine, tx, rx, mut events, desktop) = segment_fixture(audio, stt.clone());
        engine.config.privacy.keep_failed_audio = true;
        engine.kept_dir = Some(dir.clone());
        let task = tokio::spawn(engine.run(rx));
        dictate(&tx, &stt).await;
        let idle = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let r = events.recv().await.unwrap();
                assert!(
                    !matches!(r.state, State::Success | State::Error),
                    "{:?}",
                    r.message
                );
                if r.state == State::Idle && r.message.is_some() {
                    return r;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(idle.message.as_deref(), Some(DISCARDED));
        assert!(idle.failed_audio.is_none());
        assert!(desktop.injections.lock().unwrap().is_empty());
        assert!(ask(&tx, Request::Status).await.failed_audio.is_none());
        assert!(
            retry::load(&dir).ok().flatten().is_none(),
            "nothing kept to retry"
        );
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_partial_retry_delivers_only_the_lost_part_unless_full() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let (engine, tx, rx, mut events, desktop, stt) =
            kept_fixture(|start| (30_000..31_000).contains(&start), Some(&dir));
        let task = tokio::spawn(engine.run(rx));
        dictate(&tx, &stt).await;
        let done = success(&mut events).await;
        assert!(done.message.unwrap().contains("its audio is kept for 24 h"));
        assert!(done.failed_audio.unwrap().partial);
        let kept = std::fs::read_to_string(dir.join("current/kept.json")).unwrap();
        assert!(kept.contains("one") && !kept.contains("two"), "{kept}");
        assert!(dir.join("current/1.wav").exists() && !dir.join("current/0.wav").exists());

        stt.healed.store(true, Ordering::SeqCst);
        ask(&tx, retry(false)).await;
        success(&mut events).await;
        assert_eq!(
            *desktop.injections.lock().unwrap(),
            ["one three ", "two "],
            "the delivered parts are not pasted again"
        );
        assert!(!dir.exists());

        // A retry that fails again keeps the dictation and its original expiry.
        stt.healed.store(false, Ordering::SeqCst);
        dictate(&tx, &stt).await;
        let first = success(&mut events).await.failed_audio.unwrap();
        ask(&tx, retry(false)).await;
        let again = timeout_event_error(&mut events).await;
        assert_eq!(again.failed_audio, Some(first));
        stt.healed.store(true, Ordering::SeqCst);
        ask(&tx, retry(true)).await;
        success(&mut events).await;
        assert_eq!(
            desktop.injections.lock().unwrap().last().unwrap(),
            "one two three "
        );
        assert!(!dir.exists());
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_cancel_or_clear_during_a_slow_save_leaves_nothing_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let left =
            |dir: &std::path::Path| std::fs::read_dir(dir).map_or(0, |entries| entries.count());
        for (point, request) in [
            ("staged", Request::Cancel),
            ("committed", Request::Cancel),
            ("staged", Request::ClearHistory),
            ("committed", Request::ClearHistory),
        ] {
            let (mut engine, tx, rx, _, _, stt) = kept_fixture(|_| true, Some(&dir));
            let (entered, inside) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let released = Mutex::new(released);
            engine.keep_hook = Some(Arc::new(move |at: &str| {
                if at == point {
                    entered.send(()).unwrap();
                    released.lock().unwrap().recv().unwrap();
                }
            }));
            let task = tokio::spawn(engine.run(rx));
            dictate(&tx, &stt).await;
            wait_for(|| inside.try_recv().is_ok()).await;
            assert!(ask(&tx, request.clone()).await.ok, "{point} {request:?}");
            release.send(()).unwrap();
            wait_for(|| left(&dir) == 0).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(left(&dir), 0, "{point} {request:?}");
            assert!(ask(&tx, Request::Status).await.failed_audio.is_none());
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn a_retry_goes_through_failover_and_credits_only_the_provider_that_answered() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let middle: fn(usize) -> bool = |start| (30_000..31_000).contains(&start);
        let (mut engine, tx, rx, mut events, desktop, stt) = kept_fixture(middle, Some(&dir));
        // The whole failover chain already failed: no second attempt at stop.
        let stt = Arc::new(SegmentStt {
            audio: stt.audio.clone(),
            broken: Some(middle),
            failed_over: true,
            words: stt.words,
            ..SegmentStt::default()
        });
        engine.services.stt = stt.clone();
        let task = tokio::spawn(engine.run(rx));
        dictate(&tx, &stt).await;
        assert!(success(&mut events).await.failed_audio.unwrap().partial);
        let audio = stt.audio.as_ref().unwrap();
        let attempts = || {
            let clips = stt.clips.lock().unwrap();
            clips.iter().filter(|c| middle(audio.position(c))).count()
        };
        assert_eq!(attempts(), 1);
        assert!(std::fs::read_to_string(dir.join("current/kept.json"))
            .unwrap()
            .contains(r#""provider":"fixture""#));
        // The retry uses the same failover service; the backup answers.
        stt.healed.store(true, Ordering::SeqCst);
        stt.backup.store(true, Ordering::SeqCst);
        ask(&tx, retry(false)).await;
        let done = success(&mut events).await;
        assert_eq!(attempts(), 2);
        assert_eq!(desktop.injections.lock().unwrap().last().unwrap(), "two ");
        assert_eq!(done.provider.as_deref(), Some("backup"));
        assert!(done
            .message
            .unwrap()
            .contains("Transcribed by backup (fixture didn't answer)"));
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_retry_that_fails_after_transcribing_keeps_the_audio() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let (mut engine, tx, rx, mut events, desktop, stt) = kept_fixture(|_| true, Some(&dir));
        // Three segments of 12.5 KB: joined, past the 32 KiB transcript limit.
        let stt = Arc::new(SegmentStt {
            audio: stt.audio.clone(),
            broken: Some(|_| true),
            words: Some(|_| {
                static LONG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                LONG.get_or_init(|| "word ".repeat(2500))
            }),
            ..SegmentStt::default()
        });
        engine.services.stt = stt.clone();
        let task = tokio::spawn(engine.run(rx));
        dictate(&tx, &stt).await;
        let kept = timeout_event_error(&mut events).await.failed_audio;
        assert!(kept.is_some());
        stt.healed.store(true, Ordering::SeqCst);
        ask(&tx, retry(false)).await;
        let error = timeout_event_error(&mut events).await;
        assert!(error.message.unwrap().contains("32 KiB"));
        assert_eq!(error.failed_audio, kept);
        assert!(dir.join("current/0.wav").exists());
        assert!(desktop.injections.lock().unwrap().is_empty());
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn discard_clear_cancel_expiry_and_opting_out_delete_the_kept_audio() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("failed");
        let keep = |created_at| {
            let clip = AudioClip {
                samples: vec![0.2; 16_000],
                sample_rate: 16_000,
                channels: 1,
            };
            retry::save(
                &dir,
                created_at,
                vec![Part::Audio { low_energy: false }],
                vec![clip],
            )
            .unwrap();
        };
        let (mut engine, tx, rx, mut events, _, stt) = kept_fixture(|_| false, Some(&dir));
        keep(retry::now());
        engine.load_kept();
        assert!(engine.kept.is_some());
        // Tokio's paused clock does not advance the persisted wall-clock timestamp.
        keep(retry::now() - retry::TTL_SECS);
        engine.kept.as_mut().unwrap().expires_at = retry::now() - 1;
        let task = tokio::spawn(engine.run(rx));
        // Expiry runs on a timer; no request is needed.
        tokio::time::timeout(Duration::from_secs(5), async {
            while events.recv().await.unwrap().failed_audio.is_some() {}
        })
        .await
        .unwrap();
        assert!(!dir.exists(), "expired");
        let removes = [
            Request::Retry {
                full: false,
                delivery: Delivery::Inject,
                discard: true,
            },
            Request::ClearHistory,
        ];
        for request in removes {
            keep(retry::now());
            assert!(dir.exists());
            assert!(ask(&tx, request).await.ok);
            assert!(!dir.exists());
        }
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();

        // Shutting down mid-retry keeps it; canceling the retry discards it.
        keep(retry::now());
        for cancel in [false, true] {
            let (mut engine, tx, rx, _, desktop, _) = kept_fixture(|_| false, Some(&dir));
            engine.services.stt = Arc::new(SegmentStt {
                hang: true,
                audio: stt.audio.clone(),
                ..SegmentStt::default()
            });
            engine.load_kept();
            let task = tokio::spawn(engine.run(rx));
            assert_eq!(ask(&tx, retry(false)).await.state, State::Processing);
            if cancel {
                assert_eq!(ask(&tx, Request::Cancel).await.state, State::Idle);
            }
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
            assert_eq!(dir.exists(), !cancel);
            assert!(desktop.injections.lock().unwrap().is_empty());
        }

        // Turning the setting off deletes it at the next start.
        let (mut engine, ..) = kept_fixture(|_| false, Some(&dir));
        keep(retry::now());
        engine.config.privacy.keep_failed_audio = false;
        engine.load_kept();
        assert!(engine.kept.is_none() && !dir.exists());
    }
    #[tokio::test]
    async fn every_segment_gets_dictionary_hints_and_the_joined_text_snaps_once() {
        // 70 s: one segment cut while recording, the tail sent at stop. The term is
        // split across the boundary, so only the joined text can snap it.
        let audio = Arc::new(StreamAudio::ramp(70, 60, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            words: Some(|start| {
                if start == 0 {
                    "We deploy on next"
                } else {
                    "js today."
                }
            }),
            ..SegmentStt::default()
        });
        let (mut engine, tx, rx, mut events, desktop) = segment_fixture(audio, stt.clone());
        engine.config.dictionary.words = vec!["Next.js".into()];
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        wait_for(|| !stt.clips.lock().unwrap().is_empty()).await;
        ask(&tx, Request::Stop).await;
        success(&mut events).await;
        let hints = stt.hints.lock().unwrap().clone();
        assert_eq!(hints.len(), 2, "two segments, no retries");
        assert!(
            hints.iter().all(|h| h == &["Next.js".to_string()]),
            "{hints:?}"
        );
        assert_eq!(
            desktop.injections.lock().unwrap()[0].trim(),
            "We deploy on Next.js today."
        );
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_lost_segment_warning_survives_a_cleanup_failure() {
        let audio = Arc::new(StreamAudio::ramp(300, 290, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            broken: Some(|start| start >= 240_000),
            ..SegmentStt::default()
        });
        let (mut engine, tx, rx, mut events, desktop) = segment_fixture(audio, stt.clone());
        // Polished: never skipped, so the cleanup call really fails.
        engine.config.cleanup.mode = CleanupMode::Polished;
        engine.services.transformer = Some(Arc::new(FailingCleanup));
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        wait_for(|| stt.clips.lock().unwrap().len() >= 4).await;
        ask(&tx, Request::Stop).await;
        let message = success(&mut events).await.message.unwrap();
        assert!(
            message.contains("Part of the dictation could not be transcribed"),
            "{message}"
        );
        assert!(message.contains("Cleanup failed"), "{message}");
        assert!(!desktop.injections.lock().unwrap()[0].contains("at241"));
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_command_that_lost_a_segment_leaves_the_selection_untouched() {
        let audio = Arc::new(StreamAudio::ramp(300, 290, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            broken: Some(|start| start >= 240_000),
            ..SegmentStt::default()
        });
        let (mut engine, tx, rx, mut events, desktop) = segment_fixture(audio, stt.clone());
        let transformer = Arc::new(RecordingTransformer::default());
        engine.services.transformer = Some(transformer.clone());
        let task = tokio::spawn(engine.run(rx));
        let start = Request::Start {
            mode: Mode::Command,
            context: None,
            t0_us: None,
            delivery: Delivery::Inject,
            hold: Default::default(),
        };
        assert!(ask(&tx, start).await.ok);
        wait_for(|| stt.clips.lock().unwrap().len() >= 4).await;
        ask(&tx, Request::Stop).await;
        let failed = hotkey_state(&mut events, State::Error).await;
        assert!(failed
            .message
            .unwrap()
            .contains("part of the command could not be transcribed"));
        assert!(transformer.0.lock().unwrap().is_empty());
        assert!(desktop.injections.lock().unwrap().is_empty());
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_dictation_with_nothing_transcribed_still_fails() {
        let audio = Arc::new(StreamAudio::ramp(100, 90, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            broken: Some(|_| true),
            ..SegmentStt::default()
        });
        let (engine, tx, rx, mut events, desktop) = segment_fixture(audio, stt);
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        ask(&tx, Request::Stop).await;
        let failed = hotkey_state(&mut events, State::Error).await;
        assert!(failed.message.unwrap().contains("HTTP 500"));
        assert!(desktop.injections.lock().unwrap().is_empty());
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_short_dictation_fails_fast_without_a_retry() {
        let audio = Arc::new(StreamAudio::ramp(20, 20, false));
        let stt = Arc::new(SegmentStt {
            audio: Some(audio.clone()),
            broken: Some(|_| true),
            ..SegmentStt::default()
        });
        let (engine, tx, rx, mut events, _) = segment_fixture(audio, stt.clone());
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        hotkey_state(&mut events, State::Error).await;
        assert_eq!(stt.clips.lock().unwrap().len(), 1);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[test]
    fn segments_join_without_spaces_in_languages_written_without_them() {
        let parts = ["今日は".to_string(), " 晴れです。".into(), "".into()];
        assert_eq!(join_parts(&parts, Some("ja")), "今日は晴れです。");
        assert_eq!(join_parts(&parts, Some("Chinese")), "今日は晴れです。");
        let parts = ["Ship it.".to_string(), "  Then rest. ".into()];
        assert_eq!(join_parts(&parts, Some("en")), "Ship it. Then rest.");
        assert_eq!(join_parts(&parts, None), "Ship it. Then rest.");
    }
    /// Native-rate audio returned whole at stop (no drain), like #79's debug hook.
    struct WholeClip(AudioClip);
    #[async_trait]
    impl AudioCapture for WholeClip {
        async fn start(&self) -> Result<()> {
            Ok(())
        }
        async fn stop(&self) -> Result<AudioClip> {
            Ok(self.0.clone())
        }
        async fn cancel(&self) -> Result<()> {
            Ok(())
        }
        fn level(&self) -> f32 {
            0.2
        }
    }
    #[derive(Default)]
    struct FormatStt(Mutex<Vec<(u32, u16, usize)>>);
    #[async_trait]
    impl SpeechToText for FormatStt {
        fn name(&self) -> &str {
            "fixture"
        }
        async fn transcribe(&self, clip: AudioClip, _: TranscriptionOptions) -> Result<Transcript> {
            self.0
                .lock()
                .unwrap()
                .push((clip.sample_rate, clip.channels, clip.samples.len()));
            Ok(Transcript {
                provider: None,
                text: "part".into(),
                language: None,
            })
        }
    }
    #[tokio::test]
    async fn segments_are_held_and_sent_at_16_khz_mono() {
        // 62 s of 32 kHz stereo tone: two segments, downmixed and resampled.
        let samples = (0..32_000 * 62)
            .flat_map(|i| {
                let s = 0.3 * (i as f32 * 0.05).sin();
                [s, s]
            })
            .collect();
        let desktop = Arc::new(RecordingDesktop::default());
        let store = Store::open(std::path::Path::new("unused"), false, 5).unwrap();
        let (mut engine, tx, rx, mut events, _) =
            fixture(Config::default(), desktop.clone(), store);
        let stt = Arc::new(FormatStt::default());
        engine.services.audio = Arc::new(WholeClip(AudioClip {
            samples,
            sample_rate: 32_000,
            channels: 2,
        }));
        engine.services.stt = stt.clone();
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        // Debug-build resampling is slow next to the parallel suite.
        tokio::time::timeout(Duration::from_secs(30), async {
            while events.recv().await.unwrap().state != State::Success {}
        })
        .await
        .unwrap();
        let clips = stt.0.lock().unwrap().clone();
        assert_eq!(clips.len(), 2);
        assert!(clips
            .iter()
            .all(|&(rate, channels, _)| (rate, channels) == (16_000, 1)));
        let frames: usize = clips.iter().map(|&(.., len)| len).sum();
        assert!(frames.abs_diff(16_000 * 62) <= 2, "{frames}");
        assert_eq!(desktop.injections.lock().unwrap()[0].trim(), "part part");
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }

    #[tokio::test]
    async fn corrected_paste_and_history_edit_learn_dictionary_words() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (notify, field_events) = mpsc::unbounded_channel();
        let desktop = Arc::new(RecordingDesktop {
            field: Arc::new(Mutex::new(Some("Notes: ".into()))),
            field_events: Mutex::new(Some(field_events)),
            ..RecordingDesktop::default()
        });
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        let (mut engine, tx, rx, mut events, _) =
            fixture(Config::default(), desktop.clone(), store);
        engine.config_path = Some(path.clone());
        #[cfg(debug_assertions)]
        let keys = {
            let wav = dir.path().join("dictation.wav");
            std::fs::write(
                &wav,
                xflow_providers::encode_wav(&AudioClip {
                    samples: vec![0.2; 16_000],
                    sample_rate: 16_000,
                    channels: 1,
                })
                .unwrap(),
            )
            .unwrap();
            engine.services.audio = Arc::new(test_audio::WavCapture::new(wav));
            let (keys, key_events) = mpsc::channel(8);
            engine.hotkeys = Some(Box::new(FakeHotkeys(key_events)));
            keys
        };
        let task = tokio::spawn(engine.run(rx));
        #[cfg(debug_assertions)]
        {
            keys.send(HotkeyAction::Start).await.unwrap();
            hotkey_state(&mut events, State::Listening).await;
            keys.send(HotkeyAction::Stop).await.unwrap();
        }
        #[cfg(not(debug_assertions))]
        {
            ask(&tx, Request::start()).await;
            ask(&tx, Request::Stop).await;
        }
        let id = success(&mut events).await.entry.unwrap().id;
        // After the baseline read, the user fixes a word and edits elsewhere.
        tokio::time::sleep(Duration::from_millis(400)).await;
        *desktop.field.lock().unwrap() = Some("My Notes: Postgres is ready ".into());
        notify.send(FieldEvent::Changed).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        ask(&tx, Request::start()).await;
        assert_eq!(
            learned_message(&mut events).await,
            "Added 'Postgres' to your dictionary"
        );
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
    /// Prepare a focused, unsent draft containing the original below, then
    /// correct and clear it after `ready` appears in XFLOW_NATIVE_ACCEPTANCE_DIR.
    /// Dictionary writes stay temporary; no microphone or provider is used.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "reads a prepared native draft; run explicitly with XFLOW_NATIVE_ACCEPTANCE_DIR and XFLOW_NATIVE_ACCEPTANCE_APP"]
    async fn native_field_correction_reaches_the_dictionary() {
        let report = PathBuf::from(
            std::env::var_os("XFLOW_NATIVE_ACCEPTANCE_DIR").expect("acceptance directory required"),
        );
        let app = std::env::var("XFLOW_NATIVE_ACCEPTANCE_APP").expect("target app required");
        let original = "Rimpas or Kar, call me";
        let desktop: Arc<dyn Desktop> =
            Arc::new(xflow_platform::NativeDesktop::new(&Default::default()));
        let target = desktop.context().await.unwrap();
        assert_eq!(target.app_id.as_deref(), Some(app.as_str()));
        let mut field = desktop
            .watch_field(&target)
            .await
            .expect("readable test field");
        assert_eq!(field.read().await.unwrap().value.trim(), original);
        drop(field);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let before = b"# native acceptance fixture\n";
        std::fs::write(&path, before).unwrap();
        let (finalize, finalized) = oneshot::channel();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(8)).await;
            let _ = finalize.send(());
        });
        let (completion, mut received) = mpsc::channel(8);
        let learning = Arc::new(std::sync::Mutex::new(Learning::default()));
        std::fs::write(report.join("ready"), b"ready").unwrap();
        tokio::time::timeout(
            Duration::from_secs(20),
            observe(
                desktop,
                target,
                original.into(),
                finalized,
                path.clone(),
                completion,
                Arc::new(AtomicU64::new(0)),
                learning.clone(),
            ),
        )
        .await
        .expect("correct and clear the prepared draft within 20 seconds");
        std::fs::remove_file(report.join("ready")).unwrap();
        {
            let measured = lock(&learning);
            println!(
                "native learning: outcomes={:?} reads={} unread_max={} late={} merged={} editing={}",
                measured.outcomes, measured.reads, measured.unread_max,
                measured.late, measured.merged, measured.editing,
            );
            assert_eq!(measured.outcomes.get("learned"), Some(&1));
        }
        let Some(Completion::Learned {
            dictionary,
            change: Some(change),
            ..
        }) = received.recv().await
        else {
            panic!("missing learned dictionary and undo");
        };
        assert_eq!(dictionary.words, ["Rimpa Sarkar"]);
        assert!(change.notice);
        assert_eq!(change.words, ["Rimpa Sarkar"]);
        assert_ne!(std::fs::read(&path).unwrap(), before);
        let _guard = crate::config_edit::lock(&path).unwrap();
        change.undo.restore(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    async fn learned_message(events: &mut broadcast::Receiver<Response>) -> String {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(message) = events.recv().await.unwrap().message {
                    if message.starts_with("Added") {
                        return message;
                    }
                }
            }
        })
        .await
        .expect("nothing learned")
    }
    /// A chat field: the user fixes a word, presses Enter, then types a similar
    /// next message. The settled edit is learned at once, the next message is
    /// never read, and the next dictation snaps to the learned word (#64).
    #[tokio::test]
    async fn correction_sent_with_enter_learns_from_the_last_good_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (notify, field_events) = mpsc::unbounded_channel();
        let desktop = Arc::new(RecordingDesktop {
            field: Arc::new(Mutex::new(Some(String::new()))),
            field_events: Mutex::new(Some(field_events)),
            ..RecordingDesktop::default()
        });
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        let (mut engine, tx, rx, mut events, _) =
            fixture(Config::default(), desktop.clone(), store);
        engine.config_path = Some(path.clone());
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        assert_eq!(
            success(&mut events).await.text.as_deref(),
            Some("post gres is ready ")
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
        let edit = |text: &str| {
            *desktop.field.lock().unwrap() = Some(text.into());
            // Closed once the observation has ended.
            let _ = notify.send(FieldEvent::Changed);
        };
        edit("Postgres is ready ");
        tokio::time::sleep(Duration::from_millis(200)).await;
        edit("");
        tokio::time::sleep(Duration::from_millis(300)).await;
        edit("post Gres is ready now?");
        assert_eq!(
            learned_message(&mut events).await,
            "Added 'Postgres' to your dictionary"
        );
        let dictionary = Config::load(&path).unwrap().dictionary;
        assert_eq!(dictionary.words, ["Postgres"]);
        assert!(dictionary.replacements.is_empty());
        // The same misrecognition now comes out as the learned spelling.
        *desktop.field.lock().unwrap() = None;
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        assert_eq!(
            success(&mut events).await.text.as_deref(),
            Some("Postgres is ready ")
        );
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// Drives `watch_edit` over a field holding `pasted`: each step sets the
    /// field (and notifies a change) after its delay. Returns the outcome.
    async fn watch_script(
        pasted: &str,
        steps: &[(u64, &str)],
    ) -> std::result::Result<(String, String), Option<&'static str>> {
        let (notify, field_events) = mpsc::unbounded_channel();
        let desktop = Arc::new(RecordingDesktop {
            field: Arc::new(Mutex::new(Some(pasted.into()))),
            field_events: Mutex::new(Some(field_events)),
            ..RecordingDesktop::default()
        });
        let (finalize_tx, finalize) = oneshot::channel();
        let field = desktop.field.clone();
        let steps: Vec<(u64, String)> = steps.iter().map(|(ms, t)| (*ms, t.to_string())).collect();
        let script = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            for (ms, text) in steps {
                // 0 ms: no yield, so the observer sees both changes before reading.
                if ms > 0 {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
                *field.lock().unwrap() = Some(text);
                let _ = notify.send(FieldEvent::Changed);
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = finalize_tx.send(());
        });
        let result = watch_edit(&*desktop, &AppContext::default(), pasted, finalize).await;
        script.abort();
        result
    }
    #[tokio::test]
    async fn sent_dictation_ends_observation_before_the_next_message() {
        // Sent unedited, then a similar message is typed: nothing to learn.
        assert_eq!(
            watch_script(
                "post gres is ready ",
                &[(0, ""), (250, "Postgres is ready for staging")]
            )
            .await,
            Err(Some("anchors-lost"))
        );
        // A settled fix, then Enter: learned, and the next message is never read.
        assert_eq!(
            watch_script(
                "post gres is ready ",
                &[
                    (0, "Postgres is ready "),
                    (200, ""),
                    (250, "post Gres is ready now")
                ]
            )
            .await,
            Ok(("post gres is ready ".into(), "Postgres is ready ".into()))
        );
    }
    /// Notifications carry no text: a send and a new message queued before one
    /// read leave the field's history unknown, so nothing is learned (review R1).
    /// Changes still queued when the watch is finalized count before the final
    /// read: a send counted, then the next message and the next dictation's
    /// start queued together, learns nothing (re-review of 6897db4).
    #[tokio::test(start_paused = true)]
    async fn changes_queued_behind_finalize_learn_nothing() {
        for (queued, field_after) in [
            // Send counted; the next message queued with finalize.
            (
                &["Postgres is ready for staging"][..],
                Some("Postgres is ready for staging"),
            ),
            // "Postgre" confirmed; the last letter and an unreadable field queued.
            (&["Postgres is ready "][..], None),
        ] {
            let pasted = "post gres is ready ";
            let (notify, field_events) = mpsc::unbounded_channel();
            let desktop = Arc::new(RecordingDesktop {
                field: Arc::new(Mutex::new(Some(pasted.into()))),
                field_events: Mutex::new(Some(field_events)),
                ..RecordingDesktop::default()
            });
            let (finish, finalize) = oneshot::channel();
            let target = AppContext::default();
            let watch = watch_edit(&*desktop, &target, pasted, finalize);
            tokio::pin!(watch);
            let set = |text: Option<&str>| {
                *desktop.field.lock().unwrap() = text.map(str::to_owned);
                let _ = notify.send(FieldEvent::Changed);
            };
            assert!(futures_util::poll!(&mut watch).is_pending());
            tokio::time::advance(Duration::from_millis(120)).await;
            assert!(futures_util::poll!(&mut watch).is_pending());
            assert_eq!(desktop.watches.load(Ordering::SeqCst), 1);
            tokio::time::advance(FIELD_READ_GAP).await;
            // Poll each change explicitly; hold the next read inside its gap.
            if field_after.is_some() {
                set(Some("post gres is ready now "));
                assert!(futures_util::poll!(&mut watch).is_pending());
                tokio::time::advance(Duration::from_millis(5)).await;
                set(Some(""));
            } else {
                set(Some("Postgre is ready "));
                assert!(futures_util::poll!(&mut watch).is_pending());
                tokio::time::advance(FIELD_SETTLE).await;
                assert!(futures_util::poll!(&mut watch).is_pending());
                set(Some("Postgre is ready "));
            }
            assert!(futures_util::poll!(&mut watch).is_pending());
            // Hold the watch so finalize wins with an already-counted and a queued change.
            for text in queued {
                set(Some(text));
            }
            if field_after.is_none() {
                set(None);
            }
            let _ = finish.send(());
            assert_eq!(watch.await, Err(Some("unsettled")), "{queued:?}");
        }
    }
    #[tokio::test]
    async fn queued_send_and_next_message_learn_nothing() {
        assert_eq!(
            watch_script(
                "post gres is ready ",
                &[(0, ""), (0, "Postgres is ready for staging")]
            )
            .await,
            Err(Some("unsettled"))
        );
    }
    /// "Postgre" confirmed by a pause, then the last letter and Enter queued
    /// before a read: the earlier confirmation no longer holds (review R2).
    #[tokio::test]
    async fn unread_edits_void_an_earlier_confirmed_spelling() {
        assert_eq!(
            watch_script(
                "post gres is ready ",
                &[
                    (0, "Postgre is ready "),
                    (200, "Postgres is ready "),
                    (0, "")
                ]
            )
            .await,
            Err(Some("unsettled"))
        );
    }
    /// A dictation too long to ever be compared is not watched or read.
    #[tokio::test]
    async fn a_dictation_too_long_to_compare_is_not_watched() {
        let pasted = "word ".repeat(xflow_core::learn::MAX_DIFF_WORDS + 1);
        let desktop = RecordingDesktop {
            field: Arc::new(Mutex::new(Some(pasted.clone()))),
            ..RecordingDesktop::default()
        };
        let (_finalize, finalize) = oneshot::channel();
        assert_eq!(
            watch_edit(&desktop, &AppContext::default(), &pasted, finalize).await,
            Err(Some("too-long"))
        );
        assert_eq!(desktop.watches.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn fast_enter_learns_the_final_spelling_or_nothing() {
        // The last letter and Enter land inside one settle interval, but the
        // final spelling was read before the clear: it is learned, not "Postgre".
        assert_eq!(
            watch_script(
                "post gres is ready ",
                &[
                    (0, "Postgre is ready "),
                    (40, "Postgres is ready "),
                    (30, "")
                ]
            )
            .await,
            Ok(("post gres is ready ".into(), "Postgres is ready ".into()))
        );
        // The last letter and Enter both land before the next read: the final
        // spelling was never seen, so neither it nor "Postgre" is learned.
        assert_eq!(
            watch_script(
                "post gres is ready ",
                &[
                    (0, "Postgre is ready "),
                    (40, "Postgres is ready "),
                    (0, "")
                ]
            )
            .await,
            Err(Some("unsettled"))
        );
        // Typing that pauses before the field is left is final.
        assert_eq!(
            watch_script(
                "post gres is ready ",
                &[(0, "Postgre is ready "), (40, "Postgres is ready ")]
            )
            .await,
            Ok(("post gres is ready ".into(), "Postgres is ready ".into()))
        );
    }
    /// An app that announces its edits: the edit is read when focus leaves
    /// the field, not after 60 s or at the next dictation.
    #[tokio::test]
    async fn correction_is_learned_when_focus_leaves_the_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (notify, field_events) = mpsc::unbounded_channel();
        let desktop = Arc::new(RecordingDesktop {
            field: Arc::new(Mutex::new(Some("Notes: ".into()))),
            field_events: Mutex::new(Some(field_events)),
            ..RecordingDesktop::default()
        });
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        let (mut engine, tx, rx, mut events, _) =
            fixture(Config::default(), desktop.clone(), store);
        engine.config_path = Some(path.clone());
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        success(&mut events).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        *desktop.field.lock().unwrap() = Some("Notes: Postgres is ready".into());
        notify.send(FieldEvent::Changed).unwrap();
        // A focus blip that comes back within the grace keeps watching.
        notify.send(FieldEvent::Focus).unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        desktop.field_left.store(true, Ordering::SeqCst);
        notify.send(FieldEvent::Focus).unwrap();
        assert_eq!(
            learned_message(&mut events).await,
            "Added 'Postgres' to your dictionary"
        );
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// Asserts that nothing is learned within 1.5 s and the config has no word.
    async fn learns_nothing(events: &mut broadcast::Receiver<Response>, path: &std::path::Path) {
        let _ = tokio::time::timeout(Duration::from_millis(1500), async {
            loop {
                if let Some(message) = events.recv().await.unwrap().message {
                    assert!(!message.starts_with("Added"), "{message}");
                }
            }
        })
        .await;
        let config = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            !config.contains("Postgres") && !config.contains("Posters"),
            "{config}"
        );
    }
    /// An app that never announces edits (or whose observer could not start):
    /// a dictation sent with Enter is followed by the next message, which must
    /// never be read as a correction, whatever ends the watch (independent
    /// test B2). Only announced edits are learned.
    #[tokio::test]
    async fn unannounced_edits_are_never_learned() {
        for (prefix, edits, finish) in [
            ("", &["", "Posters is ready now"][..], "next-dictation"),
            ("", &["", "Postgres is ready for staging"][..], "focus"),
            ("Notes: ", &["Notes: Postgres is ready "][..], "focus"),
            (
                "Notes: ",
                &["Notes: Postgres is ready "][..],
                "next-dictation",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            // Focus notifications only, as from an app without value changes.
            let (notify, field_events) = mpsc::unbounded_channel();
            let desktop = Arc::new(RecordingDesktop {
                field: Arc::new(Mutex::new(Some(prefix.into()))),
                field_events: Mutex::new(Some(field_events)),
                ..RecordingDesktop::default()
            });
            let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
            let (mut engine, tx, rx, mut events, _) =
                fixture(Config::default(), desktop.clone(), store);
            engine.config_path = Some(path.clone());
            let task = tokio::spawn(engine.run(rx));
            ask(&tx, Request::start()).await;
            ask(&tx, Request::Stop).await;
            success(&mut events).await;
            tokio::time::sleep(Duration::from_millis(400)).await;
            for edit in edits {
                *desktop.field.lock().unwrap() = Some((*edit).into());
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            if finish == "focus" {
                desktop.field_left.store(true, Ordering::SeqCst);
                notify.send(FieldEvent::Focus).unwrap();
            } else {
                *desktop.field.lock().unwrap() = None;
                ask(&tx, Request::start()).await;
            }
            learns_nothing(&mut events, &path).await;
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    /// A history edit with punctuation and a counter next to the fix learns the
    /// word alone, never "Postgres  2" (independent test B1).
    #[tokio::test]
    async fn history_edit_learns_only_the_well_formed_word() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let desktop = Arc::new(RecordingDesktop::default());
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        let (mut engine, tx, rx, mut events, _) =
            fixture(Config::default(), desktop.clone(), store);
        engine.config_path = Some(path.clone());
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        let id = success(&mut events).await.entry.unwrap().id;
        let edited = ask(
            &tx,
            Request::HistoryUpdate {
                id,
                text: "Postgres -> 2 is ready".into(),
            },
        )
        .await;
        assert_eq!(
            edited.message.as_deref(),
            Some("Added 'Postgres' to your dictionary")
        );
        let dictionary = Config::load(&path).unwrap().dictionary;
        assert_eq!(dictionary.words, ["Postgres"]);
        assert!(dictionary.replacements.is_empty());
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// `watch_script` with the notifications of each step spelled out: none (a
    /// change the app hasn't announced yet), several, or a folded one.
    async fn watch_events(
        pasted: &str,
        steps: &[(u64, &str, &[FieldEvent])],
    ) -> std::result::Result<(String, String), Option<&'static str>> {
        let (notify, field_events) = mpsc::unbounded_channel();
        let desktop = Arc::new(RecordingDesktop {
            field: Arc::new(Mutex::new(Some(pasted.into()))),
            field_events: Mutex::new(Some(field_events)),
            ..RecordingDesktop::default()
        });
        let (finalize_tx, finalize) = oneshot::channel();
        let field = desktop.field.clone();
        let steps: Vec<(u64, String, Vec<FieldEvent>)> = steps
            .iter()
            .map(|(ms, text, events)| (*ms, text.to_string(), events.to_vec()))
            .collect();
        let script = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            for (ms, text, events) in steps {
                if ms > 0 {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
                *field.lock().unwrap() = Some(text);
                for event in events {
                    let _ = notify.send(event);
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = finalize_tx.send(());
        });
        let result = watch_edit(&*desktop, &AppContext::default(), pasted, finalize).await;
        script.abort();
        result
    }
    const CHANGED: &[FieldEvent] = &[FieldEvent::Changed];
    const UNANNOUNCED: &[FieldEvent] = &[];
    /// A short message that filled the field, its name retyped in place: the
    /// first keys make it look like another text, but the rest of the message
    /// stays, so the watch keeps going and learns the finished name.
    #[tokio::test]
    async fn a_short_message_retyped_in_place_learns_the_finished_name() {
        let pasted = "Rimpas or Kar, call me ";
        let retype: Vec<(u64, &str, &[FieldEvent])> = [
            "R, call me ",
            "Rim, call me ",
            "Rimpa, call me ",
            "Rimpa S, call me ",
            "Rimpa Sarkar, call me ",
        ]
        .into_iter()
        .enumerate()
        .map(|(i, text)| (if i == 0 { 0 } else { 90 }, text, CHANGED))
        .collect();
        let learned = Ok((pasted.to_owned(), "Rimpa Sarkar, call me ".to_owned()));
        assert_eq!(watch_events(pasted, &retype).await, learned);
        let mut sent = retype.clone();
        sent.push((120, "", CHANGED));
        assert_eq!(watch_events(pasted, &sent).await, learned);
        // Sent, or the next dictation started, mid-retype: never read finished.
        assert_eq!(
            watch_events(pasted, &[(0, "R, call me ", CHANGED), (90, "", CHANGED)]).await,
            Err(Some("editing"))
        );
        assert_eq!(
            watch_events(pasted, &[(0, "R, call me ", CHANGED)]).await,
            Err(Some("editing"))
        );
    }
    /// After a send, the field shows the next message's first key before its
    /// notification, or a placeholder: neither keeps a word of the dictation,
    /// so the watch ends and the next message is never read (review R5).
    #[tokio::test]
    async fn a_send_never_hands_the_watch_to_the_next_message_or_a_placeholder() {
        let pasted = "post gres is ready for review ";
        let next = "Postgres is ready for review";
        assert_eq!(
            watch_events(
                pasted,
                &[
                    (0, "", CHANGED),
                    (0, "p", UNANNOUNCED),
                    (30, "p", CHANGED),
                    (150, next, CHANGED)
                ]
            )
            .await,
            Err(Some("anchors-lost"))
        );
        assert_eq!(
            watch_events(
                pasted,
                &[(0, "Message #general", CHANGED), (200, next, CHANGED)]
            )
            .await,
            Err(Some("anchors-lost"))
        );
    }
    /// The last letter and Enter folded into one notification, or the clear's
    /// notification lagging the read that found the field empty: the last read
    /// may be a truncated word ("Postgre"), so nothing is learned. A plain Enter
    /// after a settled spelling still learns it, and folded notifications alone
    /// never count as edits.
    #[tokio::test]
    async fn a_change_folded_into_or_just_after_the_clear_learns_nothing() {
        let pasted = "post gres is ready ";
        let folded: &[FieldEvent] = &[FieldEvent::Changed, FieldEvent::Merged];
        assert_eq!(
            watch_events(
                pasted,
                &[(0, "Postgre is ready ", CHANGED), (200, "", folded)]
            )
            .await,
            Err(Some("unsettled"))
        );
        assert_eq!(
            watch_events(
                pasted,
                &[
                    (0, "Postgre is ready ", CHANGED),
                    (200, "", CHANGED),
                    (10, "", CHANGED)
                ]
            )
            .await,
            Err(Some("unsettled"))
        );
        let learned = Ok((pasted.to_owned(), "Postgres is ready ".to_owned()));
        assert_eq!(
            watch_events(
                pasted,
                &[(0, "Postgres is ready ", CHANGED), (200, "", CHANGED)]
            )
            .await,
            learned
        );
        assert_eq!(
            watch_events(pasted, &[(0, "Postgres is ready ", folded)]).await,
            learned
        );
        // Fail closed in a double-posting app too: a duplicate clear cannot
        // be distinguished from the last letter and a clear arriving together.
        assert_eq!(
            watch_events(
                pasted,
                &[(0, "Postgres is ready ", folded), (200, "", folded)]
            )
            .await,
            Err(Some("unsettled"))
        );
    }

    fn words(list: &[&str]) -> DictionaryConfig {
        DictionaryConfig {
            words: list.iter().map(|w| w.to_string()).collect(),
            ..DictionaryConfig::default()
        }
    }
    fn write_words(path: &std::path::Path, list: &[&str]) -> DictionaryConfig {
        let mut file = ConfigFile::load(path).unwrap();
        file.set_typed("dictionary.words", &words(list).words)
            .unwrap();
        file.save().unwrap();
        Config::load(path).unwrap().dictionary
    }
    fn reload_of(engine: &Engine, config: Config) -> Result<Reloaded> {
        Ok(Reloaded {
            config,
            services: engine.services.clone(),
            store: engine.store.clone(),
            hotkeys: None,
        })
    }
    /// Whole dictionary snapshots arrive out of order; each is installed only
    /// when nothing newer is, and memory ends equal to config.toml in every
    /// order (review R1), including an external CLI write between them.
    #[tokio::test]
    async fn dictionary_snapshots_install_in_write_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let store = Store::open(&dir.path().join("h.db"), false, 10).unwrap();
        let (mut engine, ..) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            store,
        );
        engine.config_path = Some(path.clone());
        let on_disk = |engine: &Engine| {
            assert_eq!(
                engine.config.dictionary,
                Config::load(&path).unwrap().dictionary
            );
        };
        // A learn saves {foo, claud} as 1; the CLI removes foo and reloads (its
        // read follows the learn); the learn's completion arrives last.
        let learn1 = write_words(&path, &["foo", "claud"]);
        write_words(&path, &["claud"]);
        assert!(
            engine
                .reloaded(reload_of(&engine, Config::load(&path).unwrap()), 1)
                .ok
        );
        engine.learned(learn1, 1, Some("Added 'foo'".into()), None);
        on_disk(&engine);
        // A reload read before learn 2 completes after it: its other settings
        // apply, its dictionary does not.
        let mut early = Config::load(&path).unwrap();
        early.formatting.lists = false;
        let learn2 = write_words(&path, &["Kubernetes", "claud"]);
        engine.learned(learn2, 2, None, None);
        assert!(engine.reloaded(reload_of(&engine, early), 1).ok);
        assert!(!engine.config.formatting.lists);
        on_disk(&engine);
        // Two learns out of order: the later one already holds the earlier.
        let learn3 = write_words(&path, &["Postgres", "Kubernetes", "claud"]);
        let learn4 = write_words(&path, &["Grafana", "Postgres", "Kubernetes", "claud"]);
        engine.learned(learn4, 4, None, None);
        engine.learned(learn3, 3, None, None);
        on_disk(&engine);
        // An undo (5) against a reload read before it, in both orders.
        let before = Config::load(&path).unwrap();
        let undone = write_words(&path, &["Postgres", "Kubernetes", "claud"]);
        engine.learned(undone.clone(), 5, None, None);
        assert!(engine.reloaded(reload_of(&engine, before.clone()), 4).ok);
        on_disk(&engine);
        assert!(
            engine
                .reloaded(reload_of(&engine, Config::load(&path).unwrap()), 5)
                .ok
        );
        engine.learned(undone, 5, None, None);
        on_disk(&engine);
        // A reload that read after every write installs what it read.
        write_words(&path, &["claud"]);
        assert!(
            engine
                .reloaded(reload_of(&engine, Config::load(&path).unwrap()), 5)
                .ok
        );
        on_disk(&engine);
    }

    const MINE: &str = "# mine\n[ui]\ntheme = 'paper'\n";
    /// An actor with config.toml at `path` and one dictation in history (`id`).
    async fn history_fixture(
        dir: &std::path::Path,
    ) -> (
        mpsc::Sender<Command>,
        JoinHandle<()>,
        PathBuf,
        i64,
        watch::Receiver<Option<LearnNotice>>,
    ) {
        let path = dir.join("config.toml");
        std::fs::write(&path, MINE).unwrap();
        let store = Store::open(&dir.join("h.db"), true, 10).unwrap();
        let (mut engine, tx, rx, mut events, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            store,
        );
        engine.config_path = Some(path.clone());
        // Reloads keep the fixture's services.
        engine.reload_factory = Arc::new(|_, _, services| Ok(services));
        let notices = engine.notices();
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        let id = success(&mut events).await.entry.unwrap().id;
        (tx, task, path, id, notices)
    }
    async fn correct(tx: &mpsc::Sender<Command>, id: i64, text: &str) -> Response {
        ask(
            tx,
            Request::HistoryUpdate {
                id,
                text: text.into(),
            },
        )
        .await
    }
    /// The daemon's dictionary ends equal to config.toml (by fingerprint).
    async fn in_sync(tx: &mpsc::Sender<Command>, path: &std::path::Path) {
        let file = format!(
            "{:016x}",
            Config::load(path).unwrap().dictionary.fingerprint()
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if ask(tx, Request::Status).await.learning.unwrap().fingerprint == file {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the daemon's dictionary differs from config.toml");
    }
    fn undo_request(op: Option<u64>) -> Request {
        Request::UndoLearned { op }
    }
    /// The latest automatic change is undone once, restoring config.toml's
    /// exact bytes; a wrong id or a replay writes nothing.
    #[tokio::test]
    async fn the_latest_change_is_undone_once_with_the_exact_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, task, path, id, notices) = history_fixture(dir.path()).await;
        let learned = correct(&tx, id, "Postgres is ready").await;
        assert_eq!(
            learned.message.as_deref(),
            Some("Added 'Postgres' to your dictionary")
        );
        in_sync(&tx, &path).await;
        assert!(ask(&tx, Request::Status).await.learning.unwrap().undo);
        // A history edit is not announced on the pill, but is undoable.
        assert!(notices.borrow().is_none());
        assert!(!ask(&tx, undo_request(Some(7))).await.ok);
        let undone = ask(&tx, undo_request(None)).await;
        assert!(undone.ok, "{:?}", undone.message);
        assert!(undone.message.unwrap().contains("'Postgres'"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), MINE);
        in_sync(&tx, &path).await;
        let replay = ask(&tx, undo_request(None)).await;
        assert!(!replay.ok);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), MINE);
        assert!(!ask(&tx, Request::Status).await.learning.unwrap().undo);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
        // A restarted daemon has nothing to undo.
        let dir = tempfile::tempdir().unwrap();
        let (tx, task, path, ..) = history_fixture(dir.path()).await;
        assert!(!ask(&tx, undo_request(None)).await.ok);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), MINE);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// A manual save after the learn, even of identical bytes (`dictionary add`
    /// of the same word), adopts it: undo refuses and the word stays. The reload
    /// a CLI write asks for ends the undo too. Words saved together go together.
    #[tokio::test]
    async fn any_later_save_adopts_the_change_and_a_batch_is_undone_together() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, task, path, id, _) = history_fixture(dir.path()).await;
        correct(&tx, id, "Postgres is ready").await;
        {
            let _lock = crate::config_edit::lock(&path).unwrap();
            ConfigFile::load(&path).unwrap().save().unwrap();
        }
        let refused = ask(&tx, undo_request(None)).await;
        assert!(!refused.ok && refused.message.unwrap().contains("changed"));
        assert!(Config::load(&path)
            .unwrap()
            .dictionary
            .words
            .contains(&"Postgres".to_owned()));
        in_sync(&tx, &path).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        let (tx, task, path, id, _) = history_fixture(dir.path()).await;
        correct(&tx, id, "Postgres is ready").await;
        assert!(ask(&tx, Request::Reload).await.ok);
        assert!(!ask(&tx, undo_request(None)).await.ok);
        assert!(Config::load(&path)
            .unwrap()
            .dictionary
            .words
            .contains(&"Postgres".to_owned()));
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        let (tx, task, path, id, _) = history_fixture(dir.path()).await;
        assert_eq!(
            correct(&tx, id, "Postgres is Readie")
                .await
                .message
                .as_deref(),
            Some("Added 'Postgres', 'Readie' to your dictionary")
        );
        assert!(ask(&tx, undo_request(None)).await.ok);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), MINE);
        in_sync(&tx, &path).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// A correction seen in the field the dictation was pasted into is offered
    /// on the pill (the exact saved word, its change id), and the pill's Undo
    /// sends that id back; the notice goes once the change can't be undone.
    #[tokio::test]
    async fn an_observed_correction_is_offered_on_the_pill_until_undone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, MINE).unwrap();
        let (notify, field_events) = mpsc::unbounded_channel();
        let desktop = Arc::new(RecordingDesktop {
            field: Arc::new(Mutex::new(Some(String::new()))),
            field_events: Mutex::new(Some(field_events)),
            ..RecordingDesktop::default()
        });
        let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
        let (mut engine, tx, rx, mut events, _) =
            fixture(Config::default(), desktop.clone(), store);
        engine.config_path = Some(path.clone());
        let mut notices = engine.notices();
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        success(&mut events).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        *desktop.field.lock().unwrap() = Some("Postgres is ready ".into());
        notify.send(FieldEvent::Changed).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        *desktop.field.lock().unwrap() = Some(String::new());
        notify.send(FieldEvent::Changed).unwrap();
        let notice =
            tokio::time::timeout(Duration::from_secs(3), notices.wait_for(Option::is_some))
                .await
                .expect("no notice")
                .unwrap()
                .clone()
                .unwrap();
        assert_eq!(notice.words, ["Postgres"]);
        assert!(!notice.correction && notice.op != 0);
        let undone = ask(&tx, undo_request(Some(notice.op))).await;
        assert!(undone.ok, "{:?}", undone.message);
        assert!(notices.borrow().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), MINE);
        in_sync(&tx, &path).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// The daemon never waits on a stopped lock holder: learning and undo fail
    /// closed within a bounded wait and write nothing. A CLI command that holds
    /// the lock while it asks for a reload never deadlocks it either, since
    /// reloads don't take the lock.
    #[tokio::test]
    async fn a_held_config_lock_bounds_learning_and_undo_and_never_blocks_reload() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, task, path, id, _) = history_fixture(dir.path()).await;
        let held = crate::config_edit::lock(&path).unwrap();
        let started = Instant::now();
        let refused = correct(&tx, id, "Postgres is ready").await;
        assert!(started.elapsed() < Duration::from_secs(2));
        let message = refused.message.unwrap_or_default();
        assert!(
            message.starts_with("Transcript saved; dictionary not updated"),
            "{message}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), MINE);
        assert!(ask(&tx, Request::Reload).await.ok);
        drop(held);
        // The entry already reads "Postgres is ready": correct another word.
        let learned = correct(&tx, id, "Postgres is Readie").await;
        assert_eq!(
            learned.message.as_deref(),
            Some("Added 'Readie' to your dictionary")
        );
        let held = crate::config_edit::lock(&path).unwrap();
        let after = std::fs::read(&path).unwrap();
        assert!(!ask(&tx, undo_request(None)).await.ok);
        drop(held);
        assert_eq!(std::fs::read(&path).unwrap(), after);
        in_sync(&tx, &path).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// A learn that waits for another writer's lock reads the file afresh, so
    /// that writer's edit (`config set`) and the learned word are both kept.
    #[tokio::test]
    async fn a_learn_behind_another_writer_keeps_both_edits() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, task, path, id, _) = history_fixture(dir.path()).await;
        let held = crate::config_edit::lock(&path).unwrap();
        let mut file = ConfigFile::load(&path).unwrap();
        file.set("ui.theme", "midnight").unwrap();
        let tx2 = tx.clone();
        let learning = tokio::spawn(async move { correct(&tx2, id, "Postgres is ready").await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        file.save().unwrap();
        drop(held);
        let learned = learning.await.unwrap();
        assert_eq!(
            learned.message.as_deref(),
            Some("Added 'Postgres' to your dictionary")
        );
        let config = Config::load(&path).unwrap();
        assert_eq!(config.ui.theme, "midnight");
        assert_eq!(config.dictionary.words, ["Postgres"]);
        in_sync(&tx, &path).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// A reload asked for while another is in flight is queued, not refused, and
    /// gets a read of its own after it: a CLI write made after the first read is
    /// never lost from memory (review R1).
    #[tokio::test]
    async fn a_reload_during_a_reload_is_answered_by_a_fresh_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, MINE).unwrap();
        let store = Store::open(&dir.path().join("h.db"), false, 10).unwrap();
        let (mut engine, tx, rx, _, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            store,
        );
        engine.config_path = Some(path.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        let counted = calls.clone();
        engine.reload_factory = Arc::new(move |_, _, services| {
            // The first reload holds after reading the file.
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                let _ = released.lock().unwrap().recv();
            }
            Ok(services)
        });
        let task = tokio::spawn(engine.run(rx));
        let first = tokio::spawn({
            let tx = tx.clone();
            async move { ask(&tx, Request::Reload).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // A CLI write after that read, then its reload.
        {
            let _lock = crate::config_edit::lock(&path).unwrap();
            write_words(&path, &["Kubernetes"]);
        }
        let second = tokio::spawn({
            let tx = tx.clone();
            async move { ask(&tx, Request::Reload).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        release.send(()).unwrap();
        assert!(first.await.unwrap().ok);
        assert!(second.await.unwrap().ok);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        in_sync(&tx, &path).await;
        assert_eq!(ask(&tx, Request::Status).await.learning.unwrap().words, 1);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    /// Status reports learning outcomes as counts only.
    #[tokio::test]
    async fn learning_status_is_counts_only() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, task, path, id, _) = history_fixture(dir.path()).await;
        correct(&tx, id, "Postgres is ready").await;
        in_sync(&tx, &path).await;
        let status = ask(&tx, Request::Status).await;
        let json = serde_json::to_string(&status).unwrap();
        assert!(
            !json.contains("Postgres") && !json.contains("gres"),
            "{json}"
        );
        let learning = status.learning.unwrap();
        assert_eq!(
            (learning.words, learning.learned, learning.replacements),
            (1, 1, 0)
        );
        // The paste with no readable field was watched and counted.
        let learning = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let learning = ask(&tx, Request::Status).await.learning.unwrap();
                if learning.watches > 0 {
                    return learning;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(learning.watches, 1);
        assert_eq!(learning.outcomes.get("unreadable"), Some(&1));
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_failed_session_clears_the_last_success_timings() {
        // Hardware test: status kept stt_ms=387 from the last success through HTTP 429s.
        struct SecondFails(AtomicUsize);
        #[async_trait]
        impl SpeechToText for SecondFails {
            fn name(&self) -> &str {
                "fixture"
            }
            async fn transcribe(
                &self,
                _: AudioClip,
                _: TranscriptionOptions,
            ) -> Result<Transcript> {
                if self.0.fetch_add(1, Ordering::SeqCst) > 0 {
                    bail!("provider returned HTTP 429; response body omitted");
                }
                Ok(Transcript {
                    provider: None,
                    text: "Postgres is ready.".into(),
                    language: None,
                })
            }
        }
        let desktop = Arc::new(RecordingDesktop::default());
        let (mut engine, tx, rx, mut events, _) =
            fixture(Config::default(), desktop, unused_store());
        engine.services.stt = Arc::new(SecondFails(AtomicUsize::new(0)));
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        success(&mut events).await;
        assert!(ask(&tx, Request::Status).await.timings.is_some());
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        let failed = settled(&mut events).await;
        assert_eq!(failed.state, State::Error, "{:?}", failed.message);
        assert!(failed.timings.is_none());
        let status = ask(&tx, Request::Status).await;
        assert_eq!(status.state, State::Error);
        assert!(status.timings.is_none());
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
            (Some(PROTOCOL_VERSION), Some("fixture"), Some("mock-model"))
        );
        let request = Request::Start {
            mode: Mode::Dictation,
            context: None,
            t0_us: Some(monotonic_us()),
            delivery: Delivery::Inject,
            hold: false,
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
            let mut pill = engine.desktop_events.subscribe();
            let task = tokio::spawn(engine.run(rx));
            ask(
                &tx,
                Request::Start {
                    mode: Mode::Dictation,
                    context: None,
                    t0_us: None,
                    delivery,
                    hold: false,
                },
            )
            .await;
            ask(&tx, Request::Stop).await;
            let final_event = success(&mut events).await;
            // An asked-for copy is no failure: the pill hears nothing; watch keeps it all.
            let pill_final = loop {
                let event = pill.recv().await.unwrap();
                if event.state == State::Success {
                    break event;
                }
            };
            assert_eq!(pill_final.message, None);
            if delivery == Delivery::Clipboard {
                assert!(final_event.message.unwrap().starts_with(COPIED));
            }
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
    async fn preferred_languages_reload_rebuilds_stt_for_next_dictation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let store = Store::open(&dir.path().join("h.db"), false, 10).unwrap();
        let (mut engine, tx, rx, mut events, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            store,
        );
        engine.config_path = Some(path.clone());
        engine.reload_factory = Arc::new(|_, new, services| {
            assert_eq!(new.stt.languages, ["hi", "en"]);
            Ok(Services {
                stt: Arc::new(ImmediateStt {
                    calls: Arc::new(AtomicUsize::new(0)),
                    text: "कल API deploy करना है।",
                }),
                ..services.clone()
            })
        });
        let task = tokio::spawn(engine.run(rx));
        std::fs::write(
            &path,
            "[stt]\nlanguages = ['hi', 'en']\n[privacy]\nhistory = false\n",
        )
        .unwrap();
        assert!(ask(&tx, Request::Reload).await.ok);
        assert!(ask(&tx, Request::start()).await.ok);
        assert!(ask(&tx, Request::Stop).await.ok);
        assert_eq!(
            success(&mut events).await.text.as_deref(),
            Some("कल API deploy करना है। ")
        );
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
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
                    delivery: Delivery::Inject,
                    hold: false,
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
    /// Replies with fixed text, counting edits and warm-ups.
    struct Reply(&'static str, AtomicUsize, AtomicUsize);
    #[async_trait]
    impl TextTransformer for Reply {
        async fn warm(&self) -> Result<()> {
            self.2.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn transform(&self, _: TransformRequest<'_>) -> Result<String> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(self.0.into())
        }
    }
    #[tokio::test]
    async fn light_cleanup_skips_short_dictations_and_rejects_answers() {
        let question = "can you tell me what the capital of France is";
        let answer = "The capital of France is Paris, which is also its largest city and home to about two million people.";
        use CleanupOutcome::{Applied, Rejected, Skipped};
        for (said, reply, calls, delivered, skipped, rejected, outcome) in [
            (
                "ok sounds good",
                "Okay.",
                0,
                "ok sounds good",
                true,
                false,
                Skipped,
            ),
            (
                "um three, actually make it five",
                "Make it 5.",
                1,
                "Make it 5.",
                false,
                false,
                Applied,
            ),
            (question, answer, 1, question, false, true, Rejected),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (mut engine, tx, rx, mut events, _) = fixture(
                Config::default(),
                Arc::new(RecordingDesktop::default()),
                Store::open(&dir.path().join("h.db"), true, 10).unwrap(),
            );
            engine.config.cleanup.mode = CleanupMode::Light;
            engine.services.stt = Arc::new(ImmediateStt {
                calls: Arc::default(),
                text: said,
            });
            let transformer = Arc::new(Reply(reply, AtomicUsize::new(0), AtomicUsize::new(0)));
            engine.services.transformer = Some(transformer.clone());
            let task = tokio::spawn(engine.run(rx));
            ask(&tx, Request::start()).await;
            ask(&tx, Request::Stop).await;
            let r = success(&mut events).await;
            assert_eq!(transformer.1.load(Ordering::SeqCst), calls, "{said}");
            // Recording start warms the cleanup connection, even when it's skipped.
            assert_eq!(transformer.2.load(Ordering::SeqCst), 1, "{said}");
            assert_eq!(r.text.as_deref().map(str::trim_end), Some(delivered));
            // History keeps what cleanup did (#67).
            assert_eq!(r.entry.as_ref().unwrap().cleanup, Some(outcome), "{said}");
            let timings = r.timings.unwrap();
            assert_eq!(timings.cleanup_skipped, skipped, "{said}");
            assert_eq!(timings.cleanup_ms.is_none(), skipped, "{said}");
            assert_eq!(timings.cleanup_rejected, rejected, "{said}");
            assert_eq!(
                r.message
                    .is_some_and(|m| m.contains("Cleanup changed too much")),
                rejected
            );
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    /// Answers "open use task store" and keeps the hints it was sent: the
    /// user's and the shipped domain terms.
    #[derive(Default)]
    struct HintStt(Mutex<Vec<Vec<String>>>, Mutex<Vec<&'static str>>);
    #[async_trait]
    impl SpeechToText for HintStt {
        fn name(&self) -> &str {
            "fixture"
        }
        async fn transcribe(
            &self,
            _: AudioClip,
            options: TranscriptionOptions,
        ) -> Result<Transcript> {
            self.0.lock().unwrap().push(options.vocabulary);
            self.1.lock().unwrap().extend(options.domain);
            Ok(Transcript {
                provider: None,
                text: "open use task store".into(),
                language: Some("en".into()),
            })
        }
    }
    /// One dictation into `app` with the given title settings: what was pasted,
    /// the STT hints, the saved history and the number of title reads.
    async fn titled_dictation(
        app: &str,
        terminal_input: bool,
        window_terms: bool,
        snap: bool,
        title_delay: Duration,
    ) -> (String, Vec<String>, String, usize, Duration) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db"), true, 10).unwrap();
        let desktop = Arc::new(RecordingDesktop {
            context: Some(AppContext {
                app_id: Some(app.into()),
                window_id: Some("1:2".into()),
                terminal_input,
                ..Default::default()
            }),
            title: Some("useTaskStore.ts — myapp"),
            title_delay,
            ..Default::default()
        });
        let mut config = Config::default();
        config.privacy.window_terms = window_terms;
        config.formatting.snap_to_dictionary = snap;
        let (mut engine, tx, rx, mut events, _) = fixture(config.clone(), desktop.clone(), store);
        let stt = Arc::new(HintStt::default());
        engine.services.stt = stt.clone();
        let task = tokio::spawn(engine.run(rx));
        assert!(ask(&tx, Request::start()).await.ok);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let stopped = Instant::now();
        assert!(ask(&tx, Request::Stop).await.ok);
        success(&mut events).await;
        let elapsed = stopped.elapsed();
        let history = ask(
            &tx,
            Request::History {
                limit: 5,
                offset: 0,
                query: None,
            },
        )
        .await;
        let history = serde_json::to_string(&history.history).unwrap();
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
        let pasted = desktop.injections.lock().unwrap().concat();
        let hints = stt.0.lock().unwrap().concat();
        // Shipped hints follow the same focused app; title terms are never hints.
        // `fixture` turns the dev pack off, so the expected selection does too.
        config.vocab.dev = false;
        let terminal = terminal_input || xflow_platform::is_terminal(Some(app));
        let domain = stt.1.lock().unwrap().clone();
        assert_eq!(
            domain,
            xflow_core::hints::select(
                &config,
                &xflow_core::hints::Signals {
                    terminal,
                    app: Some(app),
                    ..Default::default()
                }
            )
        );
        assert!(!domain
            .iter()
            .any(|t| t.contains("TaskStore") || t.contains("myapp")));
        (
            pasted,
            hints,
            history,
            desktop.title_reads.load(Ordering::SeqCst),
            elapsed,
        )
    }
    #[tokio::test]
    async fn window_title_terms_are_opt_in_local_and_never_kept() {
        let instant = Duration::ZERO;
        // Off (the default), snapping off, or a non-developer app (even a browser's web
        // terminal, as in vscode.dev or Codespaces): the title is never read.
        for (app, terminal_input, on, snap) in [
            ("com.microsoft.VSCode", false, false, true),
            ("com.microsoft.VSCode", false, true, false),
            ("com.google.Chrome", false, true, true),
            ("com.google.Chrome", true, true, true),
            ("com.apple.Safari", true, true, true),
            ("com.agilebits.onepassword7", false, true, true),
        ] {
            let (pasted, hints, _, reads, _) =
                titled_dictation(app, terminal_input, on, snap, instant).await;
            assert_eq!(reads, 0, "{app} {terminal_input} {on} {snap}");
            assert!(!pasted.contains("useTaskStore"), "{pasted}");
            assert!(hints.is_empty());
        }
        for app in ["com.microsoft.VSCode", "com.mitchellh.ghostty"] {
            let (pasted, hints, history, reads, _) =
                titled_dictation(app, false, true, true, instant).await;
            assert_eq!(reads, 1);
            assert!(pasted.contains("open useTaskStore"), "{pasted}");
            // Local only: no provider hint, and only the dictation itself is saved.
            assert!(hints.is_empty(), "{hints:?}");
            assert!(history.contains("useTaskStore"));
            assert!(!history.contains("myapp") && !history.contains("useTaskStore.ts"));
        }
    }
    #[tokio::test]
    async fn slow_window_title_never_holds_back_the_dictation() {
        let (pasted, _, _, reads, elapsed) = titled_dictation(
            "com.microsoft.VSCode",
            false,
            true,
            true,
            Duration::from_secs(30),
        )
        .await;
        assert_eq!(reads, 1);
        // The read is dropped after 100 ms; the dictation is delivered without its terms.
        assert!(pasted.contains("open use task store"), "{pasted}");
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    }
    #[tokio::test]
    async fn terminal_multiline_copies_but_normal_editor_buffers_paste() {
        for (app, terminal_input) in [
            ("com.mitchellh.ghostty", false),
            ("com.microsoft.VSCode", true),
            ("com.microsoft.VSCode", false),
        ] {
            let context = AppContext {
                app_id: Some(app.into()),
                window_id: Some("scratch".into()),
                terminal_input,
                ..Default::default()
            };
            let desktop = Arc::new(RecordingDesktop {
                context: Some(context),
                guard_terminal: true,
                ..Default::default()
            });
            let mut config = Config::default();
            config.cleanup.mode = CleanupMode::Light;
            config.snippets.push(xflow_core::config::Snippet {
                trigger: "terminal fixture".into(),
                text: "git status\necho “$VARS” `some_fn()`\necho ‘userId’ …\n".into(),
            });
            let (mut engine, tx, rx, mut events, _) = fixture(
                config,
                desktop.clone(),
                Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
            );
            engine.services.stt = Arc::new(ImmediateStt {
                calls: Arc::default(),
                text: "terminal fixture",
            });
            let task = tokio::spawn(engine.run(rx));
            ask(
                &tx,
                Request::Start {
                    mode: Mode::Dictation,
                    context: None,
                    t0_us: None,
                    delivery: Delivery::Inject,
                    hold: false,
                },
            )
            .await;
            ask(&tx, Request::Stop).await;
            let r = success(&mut events).await;
            if terminal_input || app == "com.mitchellh.ghostty" {
                assert_eq!(r.injection, Some(InjectionOutcome::ClipboardOnly));
                assert!(desktop.injections.lock().unwrap().is_empty());
                assert_eq!(
                    *desktop.copies.lock().unwrap(),
                    ["git status\necho \"$VARS\" `some_fn()`\necho 'userId' ..."]
                );
                assert!(r
                    .message
                    .unwrap()
                    .contains("Terminal paste capability unverified"));
            } else {
                assert_eq!(r.injection, Some(InjectionOutcome::Pasted));
                assert_eq!(
                    *desktop.injections.lock().unwrap(),
                    ["git status\necho “$VARS” `some_fn()`\necho ‘userId’ …\n"]
                );
                assert!(desktop.copies.lock().unwrap().is_empty());
                assert!(!r
                    .message
                    .unwrap_or_default()
                    .contains("Terminal paste capability unverified"));
            }
            assert_eq!(desktop.watches.load(Ordering::SeqCst), 0);
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn scratch_that_alone_discards_without_cleanup_or_paste() {
        // The parser's outcome decides, not the raw words (#107 review), in an
        // editor and in a guarded terminal (#112) alike.
        for (said, app) in [
            ("Um, scratch that.", None),
            ("Bring notes. Scratch, that.", None),
            ("Scratch\nthat.", None),
            ("Bring notes. Scratch that.", Some("com.mitchellh.ghostty")),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("h.db"), true, 10).unwrap();
            // Smart spacing (#109) has a field to read: a discard still pastes nothing.
            let desktop = Arc::new(RecordingDesktop {
                around: Some(("Done.", "")),
                context: app.map(|app| AppContext {
                    app_id: Some(app.into()),
                    ..Default::default()
                }),
                guard_terminal: app.is_some(),
                ..RecordingDesktop::default()
            });
            let mut config = Config::default();
            config.injection.smart_spacing = true;
            let (mut engine, tx, rx, mut events, _) =
                fixture(config, desktop.clone(), store.clone());
            engine.config.cleanup.mode = CleanupMode::Polished;
            engine.services.stt = Arc::new(ImmediateStt {
                calls: Arc::default(),
                text: said,
            });
            let transformer = Arc::new(Reply(
                "resurrected",
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ));
            engine.services.transformer = Some(transformer.clone());
            let task = tokio::spawn(engine.run(rx));
            ask(&tx, Request::start()).await;
            ask(&tx, Request::Stop).await;
            let idle = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let r = events.recv().await.unwrap();
                    assert!(
                        !matches!(r.state, State::Success | State::Error),
                        "{said}: {:?}",
                        r.message
                    );
                    if r.state == State::Idle && r.message.is_some() {
                        return r;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(idle.message.as_deref(), Some(DISCARDED), "{said}");
            // Cleanup can't resurrect it, nothing is pasted, and history says why.
            assert_eq!(transformer.1.load(Ordering::SeqCst), 0, "{said}");
            assert!(desktop.injections.lock().unwrap().is_empty(), "{said}");
            assert!(desktop.copies.lock().unwrap().is_empty(), "{said}");
            let (rows, _) = store.history(10, 0, None).await.unwrap();
            assert_eq!(rows.len(), 1, "{said}");
            assert_eq!(rows[0].text, "");
            assert_eq!(rows[0].raw_text.as_deref(), Some(said));
            assert_eq!(rows[0].cleanup, Some(CleanupOutcome::Discarded));
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn terminals_keep_their_text_unless_a_style_asks_for_cleanup() {
        let said = "git commit dash m fix the build on the main branch";
        for (style, calls) in [(None, 0), (Some(CleanupMode::Light), 1)] {
            let mut config = Config::default();
            config.cleanup.mode = CleanupMode::Light;
            config.styles = style
                .map(|mode| xflow_core::config::Style {
                    name: "shell".into(),
                    apps: vec!["terminal".into()],
                    mode: Some(mode),
                    prompt: None,
                })
                .into_iter()
                .collect();
            let (mut engine, tx, rx, mut events, _) = fixture(
                config,
                Arc::new(RecordingDesktop::default()),
                Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
            );
            engine.services.stt = Arc::new(ImmediateStt {
                calls: Arc::default(),
                text: said,
            });
            let transformer = Arc::new(Reply(
                "Git commit dash m, fix the build on the main branch.",
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ));
            engine.services.transformer = Some(transformer.clone());
            let task = tokio::spawn(engine.run(rx));
            let start = Request::Start {
                mode: Mode::Dictation,
                context: Some(AppContext {
                    app_id: Some("com.apple.Terminal".into()),
                    ..AppContext::default()
                }),
                t0_us: None,
                delivery: Delivery::Inject,
                hold: false,
            };
            ask(&tx, start).await;
            ask(&tx, Request::Stop).await;
            let r = success(&mut events).await;
            assert_eq!(transformer.1.load(Ordering::SeqCst), calls);
            assert_eq!(r.timings.unwrap().cleanup_skipped, calls == 0);
            if calls == 0 {
                // Local spoken symbols still run without cleanup.
                assert_eq!(
                    r.text.as_deref().map(str::trim_end),
                    Some("git commit -m fix the build on the main branch")
                );
            }
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    struct Stalled;
    #[async_trait]
    impl TextTransformer for Stalled {
        async fn transform(&self, _: TransformRequest<'_>) -> Result<String> {
            tokio::time::sleep(Duration::from_secs(600)).await;
            Ok("late".into())
        }
    }
    #[tokio::test]
    async fn a_stalled_cleanup_pastes_the_local_text_after_its_budget() {
        let (mut engine, tx, rx, mut events, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        engine.config.cleanup.mode = CleanupMode::Light;
        engine.services.transformer = Some(Arc::new(Stalled));
        engine.services.stt = Arc::new(ImmediateStt {
            calls: Arc::default(),
            text: "the deploy failed because the token expired last night",
        });
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        let stopped = Instant::now();
        ask(&tx, Request::Stop).await;
        let r = tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let r = events.recv().await.unwrap();
                if r.state == State::Success {
                    return r;
                }
            }
        })
        .await
        .unwrap();
        // 3 s plus 25 ms a word, not the 20 s provider timeout.
        assert!(stopped.elapsed() < Duration::from_secs(4));
        assert_eq!(
            r.text.as_deref().map(str::trim_end),
            Some("the deploy failed because the token expired last night")
        );
        assert!(r.message.unwrap().contains("Cleanup was too slow"));
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
                    delivery: Delivery::Inject,
                    hold: false,
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
        let r = hotkey_state(&mut events, State::Idle).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(r.message.as_deref(), Some(NOT_CAUGHT));
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
                hold: false,
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
                    hold: false,
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
    /// Records the first upload's options; the dictation is then cancelled.
    async fn first_upload(desktop: RecordingDesktop, config: Config) -> TranscriptionOptions {
        let desktop = Arc::new(desktop);
        let (called, options) = (Arc::new(Notify::new()), Arc::new(Mutex::new(Vec::new())));
        let (mut engine, tx, rx, _, _) = fixture(
            config,
            desktop.clone(),
            Store::open(std::path::Path::new("unused"), false, 5).unwrap(),
        );
        engine.services.stt = Arc::new(FakeStt {
            called: called.clone(),
            release: Arc::new(Notify::new()),
            options: options.clone(),
        });
        let task = tokio::spawn(engine.run(rx));
        assert_eq!(ask(&tx, Request::start()).await.state, State::Listening);
        // Single-threaded: once the read has started, an unblocked one has finished.
        while desktop.contexts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(ask(&tx, Request::Stop).await.state, State::Processing);
        tokio::time::timeout(Duration::from_secs(2), called.notified())
            .await
            .unwrap();
        assert_eq!(ask(&tx, Request::Cancel).await.state, State::Idle);
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
        let sent = options.lock().unwrap()[0].clone();
        sent
    }
    #[tokio::test]
    async fn uploads_hint_the_focused_apps_domains_without_waiting() {
        let excel = || RecordingDesktop {
            context: Some(AppContext {
                app_id: Some("com.microsoft.Excel".into()),
                ..AppContext::default()
            }),
            ..RecordingDesktop::default()
        };
        // `fixture` turns the dev pack off; the selection must match that config.
        let mut config = Config::default();
        config.vocab.dev = false;
        let sent = first_upload(excel(), Config::default()).await;
        assert_eq!(sent.domain[0], "XLOOKUP");
        assert_eq!(
            sent.domain,
            xflow_core::hints::select(
                &config,
                &xflow_core::hints::Signals {
                    app: Some("com.microsoft.Excel"),
                    ..Default::default()
                }
            )
        );
        // A focus read that never returns: the upload goes at once with the
        // last known focus (none yet), never waiting for it.
        let blocked = RecordingDesktop {
            block_context: Some(Arc::new(Notify::new())),
            ..excel()
        };
        let sent = first_upload(blocked, Config::default()).await;
        assert_eq!(
            sent.domain,
            xflow_core::hints::select(&config, &Default::default())
        );
        assert!(!sent.domain.contains(&"XLOOKUP"));
        let mut off = Config::default();
        off.stt.domain_hints = false;
        assert!(first_upload(excel(), off).await.domain.is_empty());
    }
    /// The first listening event whose message contains `text`, or None after `within`.
    async fn listening_with(
        events: &mut broadcast::Receiver<Response>,
        text: &str,
        within: Duration,
    ) -> Option<Response> {
        tokio::time::timeout(within, async {
            loop {
                let r = events.recv().await.unwrap();
                if r.state == State::Listening
                    && r.message.as_deref().is_some_and(|m| m.contains(text))
                {
                    return r;
                }
            }
        })
        .await
        .ok()
    }
    /// Paused clock: the 25 Hz level tick runs on exact virtual time, so the bounds cannot flake.
    #[tokio::test(start_paused = true)]
    async fn muted_mic_hint_appears_by_two_and_a_half_seconds_and_clears_on_sound() {
        let (mut engine, tx, rx, mut events, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let audio = Arc::new(LevelAudio(std::sync::atomic::AtomicU32::new(
            0.0_f32.to_bits(),
        )));
        engine.services.audio = audio.clone();
        let task = tokio::spawn(engine.run(rx));
        let started = tokio::time::Instant::now();
        ask(&tx, Request::start()).await;
        listening_with(&mut events, NO_SIGNAL, Duration::from_millis(2500))
            .await
            .expect("no-signal hint within 2.5 s");
        let at = started.elapsed();
        assert!(
            at >= NO_SIGNAL_AFTER && at <= NO_SIGNAL_AFTER + Duration::from_millis(80),
            "{at:?}"
        );
        audio.0.store(0.2_f32.to_bits(), Ordering::SeqCst);
        // The engine is parked on its next tick, which reads the new level.
        let next = events.recv().await.unwrap();
        assert_eq!((next.state, next.message), (State::Listening, None));
        // Quiet after speech is a pause, not a dead microphone.
        audio.0.store(0.0_f32.to_bits(), Ordering::SeqCst);
        assert!(
            listening_with(&mut events, NO_SIGNAL, Duration::from_secs(5))
                .await
                .is_none()
        );
        assert_eq!(ask(&tx, Request::Status).await.state, State::Listening);
        ask(&tx, Request::Cancel).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn cap_warning_is_emitted_a_minute_before_max_and_resets_per_session() {
        let mut config = Config::default();
        config.recording.max_seconds = ENDS_SOON_SECS as u32 + 1;
        let (mut engine, tx, rx, mut events, _) = fixture(
            config,
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        engine.services.audio = Arc::new(LevelAudio(std::sync::atomic::AtomicU32::new(
            0.2_f32.to_bits(),
        )));
        let task = tokio::spawn(engine.run(rx));
        for _ in 0..2 {
            let started = tokio::time::Instant::now();
            ask(&tx, Request::start()).await;
            listening_with(&mut events, ENDS_SOON, Duration::from_millis(1500))
                .await
                .expect("warning one minute before the cap");
            let at = started.elapsed();
            assert!(
                at >= Duration::from_secs(1) && at <= Duration::from_millis(1080),
                "{at:?}"
            );
            assert_eq!(ask(&tx, Request::Status).await.state, State::Listening);
            // A terminal event ends the session clock; the next session starts unwarned.
            ask(&tx, Request::Cancel).await;
        }
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[test]
    fn gnome_pill_knows_the_listening_hints() {
        let logic = include_str!("../../../../../packaging/gnome-extension/logic.js");
        assert!(logic.contains(NO_SIGNAL) && logic.contains(ENDS_SOON));
        assert!(logic.contains("Copied — press Ctrl+V to paste"));
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
        let (mut client, server) = UnixStream::pair().unwrap();
        let connection = tokio::spawn(connection(
            server,
            tx.clone(),
            engine.events.clone(),
            engine.live.subscribe(),
        ));
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
        write_frame(&mut client, &Request::start()).await.unwrap();
        assert_eq!(ask(&tx, Request::Status).await.state, State::Idle);
        let waiting = hotkey_state(&mut events, State::Idle).await;
        assert_eq!(waiting.message.as_deref(), Some(MIC_WAITING));
        assert_eq!(
            ask(&tx, Request::Status).await.message.as_deref(),
            Some(MIC_WAITING)
        );
        drop(client);
        tokio::time::timeout(Duration::from_millis(500), connection)
            .await
            .expect("disconnected requester still owns the start reply")
            .unwrap()
            .unwrap();
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
    #[tokio::test]
    async fn start_reply_lost_to_a_disconnect_cancels_only_its_own_session() {
        let (mut engine, tx, rx, mut events, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let audio = Arc::new(GatedAudio::default());
        engine.services.audio = audio.clone();
        let (mut client, server) = UnixStream::pair().unwrap();
        let connection = connection(
            server,
            tx.clone(),
            engine.events.clone(),
            engine.live.subscribe(),
        );
        tokio::pin!(connection);
        let task = tokio::spawn(engine.run(rx));
        write_frame(&mut client, &Request::start()).await.unwrap();
        tokio::select! {
            result = &mut connection => panic!("connection ended before capture opened: {result:?}"),
            waiting = hotkey_state(&mut events, State::Idle) => {
                assert_eq!(waiting.message.as_deref(), Some(MIC_WAITING));
            }
        }
        // Hold the connection future until capture starts, so EOF and the reply are both ready.
        drop(client);
        audio.open.notify_one();
        hotkey_state(&mut events, State::Listening).await;
        connection.await.unwrap();
        hotkey_state(&mut events, State::Idle).await;
        assert_eq!(audio.cancels.load(Ordering::SeqCst), 1);
        // A late abandon for that session leaves a newer one alone.
        audio.open.notify_one();
        assert_eq!(ask(&tx, Request::start()).await.state, State::Listening);
        let stale = ask(&tx, Request::Abandon { session: 1 }).await;
        assert_eq!(stale.state, State::Listening);
        ask(&tx, Request::Cancel).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn a_held_session_ends_with_its_connection_and_closes_it_when_done() {
        let (mut engine, tx, rx, _, _) = fixture(
            Config::default(),
            Arc::new(RecordingDesktop::default()),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let audio = Arc::new(FakeAudio(AtomicBool::new(false)));
        engine.services.audio = audio.clone();
        let (events, live) = (engine.events.clone(), engine.live.subscribe());
        let task = tokio::spawn(engine.run(rx));
        let start = |hold| {
            let (mut client, server) = UnixStream::pair().unwrap();
            let connection =
                tokio::spawn(connection(server, tx.clone(), events.clone(), live.clone()));
            async move {
                let request = Request::Start {
                    mode: Mode::Dictation,
                    context: None,
                    t0_us: None,
                    delivery: Delivery::None,
                    hold,
                };
                write_frame(&mut client, &request).await.unwrap();
                let mut client = BufReader::new(client);
                let reply: Response = read_frame(&mut client).await.unwrap().unwrap();
                assert_eq!(reply.state, State::Listening);
                (client, connection)
            }
        };
        let done = |connection: JoinHandle<Result<()>>| async {
            tokio::time::timeout(Duration::from_secs(2), connection)
                .await
                .expect("connection outlived its session")
                .unwrap()
                .unwrap();
        };
        // The holder dies mid-recording (as with kill -9): the mic is released.
        let (client, connection) = start(true).await;
        drop(client);
        done(connection).await;
        assert_eq!(ask(&tx, Request::Status).await.state, State::Idle);
        assert!(!audio.0.load(Ordering::SeqCst), "microphone still open");
        // A session that ends on its own ends the hold: the client reads EOF.
        let (mut client, connection) = start(true).await;
        ask(&tx, Request::Stop).await;
        let eof = tokio::time::timeout(
            Duration::from_secs(3),
            read_frame::<Response, _>(&mut client),
        );
        assert!(eof.await.unwrap().unwrap().is_none());
        done(connection).await;
        // Without a hold (xflow start, D-Bus) the session outlives its requester.
        let (client, connection) = start(false).await;
        done(connection).await;
        drop(client);
        assert_eq!(ask(&tx, Request::Status).await.state, State::Listening);
        ask(&tx, Request::Cancel).await;
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
    }
    #[tokio::test]
    async fn start_grace_expires_for_ipc_and_hands_free_opens() {
        for hotkey in [false, true] {
            let (mut engine, tx, rx, mut events, calls) = fixture(
                Config::default(),
                Arc::new(RecordingDesktop::default()),
                Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
            );
            let audio = Arc::new(GatedAudio::default());
            engine.services.audio = audio.clone();
            let (keys, key_events) = mpsc::channel(4);
            engine.hotkeys = Some(Box::new(FakeHotkeys(key_events)));
            let task = tokio::spawn(engine.run(rx));
            let (reply, response) = oneshot::channel();
            if hotkey {
                keys.send(HotkeyAction::Start).await.unwrap();
                keys.send(HotkeyAction::HandsFree).await.unwrap();
            } else {
                tx.send(Command {
                    request: Request::start(),
                    reply,
                })
                .await
                .unwrap();
            }
            let waiting = hotkey_state(&mut events, State::Idle).await;
            assert_eq!(waiting.message.as_deref(), Some(MIC_WAITING));
            tokio::time::sleep(START_GRACE).await;
            if !hotkey {
                let refused = tokio::time::timeout(Duration::from_millis(500), response)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(!refused.ok);
                assert!(refused.message.unwrap().contains("start again"));
            }
            let reload = ask(&tx, Request::Reload).await;
            assert!(!reload.ok);
            assert!(reload.message.unwrap().contains(MIC_WAITING));
            audio.open.notify_one();
            let discarded = hotkey_state(&mut events, State::Idle).await;
            assert!(discarded.message.is_none());
            assert_eq!(audio.cancels.load(Ordering::SeqCst), 1);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            // Once the abandoned open returns, a fresh gesture can record.
            audio.open.notify_one();
            assert_eq!(ask(&tx, Request::start()).await.state, State::Listening);
            ask(&tx, Request::Cancel).await;
            ask(&tx, Request::Shutdown).await;
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn busy_connection_returns_an_error_frame() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (commands, mut pending) = mpsc::channel(1);
        let (events, _) = broadcast::channel(1);
        let task = tokio::spawn(connection(server, commands, events, watch::channel(None).1));
        write_frame(&mut client, &Request::Status).await.unwrap();
        let command = pending.recv().await.unwrap();
        let response: Response = tokio::time::timeout(
            Duration::from_secs(10),
            read_frame(&mut BufReader::new(client)),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert!(!response.ok);
        assert_eq!(response.protocol, Some(PROTOCOL_VERSION));
        assert!(response.message.unwrap().contains("running but busy"));
        assert!(command.reply.is_closed());
        task.await.unwrap().unwrap();
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
        assert!(message.contains("Cleanup failed") && message.contains(COPIED));
    }

    #[tokio::test]
    async fn uncertain_delivery_is_explicit_and_keeps_last_without_learning() {
        let desktop = Arc::new(RecordingDesktop {
            outcome: Some(InjectionOutcome::Uncertain),
            field: Arc::new(Mutex::new(Some(String::new()))),
            ..Default::default()
        });
        let (mut engine, tx, rx, mut events, _) = fixture(
            Config::default(),
            desktop.clone(),
            Store::open(std::path::Path::new("unused"), false, 10).unwrap(),
        );
        let dir = tempfile::tempdir().unwrap();
        engine.config_path = Some(dir.path().join("config.toml"));
        let task = tokio::spawn(engine.run(rx));
        ask(&tx, Request::start()).await;
        ask(&tx, Request::Stop).await;
        let event = success(&mut events).await;
        assert_eq!(event.injection, Some(InjectionOutcome::Uncertain));
        assert!(event
            .message
            .as_deref()
            .unwrap()
            .contains("check it landed"));
        assert_eq!(ask(&tx, Request::Last).await.text, event.text);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(desktop.watches.load(Ordering::SeqCst), 0);
        let message = delivery_message(Some("Cleanup failed".into()), event.injection).unwrap();
        assert!(message.contains("Cleanup failed") && message.contains("check it landed"));
        ask(&tx, Request::Shutdown).await;
        task.await.unwrap();
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
