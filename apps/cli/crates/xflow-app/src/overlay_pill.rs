//! The daemon only forwards small events; AppKit is loaded by `xflow overlay-pill`.
use crate::daemon::LearnNotice;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{io::BufRead, process::Stdio, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
    sync::{broadcast, watch},
    time::timeout,
};
use xflow_core::{DesktopEvent, State};

mod native;

const MAX_EVENT_BYTES: usize = 192;
/// A notice frame: at most four saved words of at most 40 characters.
const MAX_NOTICE_BYTES: usize = 1024;
const WRITE_TIMEOUT: Duration = Duration::from_millis(100);

/// The learning notice a separate helper shows for one second: the exact words
/// saved and the change its Undo sends back. Its own first frame, so recording
/// frames are unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Notice {
    op: u64,
    words: Vec<String>,
    /// A correction rule for a word already saved, not a new word.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    correction: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoticeFrame {
    notice: Notice,
}
impl Notice {
    /// None when it can't be shown in full (the change stays undoable from the CLI).
    fn new(notice: &LearnNotice) -> Option<Self> {
        let notice = Self {
            op: notice.op,
            words: notice.words.clone(),
            correction: notice.correction,
        };
        notice.valid().then_some(notice)
    }
    fn valid(&self) -> bool {
        self.op != 0
            && (1..=4).contains(&self.words.len())
            && self.words.iter().all(|w| {
                (1..=40).contains(&w.chars().count()) && !w.chars().any(char::is_control)
            })
    }
}

/// When the pill shows the learning notice. Recording comes first: a session
/// start removes a visible notice and drops a waiting one; a notice saved
/// during a session (often finalized by the next dictation) waits for it and
/// shows once the pill is idle, only while it is still the current change.
/// The actor owns that: it publishes None, or a newer change, when this one
/// can no longer be undone, and a visible notice for it goes at once.
#[derive(Debug, Default)]
struct Notices {
    pending: Option<Notice>,
    /// The change of the notice helper on screen.
    shown: Option<u64>,
    /// The last change shown or dropped: never shown again.
    done: Option<u64>,
}
#[derive(Debug, PartialEq)]
enum Step {
    /// A new recording session started.
    Session,
    /// The actor's current change; `busy`: a recording or its alert is on screen.
    Latest(Option<Notice>, bool),
    /// The notice helper exited.
    Exited,
}
#[derive(Debug, PartialEq)]
enum Action {
    Show(Notice),
    /// Kill the notice helper, without waiting for it.
    Kill,
}
impl Notices {
    fn step(&mut self, step: Step) -> Vec<Action> {
        let mut actions = Vec::new();
        match step {
            Step::Session => {
                if let Some(notice) = self.pending.take() {
                    self.done = Some(notice.op);
                }
                if self.shown.take().is_some() {
                    actions.push(Action::Kill);
                }
            }
            Step::Latest(latest, busy) => {
                let op = latest.as_ref().map(|n| n.op);
                if self.shown.is_some() && self.shown != op {
                    self.shown = None;
                    actions.push(Action::Kill);
                }
                self.pending = latest.filter(|n| Some(n.op) != self.done);
                if !busy {
                    if let Some(notice) = self.pending.take() {
                        if self.shown.take().is_some() {
                            actions.push(Action::Kill);
                        }
                        self.done = Some(notice.op);
                        self.shown = Some(notice.op);
                        actions.push(Action::Show(notice));
                    }
                }
            }
            Step::Exited => self.shown = None,
        }
        actions
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PillEvent {
    state: State,
    level: f32,
    #[serde(default)]
    hands_free: bool,
    /// Listening advisories; the session continues.
    #[serde(default)]
    no_signal: bool,
    #[serde(default)]
    ends_soon: bool,
    /// The only thing shown after release: a short red failure pill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alert: Option<Alert>,
}

/// Failures the user must act on; a paste, an asked-for copy or a discard is silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Alert {
    Copied,
    NoSpeech,
    Paste,
    Transcribe,
    Record,
}

impl PillEvent {
    fn active(self) -> bool {
        matches!(self.state, State::Listening | State::Processing)
    }

    /// `processed`: this session reached Processing, so an error is a transcription failure.
    fn from_desktop(event: DesktopEvent, processed: bool) -> Self {
        use crate::daemon::{COPIED, ENDS_SOON, HANDS_FREE, INJECTION_FAILED, NO_SIGNAL};
        let message = event.message.as_deref().unwrap_or_default();
        let listening = event.state == State::Listening;
        Self {
            state: event.state,
            level: if event.level.is_finite() {
                event.level.clamp(0.0, 1.0)
            } else {
                0.0
            },
            hands_free: message.starts_with(HANDS_FREE),
            no_signal: listening && message.contains(NO_SIGNAL),
            ends_soon: listening && message.contains(ENDS_SOON),
            alert: match event.state {
                State::Idle if message == crate::speech::NOT_CAUGHT => Some(Alert::NoSpeech),
                State::Success if message.starts_with(COPIED) => Some(Alert::Copied),
                State::Error if message.starts_with(INJECTION_FAILED) => Some(Alert::Paste),
                State::Error if processed => Some(Alert::Transcribe),
                State::Error => Some(Alert::Record),
                _ => None,
            },
        }
    }
}

/// Spawn this future with a dedicated desktop-event subscription. Failures are
/// deliberately isolated from recording; no helper, timer or AppKit exists at idle.
/// The pill shows while listening and collapses on release; afterwards only a
/// session's failure brings it back, briefly. A saved correction's notice is a
/// separate short-lived helper (see `Notices`), never waited on here.
pub async fn run(
    mut events: broadcast::Receiver<DesktopEvent>,
    mut notices: watch::Receiver<Option<LearnNotice>>,
    config: std::path::PathBuf,
) {
    let mut helper: Option<Child> = None;
    // None outside a session; Some(true) once it reached Processing.
    let mut session: Option<bool> = None;
    let mut state = Notices::default();
    let mut notice: Option<Child> = None;
    let mut notices_open = true;
    loop {
        let steps = tokio::select! {
            event = events.recv() => {
                let event = match event {
                    Ok(event) => PillEvent::from_desktop(event, session == Some(true)),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                match event.state {
                    State::Listening => {
                        let mut steps = Vec::new();
                        if session.is_none() {
                            session = Some(false);
                            // Only the transition: level frames repeat at 25 Hz.
                            steps.push(Step::Session);
                            // Retry only on the next session, not on every microphone-level event.
                            helper = spawn(&config).ok();
                        }
                        if let Some(child) = &mut helper {
                            if send(child, event).await.is_err() {
                                close(&mut helper, false).await;
                            }
                        }
                        steps
                    }
                    State::Processing => {
                        session = Some(true);
                        close(&mut helper, false).await;
                        Vec::new()
                    }
                    _ => {
                        let alert = session.take().is_some() && event.alert.is_some();
                        if alert && helper.is_none() {
                            helper = spawn(&config).ok();
                        }
                        if let Some(child) = helper.as_mut().filter(|_| alert) {
                            let _ = send(child, event).await;
                        }
                        close(&mut helper, alert).await;
                        // Idle again: a notice that waited shows now, if still current.
                        vec![Step::Latest(current(&mut notices), false)]
                    }
                }
            }
            changed = notices.changed(), if notices_open => {
                if changed.is_err() {
                    notices_open = false;
                    continue;
                }
                vec![Step::Latest(current(&mut notices), session.is_some() || helper.is_some())]
            }
            _ = async { notice.as_mut().unwrap().wait().await }, if notice.is_some() => {
                notice = None;
                vec![Step::Exited]
            }
        };
        for step in steps {
            for action in state.step(step) {
                if let Some(mut child) = notice.take() {
                    // Never waited on: dropped children are reaped in the background.
                    let _ = child.start_kill();
                }
                if let Action::Show(shown) = action {
                    notice = show(&config, &shown).await;
                    if notice.is_none() {
                        state.step(Step::Exited);
                    }
                }
            }
        }
    }
    close(&mut helper, false).await;
    if let Some(mut child) = notice.take() {
        let _ = child.start_kill();
    }
}

fn current(notices: &mut watch::Receiver<Option<LearnNotice>>) -> Option<Notice> {
    notices.borrow_and_update().as_ref().and_then(Notice::new)
}

/// A notice helper with its one frame sent; None when the overlay is off or
/// the helper failed.
async fn show(config: &std::path::Path, notice: &Notice) -> Option<Child> {
    let mut child = spawn(config).ok()?;
    let mut bytes = serde_json::to_vec(&NoticeFrame {
        notice: notice.clone(),
    })
    .ok()?;
    bytes.push(b'\n');
    if bytes.len() > MAX_NOTICE_BYTES {
        let _ = child.start_kill();
        return None;
    }
    let mut stdin = child.stdin.take()?;
    if !matches!(timeout(WRITE_TIMEOUT, stdin.write_all(&bytes)).await, Ok(Ok(()))) {
        let _ = child.start_kill();
        return None;
    }
    // The notice needs no more input; EOF does not end it early.
    drop(stdin);
    Some(child)
}

fn spawn(config: &std::path::Path) -> Result<Child> {
    if crate::gsettings::GSettings::beside(config).get("enabled")? != "true" {
        bail!("overlay disabled");
    }
    let executable = std::env::current_exe()?.with_file_name("xflow");
    Command::new(executable)
        .arg("--config")
        .arg(config)
        .arg("overlay-pill")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("cannot start the recording pill")
}

async fn send(child: &mut Child, event: PillEvent) -> Result<()> {
    let mut bytes = serde_json::to_vec(&event)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_EVENT_BYTES {
        bail!("pill event exceeds limit");
    }
    let stdin = child.stdin.as_mut().context("pill input closed")?;
    timeout(WRITE_TIMEOUT, stdin.write_all(&bytes)).await??;
    Ok(())
}

/// `notice`: the pill holds a failure alert before it fades.
async fn close(helper: &mut Option<Child>, notice: bool) {
    if let Some(mut child) = helper.take() {
        drop(child.stdin.take());
        if timeout(
            Duration::from_secs(if notice { 4 } else { 1 }),
            child.wait(),
        )
        .await
        .is_err()
        {
            let _ = child.kill().await;
        }
    }
}

fn read_line(reader: &mut impl BufRead, limit: usize) -> Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader.take((limit + 1) as u64).read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > limit || bytes.last() != Some(&b'\n') {
        bail!("invalid pill event frame");
    }
    Ok(Some(bytes))
}

fn pill_event(bytes: &[u8]) -> Result<PillEvent> {
    let event: PillEvent = serde_json::from_slice(bytes)?;
    if !event.level.is_finite() || !(0.0..=1.0).contains(&event.level) {
        bail!("invalid pill audio level");
    }
    Ok(event)
}

fn read_event(reader: &mut impl BufRead) -> Result<Option<PillEvent>> {
    read_line(reader, MAX_EVENT_BYTES)?
        .map(|bytes| pill_event(&bytes))
        .transpose()
}

/// A helper's first frame: a recording event, or a learning notice.
enum First {
    Pill(PillEvent),
    Notice(Notice),
}
fn read_first(reader: &mut impl BufRead) -> Result<Option<First>> {
    let Some(bytes) = read_line(reader, MAX_NOTICE_BYTES)? else {
        return Ok(None);
    };
    if let Ok(frame) = serde_json::from_slice::<NoticeFrame>(&bytes) {
        if !frame.notice.valid() {
            bail!("invalid learning notice");
        }
        return Ok(Some(First::Notice(frame.notice)));
    }
    if bytes.len() > MAX_EVENT_BYTES {
        bail!("invalid pill event frame");
    }
    pill_event(&bytes).map(|event| Some(First::Pill(event)))
}

/// Hidden CLI entry point; must run on the process main thread.
pub fn helper(config: &std::path::Path) -> Result<()> {
    let mut input = std::io::BufReader::new(std::io::stdin());
    let Some(first) = read_first(&mut input)? else {
        return Ok(());
    };
    if let First::Pill(event) = &first {
        if !event.active() && event.alert.is_none() {
            return Ok(());
        }
    }
    let settings = crate::gsettings::GSettings::beside(config);
    if settings.get("enabled")? != "true" {
        return Ok(());
    }
    match first {
        First::Pill(event) => native::run(event, input, settings),
        First::Notice(notice) => native::notice(notice, settings),
    }
}

/// Hidden `xflow overlay preview`: cycles listening, hands-free (then no signal)
/// and a failure alert without dictating. `notice`: the one-second learning
/// notice instead, for an example word (its Undo is refused: no such change);
/// with XFLOW_PILL_TIMING set, the helper prints how long it was visible.
pub fn preview(notice: bool) -> Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("overlay-pill")
        .stdin(Stdio::piped())
        .spawn()
        .context("cannot start the recording pill")?;
    let mut stdin = child.stdin.take().context("pill input closed")?;
    if notice {
        let frame = NoticeFrame {
            notice: Notice {
                op: 1,
                words: vec!["claud".into()],
                correction: false,
            },
        };
        writeln!(stdin, "{}", serde_json::to_string(&frame)?)?;
        drop(stdin);
        child.wait()?;
        return Ok(());
    }
    for i in 0..=175 {
        let t = i as f32 * 0.04; // 25 Hz, like the daemon's level tick
        let level = 0.01 + 0.2 * ((t * 7.0).sin() * (t * 1.7).sin()).abs();
        let event = PillEvent {
            state: if i < 175 {
                State::Listening
            } else {
                State::Error
            },
            level,
            hands_free: i >= 100,
            no_signal: (150..175).contains(&i),
            alert: (i == 175).then_some(Alert::Transcribe),
            ..Default::default()
        };
        writeln!(stdin, "{}", serde_json::to_string(&event)?)?;
        std::thread::sleep(Duration::from_millis(40));
    }
    drop(stdin);
    child.wait()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_helper_has_bounded_writes_and_is_reaped() {
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let event = PillEvent {
            state: State::Listening,
            level: 0.5,
            ..Default::default()
        };
        let mut stalled = false;
        for _ in 0..4096 {
            if send(&mut child, event).await.is_err() {
                stalled = true;
                break;
            }
        }
        let mut helper = Some(child);
        timeout(Duration::from_secs(2), close(&mut helper, false))
            .await
            .unwrap();
        assert!(stalled, "an unread pipe must stop accepting events");
        assert!(helper.is_none());
    }

    #[test]
    fn frames_are_bounded_and_levels_validated() {
        for frame in [
            b"{\"state\":\"listening\",\"level\":0.5}\n".as_slice(),
            b"{\"state\":\"processing\",\"level\":0}\n",
        ] {
            assert!(read_event(&mut std::io::Cursor::new(frame))
                .unwrap()
                .unwrap()
                .active());
        }
        for frame in [
            vec![b'x'; MAX_EVENT_BYTES + 1],
            b"{\"state\":\"listening\",\"level\":2}\n".to_vec(),
            b"{\"state\":\"idle\",\"level\":0}".to_vec(),
        ] {
            assert!(read_event(&mut std::io::Cursor::new(frame)).is_err());
        }
        let event = PillEvent::from_desktop(
            DesktopEvent {
                state: State::Listening,
                level: f32::NAN,
                message: Some("private".into()),
                mode: Default::default(),
            },
            false,
        );
        let encoded = serde_json::to_string(&event).unwrap();
        assert_eq!(event.level, 0.0);
        assert!(!encoded.contains("private"));
        assert!(encoded.len() < MAX_EVENT_BYTES);
        let alert = |state, message: &str, processed| {
            PillEvent::from_desktop(
                DesktopEvent {
                    state,
                    level: 0.0,
                    message: Some(message.into()),
                    mode: Default::default(),
                },
                processed,
            )
            .alert
        };
        use crate::daemon::{COPIED, DISCARDED, INJECTION_FAILED, UNCERTAIN};
        // Silent after release: pasted, uncertain paste, warnings, discard, too short.
        for (state, message) in [
            (
                State::Success,
                "Cleanup failed; original transcript retained",
            ),
            (State::Success, UNCERTAIN),
            (State::Idle, DISCARDED),
            (State::Idle, "Recording too short; discarded"),
            (State::Processing, ""),
        ] {
            assert_eq!(alert(state, message, true), None, "{message}");
        }
        let copied = format!("{COPIED}; Terminal paste capability unverified");
        assert_eq!(alert(State::Success, &copied, true), Some(Alert::Copied));
        assert_eq!(
            alert(State::Idle, crate::speech::NOT_CAUGHT, true),
            Some(Alert::NoSpeech)
        );
        let paste = format!("{INJECTION_FAILED}: denied; transcript available via last");
        assert_eq!(alert(State::Error, &paste, true), Some(Alert::Paste));
        assert_eq!(alert(State::Error, "401", true), Some(Alert::Transcribe));
        assert_eq!(
            alert(State::Error, "device lost", false),
            Some(Alert::Record)
        );
        let frame = PillEvent {
            state: State::Error,
            alert: Some(Alert::Transcribe),
            ..Default::default()
        };
        let frame = format!("{}\n", serde_json::to_string(&frame).unwrap());
        assert!(frame.len() < MAX_EVENT_BYTES);
        let read = read_event(&mut std::io::Cursor::new(frame))
            .unwrap()
            .unwrap();
        assert_eq!(read.alert, Some(Alert::Transcribe));
    }

    fn notice(op: u64) -> Notice {
        Notice {
            op,
            words: vec!["claud".into()],
            correction: false,
        }
    }

    #[test]
    fn notice_waits_for_recording_and_shows_once_while_current() {
        let mut notices = Notices::default();
        // Idle: shown at once, and never again for the same change.
        assert_eq!(
            notices.step(Step::Latest(Some(notice(1)), false)),
            [Action::Show(notice(1))]
        );
        assert!(notices.step(Step::Exited).is_empty());
        assert!(notices.step(Step::Latest(Some(notice(1)), false)).is_empty());
        // Saved during a session (often finalized by the next dictation): it
        // waits through the 25 Hz level frames, which are not session starts,
        // and shows once the pill is idle.
        assert!(notices.step(Step::Session).is_empty());
        assert!(notices.step(Step::Latest(Some(notice(2)), true)).is_empty());
        assert_eq!(notices.pending, Some(notice(2)));
        assert_eq!(
            notices.step(Step::Latest(Some(notice(2)), false)),
            [Action::Show(notice(2))]
        );
        // A new session removes a visible notice at once and drops a waiting one.
        assert_eq!(notices.step(Step::Session), [Action::Kill]);
        assert!(notices.step(Step::Latest(Some(notice(3)), true)).is_empty());
        assert!(notices.step(Step::Session).is_empty());
        assert!(notices.step(Step::Latest(Some(notice(3)), false)).is_empty());
        // A newer change replaces the visible notice; one that can no longer be
        // undone takes it away, and a waiting one is forgotten.
        assert_eq!(
            notices.step(Step::Latest(Some(notice(4)), false)),
            [Action::Show(notice(4))]
        );
        assert_eq!(
            notices.step(Step::Latest(Some(notice(5)), false)),
            [Action::Kill, Action::Show(notice(5))]
        );
        assert_eq!(notices.step(Step::Latest(None, false)), [Action::Kill]);
        assert!(notices.step(Step::Latest(Some(notice(6)), true)).is_empty());
        assert!(notices.step(Step::Latest(None, true)).is_empty());
        assert!(notices.step(Step::Latest(None, false)).is_empty());
    }

    #[test]
    fn notice_frames_are_bounded_validated_and_first_only() {
        let widest = Notice {
            op: u64::MAX,
            words: vec!["“é’\"".repeat(10); 4],
            correction: true,
        };
        assert!(widest.valid());
        let mut frame = serde_json::to_vec(&NoticeFrame {
            notice: widest.clone(),
        })
        .unwrap();
        frame.push(b'\n');
        assert!(frame.len() < MAX_NOTICE_BYTES, "{}", frame.len());
        assert!(matches!(
            read_first(&mut std::io::Cursor::new(frame)).unwrap(),
            Some(First::Notice(read)) if read == widest
        ));
        for words in [
            vec![],
            vec!["".to_owned()],
            vec!["x".repeat(41)],
            vec!["bell\u{7}".to_owned()],
            vec!["claud".to_owned(); 5],
        ] {
            let bad = Notice {
                op: 1,
                words,
                correction: false,
            };
            let frame = format!(
                "{}\n",
                serde_json::to_string(&NoticeFrame {
                    notice: bad.clone()
                })
                .unwrap()
            );
            assert!(read_first(&mut std::io::Cursor::new(frame)).is_err(), "{bad:?}");
        }
        let zero = "{\"notice\":{\"op\":0,\"words\":[\"claud\"]}}\n";
        assert!(read_first(&mut std::io::Cursor::new(zero)).is_err());
        // Only a first frame: later frames are recording events.
        let small = "{\"notice\":{\"op\":1,\"words\":[\"claud\"]}}\n";
        assert!(read_event(&mut std::io::Cursor::new(small)).is_err());
        // Recording frames are unchanged.
        let listening = b"{\"state\":\"listening\",\"level\":0.5}\n".as_slice();
        assert!(matches!(
            read_first(&mut std::io::Cursor::new(listening)).unwrap(),
            Some(First::Pill(event)) if event.active()
        ));
        // A word too long to show in full is not offered on the pill.
        let long = LearnNotice {
            op: 1,
            words: vec!["x".repeat(41)],
            correction: false,
        };
        assert_eq!(Notice::new(&long), None);
    }

    #[test]
    fn listening_hints_map_only_while_listening() {
        let event = |state, message: String| {
            PillEvent::from_desktop(
                DesktopEvent {
                    state,
                    level: 0.0,
                    message: Some(message),
                    mode: Default::default(),
                },
                false,
            )
        };
        use crate::daemon::{ENDS_SOON, HANDS_FREE, NO_SIGNAL};
        let quiet = event(State::Listening, format!("{HANDS_FREE}; {NO_SIGNAL}"));
        assert!(quiet.hands_free && quiet.no_signal && !quiet.ends_soon && quiet.active());
        let ending = event(State::Listening, ENDS_SOON.into());
        assert!(ending.ends_soon && !ending.no_signal && !ending.hands_free);
        assert!(!event(State::Listening, HANDS_FREE.into()).no_signal);
        let done = event(State::Success, NO_SIGNAL.into());
        assert!(!done.no_signal && !done.ends_soon);
        let all = PillEvent {
            state: State::Processing,
            level: 0.123_456_79,
            hands_free: true,
            no_signal: true,
            ends_soon: true,
            alert: Some(Alert::NoSpeech),
        };
        let frame = format!("{}\n", serde_json::to_string(&all).unwrap());
        let read = read_event(&mut std::io::Cursor::new(frame))
            .unwrap()
            .unwrap();
        assert!(read.no_signal && read.ends_soon);
    }
}
