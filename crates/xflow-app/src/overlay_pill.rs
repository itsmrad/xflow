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

const MAX_EVENT_BYTES: usize = 128;
const WRITE_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PillEvent {
    state: State,
    level: f32,
    #[serde(default)]
    hands_free: bool,
}

impl PillEvent {
    fn active(self) -> bool {
        matches!(self.state, State::Listening | State::Processing)
    }

    fn from_desktop(event: DesktopEvent) -> Self {
        Self {
            state: event.state,
            level: if event.level.is_finite() {
                event.level.clamp(0.0, 1.0)
            } else {
                0.0
            },
            hands_free: event
                .message
                .is_some_and(|m| m.starts_with(crate::daemon::HANDS_FREE)),
        }
    }
}

/// Spawn this future with a dedicated desktop-event subscription. Failures are
/// deliberately isolated from recording; no helper, timer or AppKit exists at idle.
pub async fn run(mut events: broadcast::Receiver<DesktopEvent>) {
    let mut helper: Option<Child> = None;
    let mut session_active = false;
    loop {
        let event = match events.recv().await {
            Ok(event) => PillEvent::from_desktop(event),
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        };
        if !event.active() {
            // The final success/error frame lets the pill play its exit animation.
            if let Some(child) = &mut helper {
                let _ = send(child, event).await;
            }
            close(&mut helper).await;
            session_active = false;
            continue;
        }
        if !session_active {
            session_active = true;
            // Retry only on the next session, not on every microphone-level event.
            helper = spawn().ok();
        }
        if let Some(child) = &mut helper {
            if send(child, event).await.is_err() {
                close(&mut helper).await;
            }
        }
    }
    close(&mut helper).await;
}

fn spawn() -> Result<Child> {
    if crate::gsettings::GSettings::detect()?.get("enabled")? != "true" {
        bail!("overlay disabled");
    }
    let executable = std::env::current_exe()?.with_file_name("xflow");
    Command::new(executable)
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

async fn close(helper: &mut Option<Child>) {
    if let Some(mut child) = helper.take() {
        drop(child.stdin.take());
        if timeout(Duration::from_secs(1), child.wait()).await.is_err() {
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
pub fn helper() -> Result<()> {
    let mut input = std::io::BufReader::new(std::io::stdin());
    let Some(first) = read_event(&mut input)? else {
        return Ok(());
    };
    if !first.active() {
        return Ok(());
    }
    let settings = crate::gsettings::GSettings::detect()?;
    if settings.get("enabled")? != "true" {
        return Ok(());
    }
    native::run(first, input, settings)
}

/// Hidden `xflow overlay preview`: cycles listening, hands-free, processing
/// and error without dictating.
pub fn preview() -> Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("overlay-pill")
        .stdin(Stdio::piped())
        .spawn()
        .context("cannot start the recording pill")?;
    let mut stdin = child.stdin.take().context("pill input closed")?;
    for i in 0..=250 {
        let t = i as f32 * 0.04; // 25 Hz, like the daemon's level tick
        let (state, hands_free) = match i {
            0..=99 => (State::Listening, false),
            100..=174 => (State::Listening, true),
            175..=249 => (State::Processing, false),
            _ => (State::Error, false),
        };
        let level = 0.01 + 0.2 * ((t * 7.0).sin() * (t * 1.7).sin()).abs();
        let event = PillEvent {
            state,
            level,
            hands_free,
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
            hands_free: false,
        };
        let mut stalled = false;
        for _ in 0..4096 {
            if send(&mut child, event).await.is_err() {
                stalled = true;
                break;
            }
        }
        let mut helper = Some(child);
        timeout(Duration::from_secs(2), close(&mut helper))
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
        let event = PillEvent::from_desktop(DesktopEvent {
            state: State::Listening,
            level: f32::NAN,
            message: Some("private".into()),
            mode: Default::default(),
        });
        let encoded = serde_json::to_string(&event).unwrap();
        assert_eq!(event.level, 0.0);
        assert!(!encoded.contains("private"));
        assert!(encoded.len() < MAX_EVENT_BYTES);
    }
}
