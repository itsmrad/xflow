//! Consent and recorder lifecycle stay out of xflowd. Titles/URLs never leave
//! the observer; the UI receives only the adapter's name and parent bundle.
use super::Result;
use std::path::Path;

pub fn run(config: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        super::ipc::block_on(mac::watch(config))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        Err(anyhow::anyhow!("meeting detection requires macOS 14.2 or newer").into())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use anyhow::Context;
    use std::{process::Stdio, time::Duration};
    use tokio::{
        io::AsyncReadExt,
        process::{Child, Command},
    };
    use xflow_core::{
        config::Config,
        meeting_detect::{Decision, Event, MeetingId},
    };

    #[derive(Clone, Copy)]
    enum Question {
        Start,
        Stop,
    }
    struct Prompt {
        id: MeetingId,
        question: Question,
        event: Event,
        child: Child,
    }
    struct Recording {
        id: MeetingId,
        child: Child,
    }

    fn prompt(event: Event, question: Question) -> anyhow::Result<Prompt> {
        // Only static adapter names enter this script, never window text or URLs.
        let script = match question {
            Question::Start => format!(
                "display dialog \"Turn on Xflow for this meeting?\\n{} call detected. Yes opens recording consent and privacy details.\" with title \"Xflow\" buttons {{\"Turn off prompts\", \"Not now\", \"Yes\"}} default button \"Not now\" giving up after 30", event.app),
            Question::Stop => "display dialog \"Still recording. Stop?\\nThe meeting microphone is inactive. Xflow will keep recording unless you stop it.\" with title \"Xflow\" buttons {\"Keep recording\", \"Stop\"} default button \"Keep recording\" giving up after 15".into(),
        };
        Ok(Prompt {
            id: event.meeting,
            question,
            event,
            child: Command::new("/usr/bin/osascript")
                .args(["-e", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?,
        })
    }

    /// Recorder contract: --control-stdin EOF finalizes, even during its consent
    /// dialog. No audio before that dialog's Record; no automatic restart.
    fn start(event: Event, config: &Path) -> anyhow::Result<Recording> {
        let child = Command::new(std::env::current_exe()?)
            .arg("--config")
            .arg(config)
            .args([
                "meeting",
                "record",
                "--app",
                event.bundle,
                "--control-stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        Ok(Recording {
            id: event.meeting,
            child,
        })
    }
    fn stop(recording: &mut Recording) {
        drop(recording.child.stdin.take());
    }
    async fn dismiss(prompt: &mut Option<Prompt>) {
        if let Some(mut prompt) = prompt.take() {
            let _ = prompt.child.kill().await;
        }
    }
    async fn prompt_exited(
        prompt: &mut Option<Prompt>,
    ) -> std::io::Result<std::process::ExitStatus> {
        match prompt {
            Some(p) => p.child.wait().await,
            None => std::future::pending().await,
        }
    }
    async fn recorder_exited(
        recording: &mut Option<Recording>,
    ) -> std::io::Result<std::process::ExitStatus> {
        match recording {
            Some(r) => r.child.wait().await,
            None => std::future::pending().await,
        }
    }
    fn disable(config: &Path) -> anyhow::Result<()> {
        let _lock = xflow_app::config_edit::lock_within(config, Duration::from_millis(200))?;
        let mut file = xflow_app::config_edit::ConfigFile::load(config)?;
        file.set("meetings.detect", "false")?;
        file.save()
    }

    pub async fn watch(config: &Path) -> Result<()> {
        if !Config::load(config)?.meetings.detect {
            return Ok(());
        }
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let observer = xflow_platform::meeting::observe(move |event| {
            let _ = tx.send(event);
        })?;
        let (owner_tx, mut owner_gone) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0]);
            let _ = owner_tx.send(());
        });
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut prompt_open = None;
        let mut recording: Option<Recording> = None;
        loop {
            tokio::select! {
                _ = &mut owner_gone => break,
                _ = tokio::signal::ctrl_c() => break,
                _ = terminate.recv() => break,
                status = recorder_exited(&mut recording) => {
                    if !status?.success() { eprintln!("xflow: meeting recorder unavailable or failed; use xflow meeting record after installing recorder support"); }
                    if let Some(r) = &recording { observer.decline(r.id); }
                    recording = None;
                    dismiss(&mut prompt_open).await;
                }
                status = prompt_exited(&mut prompt_open) => {
                    let mut p = prompt_open.take().context("prompt vanished")?;
                    let mut answer = String::new();
                    if status?.success() {
                        if let Some(stdout) = p.child.stdout.take() { stdout.take(512).read_to_string(&mut answer).await?; }
                    }
                    match p.question {
                        Question::Start => {
                            if answer.contains("button returned:Yes") && recording.is_none() {
                                match start(p.event, config) {
                                    Ok(r) => recording = Some(r),
                                    Err(e) => { observer.decline(p.id); eprintln!("xflow: cannot start meeting recorder: {e}"); },
                                }
                            } else { observer.decline(p.id); }
                            if answer.contains("button returned:Turn off prompts") {
                                match disable(config) {
                                    Ok(()) => {
                                        let _ = super::super::ipc::send(&xflow_core::ipc::Request::Reload).await;
                                        break;
                                    }
                                    Err(e) => eprintln!("xflow: could not disable meeting prompts: {e}"),
                                }
                            }
                        }
                        Question::Stop => {
                            if answer.contains("button returned:Stop") {
                                if let Some(r) = &mut recording { stop(r); }
                            }
                        }
                    }
                }
                Some(event) = events.recv() => match event.decision {
                    Decision::Start => {
                        if prompt_open.is_none() && recording.is_none() {
                            match prompt(event, Question::Start) {
                                Ok(p) => prompt_open = Some(p),
                                Err(e) => { observer.decline(event.meeting); eprintln!("xflow: cannot show meeting prompt: {e}"); }
                            }
                        } else { observer.decline(event.meeting); }
                    }
                    Decision::MaybeEnded => {
                        if recording.as_ref().is_some_and(|r| r.id == event.meeting && r.child.stdin.is_some()) && prompt_open.is_none() {
                            prompt_open = prompt(event, Question::Stop).ok();
                        }
                    }
                    Decision::End => {
                        if prompt_open.as_ref().is_some_and(|p| p.id == event.meeting) { dismiss(&mut prompt_open).await; }
                        if let Some(r) = recording.as_mut().filter(|r| r.id == event.meeting) { stop(r); }
                    }
                }
            }
        }
        dismiss(&mut prompt_open).await;
        drop(observer);
        if let Some(mut r) = recording {
            stop(&mut r);
            let _ = r.child.wait().await;
        }
        Ok(())
    }
    #[tokio::test]
    async fn closing_control_pipe_allows_recorder_to_finalize() {
        use xflow_core::meeting_detect::{AppSeen, Detector, Window};
        let app = AppSeen::new(
            "us.zoom.xos",
            true,
            Some(vec![Window {
                title: "Zoom Meeting".into(),
                in_call: Some(true),
                ..Window::default()
            }]),
        );
        let mut detector = Detector::new();
        detector.update(0, std::slice::from_ref(&app));
        let id = detector.update(2000, &[app])[0].meeting;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("finished");
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                "cat >/dev/null; printf finalized > \"$1\"",
                "recorder-test",
            ])
            .arg(&marker)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut recording = Recording { id, child };
        stop(&mut recording);
        assert!(recording.child.wait().await.unwrap().success());
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "finalized");
    }
}
