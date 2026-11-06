//! The daemon owns only a control pipe. Detection, window inspection and prompts
//! live in the client; closing the pipe ends it and gracefully stops its recorder.
use std::{path::PathBuf, process::Stdio};
use tokio::{process::Command, sync::watch};

pub async fn run(config: PathBuf, mut enabled: watch::Receiver<bool>) {
    let mut child = None;
    loop {
        if *enabled.borrow_and_update() && child.is_none() {
            child = std::env::current_exe().ok().and_then(|exe| {
                Command::new(exe.with_file_name("xflow"))
                    .arg("--config")
                    .arg(&config)
                    .arg("meeting-watch")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .ok()
            });
        }
        if enabled.changed().await.is_err() || !*enabled.borrow() {
            if let Some(mut child) = child.take() {
                // EOF, never a kill: the recorder may still be flushing saved audio.
                drop(child.stdin.take());
                tokio::spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
        if enabled.has_changed().is_err() {
            break;
        }
    }
}
