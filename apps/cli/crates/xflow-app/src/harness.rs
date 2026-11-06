//! Installed CLIs belong to the user. Never inspect their authentication or config.
use async_trait::async_trait;
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use xflow_core::config::{AiConfig, CleanupConfig};
use xflow_providers::llm::{self, ChatProvider, ChatRequest, Event, LlmError, Message, Stop};

pub fn is_harness(id: &str) -> bool {
    matches!(id, "claude-code" | "codex" | "cursor")
}
fn executable(id: &str) -> Result<PathBuf, LlmError> {
    use std::os::unix::fs::PermissionsExt;
    let names: &[&str] = match id {
        "claude-code" => &["claude"],
        "codex" => &["codex"],
        _ => &["cursor-agent", "agent"],
    };
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for name in names {
            let path = dir.join(name);
            if path.is_absolute()
                && path
                    .metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            {
                return Ok(path);
            }
        }
    }
    Err(LlmError::NotInstalled)
}
fn invalid() -> LlmError {
    LlmError::Provider(
        "unsupported CLI response or configuration; update your CLI and retry".into(),
    )
}
fn classify(text: &str) -> LlmError {
    let text = text.to_ascii_lowercase();
    if text.contains("not logged in")
        || text.contains("authentication")
        || text.contains("login required")
    {
        LlmError::NotLoggedIn
    } else if text.contains("quota") || text.contains("credit") {
        LlmError::QuotaExhausted
    } else if text.contains("rate limit") || text.contains("rate_limit") {
        LlmError::RateLimit { retry_after: None }
    } else {
        invalid()
    }
}
// Keep CLI shutdown alive until every supervisor has killed/reaped its group.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Active;
impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}
pub async fn drain() {
    while ACTIVE.load(Ordering::SeqCst) != 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
// The supervisor owns the child until group cleanup and wait, including on future drop.
struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
async fn run(
    path: PathBuf,
    args: Vec<String>,
    input: Vec<u8>,
    deadline: Duration,
    tx: mpsc::Sender<String>,
) -> Result<(), LlmError> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let _guard = Cancel(cancelled.clone());
    ACTIVE.fetch_add(1, Ordering::SeqCst);
    let active = Active;
    let worker = tokio::task::spawn_blocking(move || {
        let _active = active;
        if cancelled.load(Ordering::Relaxed) {
            return Err(LlmError::Interrupted { partial: false });
        }
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().map_err(|_| invalid())?;
        let mut child = Command::new(path)
            .args(args)
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|_| invalid())?;
        let pid = child.id() as i32;
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (lines, rx) = std::sync::mpsc::sync_channel(8);
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let n = reader
                    .by_ref()
                    .take(1024 * 1024 + 1)
                    .read_until(b'\n', &mut line);
                match n {
                    Ok(0) => break,
                    Ok(_) if line.len() <= 1024 * 1024 && line.ends_with(b"\n") => {
                        if lines
                            .send(String::from_utf8(line).map_err(|_| invalid()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    _ => {
                        let _ = lines.send(Err(invalid()));
                        break;
                    }
                }
            }
        });
        let errors = std::thread::spawn(move || {
            let mut stderr = stderr;
            let mut saved = Vec::new();
            let mut buf = [0; 4096];
            while let Ok(n) = stderr.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let keep = n.min(65536usize.saturating_sub(saved.len()));
                saved.extend_from_slice(&buf[..keep]);
            }
            saved
        });
        let start = Instant::now();
        let mut last = start;
        let mut total = 0;
        let result = loop {
            if cancelled.load(Ordering::Relaxed) || tx.is_closed() {
                break Err(LlmError::Interrupted { partial: total > 0 });
            }
            if start.elapsed() > deadline
                || last.elapsed() > Duration::from_secs(if total == 0 { 30 } else { 45 })
            {
                break Err(LlmError::Timeout);
            }
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(Ok(line)) => {
                    total += line.len();
                    last = Instant::now();
                    if total > 32 * 1024 * 1024 {
                        break Err(invalid());
                    }
                    let mut value = line;
                    loop {
                        match tx.try_send(value) {
                            Ok(()) => break,
                            Err(mpsc::error::TrySendError::Full(v))
                                if !cancelled.load(Ordering::Relaxed)
                                    && start.elapsed() < deadline =>
                            {
                                value = v;
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(_) => break,
                        }
                    }
                }
                Ok(Err(e)) => break Err(e),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
                Err(_) => (),
            }
        };
        let mut result = result;
        // Wait for a normal exit without reaping, reserving the process-group ID.
        if result.is_ok() {
            loop {
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let rc = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pid as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                };
                if rc != 0 {
                    result = Err(invalid());
                    break;
                }
                if unsafe { info.si_pid() } == pid {
                    break;
                }
                if cancelled.load(Ordering::Relaxed) || start.elapsed() > deadline {
                    result = Err(LlmError::Timeout);
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Do not reap the group leader until KILL: its PID cannot be reused meanwhile.
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_secs(2));
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let status = child.wait().map_err(|_| invalid());
        drop(rx);
        let _ = writer.join();
        let _ = reader.join();
        let error = errors.join().unwrap_or_default();
        result?;
        if !status?.success() {
            return Err(classify(&String::from_utf8_lossy(&error)));
        }
        Ok(())
    });
    worker.await.map_err(|_| invalid())?
}
async fn probe(path: &std::path::Path, args: &[&str]) -> Result<String, LlmError> {
    let (tx, mut rx) = mpsc::channel::<String>(8);
    let collect = async {
        let mut out = String::new();
        while let Some(line) = rx.recv().await {
            if out.len() + line.len() > 128 * 1024 {
                return Err(invalid());
            }
            out.push_str(&line);
        }
        Ok(out)
    };
    let (done, out) = tokio::join!(
        run(
            path.to_owned(),
            args.iter().map(|s| s.to_string()).collect(),
            vec![],
            Duration::from_secs(5),
            tx
        ),
        collect
    );
    done?;
    out
}

pub struct Gateway {
    api: Option<llm::Gateway>,
    harness: Option<String>,
}
impl Gateway {
    pub fn overhead(&self) -> Duration {
        self.api.as_ref().map_or(Duration::ZERO, |g| g.overhead())
    }
}
pub fn build_provider(ai: &AiConfig, offline: bool) -> Result<Gateway, LlmError> {
    if is_harness(&ai.provider) {
        if offline {
            return Err(LlmError::Offline);
        }
        if ai.endpoint.is_some() {
            return Err(invalid());
        }
        return Ok(Gateway {
            api: None,
            harness: Some(ai.provider.clone()),
        });
    }
    llm::build_provider(ai, offline).map(|api| Gateway {
        api: Some(api),
        harness: None,
    })
}
pub fn build_provider_with_cleanup(
    ai: &AiConfig,
    cleanup: &CleanupConfig,
    offline: bool,
) -> Result<(AiConfig, Gateway), LlmError> {
    if is_harness(&ai.provider) {
        return build_provider(ai, offline).map(|g| (ai.clone(), g));
    }
    llm::build_provider_with_cleanup(ai, cleanup, offline).map(|(ai, api)| {
        (
            ai,
            Gateway {
                api: Some(api),
                harness: None,
            },
        )
    })
}
#[async_trait]
impl ChatProvider for Gateway {
    async fn warm(&self) -> Result<(), LlmError> {
        if let Some(api) = &self.api {
            api.warm().await
        } else {
            Ok(())
        }
    }
    async fn stream(
        &self,
        request: &ChatRequest,
        tx: &mpsc::Sender<Event>,
    ) -> Result<(), LlmError> {
        if let Some(api) = &self.api {
            return api.stream(request, tx).await;
        }
        let id = self.harness.as_deref().unwrap();
        let path = executable(id)?;
        if id != "claude-code" {
            return Err(LlmError::Provider("this CLI's stdin and host-access isolation are not verified; choose Claude Code or an API provider".into()));
        }
        let help = probe(&path, &["--help"]).await?;
        for flag in [
            "--safe-mode",
            "--tools",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--include-partial-messages",
        ] {
            if !help.contains(flag) {
                return Err(invalid());
            }
        }
        // Prefetch only: safe-mode disables customizations and MCP; no inherited tools.
        let mut args = vec![
            "-p",
            "--safe-mode",
            "--tools",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--verbose",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        if !request.model.is_empty() {
            args.extend(["--model".into(), request.model.clone()]);
        }
        let mut prompt = request.system.clone();
        for message in &request.messages {
            match message {
                Message::User(t) => {
                    prompt.push_str("\nUser:\n");
                    prompt.push_str(t);
                }
                Message::Assistant { text, .. } => {
                    prompt.push_str("\nAssistant:\n");
                    prompt.push_str(text);
                }
                Message::ToolResult { content, .. } => {
                    prompt.push('\n');
                    prompt.push_str(content);
                }
            }
        }
        let (lines, mut rx) = mpsc::channel::<String>(8);
        let process = run(
            path,
            args,
            prompt.into_bytes(),
            Duration::from_secs(300),
            lines,
        );
        let parse = async {
            let mut parser = Parser::default();
            while let Some(line) = rx.recv().await {
                if let Some(text) = parser.line(&line)? {
                    tx.send(Event::Text(text)).await.map_err(|_| invalid())?;
                }
            }
            if !parser.done {
                return Err(LlmError::Interrupted {
                    partial: parser.text,
                });
            }
            Ok(())
        };
        tokio::try_join!(process, parse)?;
        tx.send(Event::Done(Stop::End))
            .await
            .map_err(|_| invalid())?;
        Ok(())
    }
}
#[derive(Default)]
struct Parser {
    text: bool,
    done: bool,
}
impl Parser {
    fn line(&mut self, line: &str) -> Result<Option<String>, LlmError> {
        let v: serde_json::Value = serde_json::from_str(line).map_err(|_| invalid())?;
        match v["type"].as_str() {
            Some("stream_event") if v["event"]["delta"]["type"] == "text_delta" => {
                let t = v["event"]["delta"]["text"].as_str().ok_or_else(invalid)?;
                self.text |= !t.is_empty();
                Ok(Some(t.into()))
            }
            Some("result") => {
                self.done = true;
                if v["is_error"] == true || v["subtype"].as_str().is_some_and(|s| s != "success") {
                    return Err(classify(&v.to_string()));
                }
                if !self.text {
                    if let Some(t) = v["result"].as_str() {
                        self.text = !t.is_empty();
                        return Ok(Some(t.into()));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

/// A bounded status probe; no account identity or raw diagnostic reaches the UI.
pub async fn status(id: &str, offline: bool) -> String {
    if offline {
        return "Offline; CLI probes disabled".into();
    }
    let Ok(path) = executable(id) else {
        return "Not installed on PATH".into();
    };
    let args: &[&str] = match id {
        "claude-code" => &["auth", "status", "--json"],
        "codex" => &["login", "status"],
        _ => &["status"],
    };
    match probe(&path, args).await {
        Ok(out)
            if id == "claude-code"
                && serde_json::from_str::<serde_json::Value>(&out)
                    .ok()
                    .is_some_and(|v| v["loggedIn"] == false) =>
        {
            "Not logged in; use the CLI to log in".into()
        }
        Ok(_) if id == "claude-code" => "CLI login available; tools-off safe mode".into(),
        Ok(_) => "Installed; isolation unverified, launch refused".into(),
        Err(LlmError::NotLoggedIn) => "Not logged in; use the CLI to log in".into(),
        Err(_) => "Installed; login status unavailable".into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jsonl_deltas_final_errors_and_unknown_events() {
        let mut p = Parser::default();
        assert_eq!(p.line(r#"{"type":"future"}"#).unwrap(), None);
        assert_eq!(p.line(r#"{"type":"stream_event","event":{"delta":{"type":"text_delta","text":"नमस्ते 世界"}}}"#).unwrap().as_deref(), Some("नमस्ते 世界"));
        assert_eq!(
            p.line(r#"{"type":"result","subtype":"success","result":"नमस्ते 世界"}"#)
                .unwrap(),
            None
        );
        assert!(p.done);
        assert!(Parser::default().line("{").is_err());
        assert_eq!(
            Parser::default()
                .line(r#"{"type":"result","is_error":true,"result":"rate limit PRIVATE"}"#),
            Err(LlmError::RateLimit { retry_after: None })
        );
        assert!(!classify("PRIVATE").to_string().contains("PRIVATE"));
    }
    fn fixture(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        (dir, path)
    }
    #[tokio::test]
    async fn stdin_private_cwd_concurrent_stderr_and_split_unicode_lines() {
        let (_dir, path) = fixture("test $# = 0 || exit 7\ntest -z \"$(/bin/ls -A)\" || exit 8\n/bin/cat\ni=0; while [ $i -lt 10000 ]; do echo diagnostic >&2; i=$((i+1)); done\nprintf 'split'; printf ' line\\n'");
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let (result, lines) = tokio::join!(
            run(
                path,
                vec![],
                "नमस्ते 世界\n".as_bytes().to_vec(),
                Duration::from_secs(10),
                tx
            ),
            async {
                let mut out = String::new();
                while let Some(s) = rx.recv().await {
                    out.push_str(&s);
                }
                out
            }
        );
        result.unwrap();
        assert_eq!(lines, "नमस्ते 世界\nsplit line\n");
    }
    #[tokio::test]
    async fn malformed_oversized_and_nonzero_are_bounded_and_sanitized() {
        for script in [
            "printf truncated",
            "echo private >&2; exit 7",
            " /usr/bin/head -c 1048577 /dev/zero",
        ] {
            let (_dir, path) = fixture(script);
            let (tx, mut rx) = mpsc::channel::<String>(8);
            let (result, ()) = tokio::join!(
                run(path, vec![], vec![], Duration::from_secs(5), tx),
                async { while rx.recv().await.is_some() {} }
            );
            assert!(result.is_err());
            assert!(!result.unwrap_err().to_string().contains("private"));
        }
    }
    #[tokio::test]
    async fn dropping_future_kills_term_ignoring_group_and_reaps_leader() {
        let (_dir, path) = fixture("trap '' TERM\n/bin/sleep 90 &\necho \"$$ $!\"\nwait");
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let task = tokio::spawn(run(path, vec![], vec![], Duration::from_secs(20), tx));
        let line = rx.recv().await.unwrap();
        let pids: Vec<i32> = line
            .split_whitespace()
            .map(|p| p.parse().unwrap())
            .collect();
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(2300)).await;
        // The leader has been waited, while Linux may briefly retain a grandchild zombie.
        assert_eq!(unsafe { libc::kill(pids[0], 0) }, -1);
        #[cfg(target_os = "linux")]
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pids[1])) {
            assert_eq!(stat.split_whitespace().nth(2), Some("Z"));
        }
        #[cfg(not(target_os = "linux"))]
        assert_eq!(unsafe { libc::kill(pids[1], 0) }, -1);
    }
    #[test]
    fn offline_refuses_before_executable_detection() {
        for id in ["claude-code", "codex", "cursor"] {
            assert!(matches!(
                build_provider(
                    &AiConfig {
                        provider: id.into(),
                        ..AiConfig::default()
                    },
                    true
                ),
                Err(LlmError::Offline)
            ));
        }
    }
}
