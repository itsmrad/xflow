//! The daemon only forwards small events; AppKit is loaded by `xflow overlay-pill`.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{io::BufRead, process::Stdio, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
    sync::broadcast,
    time::timeout,
};
use xflow_core::{DesktopEvent, State};

mod native;

const MAX_EVENT_BYTES: usize = 192;
const WRITE_TIMEOUT: Duration = Duration::from_millis(100);

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
/// session's failure brings it back, briefly.
pub async fn run(mut events: broadcast::Receiver<DesktopEvent>, config: std::path::PathBuf) {
    let mut helper: Option<Child> = None;
    // None outside a session; Some(true) once it reached Processing.
    let mut session: Option<bool> = None;
    loop {
        let event = match events.recv().await {
            Ok(event) => PillEvent::from_desktop(event, session == Some(true)),
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        };
        match event.state {
            State::Listening => {
                if session.is_none() {
                    session = Some(false);
                    // Retry only on the next session, not on every microphone-level event.
                    helper = spawn(&config).ok();
                }
                if let Some(child) = &mut helper {
                    if send(child, event).await.is_err() {
                        close(&mut helper, false).await;
                    }
                }
            }
            State::Processing => {
                session = Some(true);
                close(&mut helper, false).await;
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
            }
        }
    }
    close(&mut helper, false).await;
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

fn read_event(reader: &mut impl BufRead) -> Result<Option<PillEvent>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader
        .take((MAX_EVENT_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > MAX_EVENT_BYTES || bytes.last() != Some(&b'\n') {
        bail!("invalid pill event frame");
    }
    let event: PillEvent = serde_json::from_slice(&bytes)?;
    if !event.level.is_finite() || !(0.0..=1.0).contains(&event.level) {
        bail!("invalid pill audio level");
    }
    Ok(Some(event))
}

/// Hidden CLI entry point; must run on the process main thread.
pub fn helper(config: &std::path::Path) -> Result<()> {
    let mut input = std::io::BufReader::new(std::io::stdin());
    let Some(first) = read_event(&mut input)? else {
        return Ok(());
    };
    if !first.active() && first.alert.is_none() {
        return Ok(());
    }
    let settings = crate::gsettings::GSettings::beside(config);
    if settings.get("enabled")? != "true" {
        return Ok(());
    }
    native::run(first, input, settings)
}

/// Hidden `xflow overlay preview`: cycles listening, hands-free (then no signal)
/// and a failure alert without dictating.
pub fn preview() -> Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("overlay-pill")
        .stdin(Stdio::piped())
        .spawn()
        .context("cannot start the recording pill")?;
    let mut stdin = child.stdin.take().context("pill input closed")?;
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
