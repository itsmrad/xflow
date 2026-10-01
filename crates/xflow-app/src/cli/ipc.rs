use super::{args, output::Output, Error, Result};
use anyhow::Context;
use std::{io::IsTerminal, time::Duration};
use tokio::io::BufReader;
use xflow_app::transport;
use xflow_core::{
    ipc::{Delivery, Request, Response, PROTOCOL_VERSION},
    Mode, State,
};

pub fn block_on<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(future)
}
pub fn request(request: &Request) -> Result<Response> {
    block_on(send(request))
}
pub async fn send(request: &Request) -> Result<Response> {
    let response = transport::request(request).await.map_err(|e| {
        Error::new(
            3,
            e.to_string(),
            "Run xflow daemon start, or start xflowd in a separate terminal",
        )
    })?;
    validate(&response, matches!(request, Request::Status))?;
    Ok(response)
}
fn validate(response: &Response, require_protocol: bool) -> Result<()> {
    if (require_protocol && response.protocol != Some(PROTOCOL_VERSION))
        || response.protocol.is_some_and(|p| p != PROTOCOL_VERSION)
    {
        return Err(Error::new(
            3,
            format!(
                "Daemon protocol {:?} does not match CLI protocol {PROTOCOL_VERSION}",
                response.protocol
            ),
            "Restart the daemon with xflow daemon restart",
        ));
    }
    if !response.ok {
        return Err(Error::new(
            1,
            response
                .message
                .as_deref()
                .unwrap_or("Daemon command failed"),
            "Run xflow status or xflow doctor for details",
        ));
    }
    Ok(())
}
pub fn mode_request(toggle: bool, command: bool, delivery: Delivery) -> Request {
    let mode = if command {
        Mode::Command
    } else {
        Mode::Dictation
    };
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let t0_us = if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } == 0 {
        Some(time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1000)
    } else {
        None
    };
    if toggle {
        Request::Toggle {
            mode,
            context: None,
            t0_us,
            delivery,
        }
    } else {
        Request::Start {
            mode,
            context: None,
            t0_us,
            delivery,
        }
    }
}
pub fn reload_if_running(path: &std::path::Path, out: &Output) -> Result<()> {
    let default = xflow_app::paths::config_path()?;
    let resolve = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    if resolve(path) != resolve(&default) {
        return Ok(());
    }
    let socket = xflow_app::paths::runtime_dir()?.join("daemon.sock");
    if !socket.exists() {
        return Ok(());
    }
    request(&Request::Reload).map_err(|e| {
        Error::new(
            e.code,
            "Configuration saved, but daemon reload failed",
            format!("{}; fix the daemon and run xflow daemon restart", e.message),
        )
    })?;
    if !out.quiet && !out.json {
        eprintln!("Daemon configuration reloaded");
    }
    Ok(())
}
pub async fn watch(out: &Output) -> Result<()> {
    let mut stream = transport::connect().await.map_err(Error::daemon)?;
    transport::write_frame(&mut stream, &Request::Subscribe).await?;
    let mut reader = BufReader::new(stream);
    let first: Response = transport::read_frame(&mut reader)
        .await?
        .context("Daemon closed subscription")?;
    validate(&first, true)?;
    out.response(&first)?;
    loop {
        tokio::select! {
            result = transport::read_frame::<Response, _>(&mut reader) => {
                let response = result?.context("Daemon disconnected")?;
                out.response(&response)?;
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
pub async fn listen(once: bool, seconds: Option<u64>, command: bool, out: &Output) -> Result<()> {
    let interactive = std::io::stdin().is_terminal();
    if seconds.is_none() && !interactive {
        return Err(Error::new(
            2,
            "listen needs a recording duration when stdin is not a terminal",
            "Pass --seconds N; use --once for a single transcript",
        ));
    }
    let mut stream = transport::connect().await.map_err(Error::daemon)?;
    transport::write_frame(&mut stream, &Request::Subscribe).await?;
    let mut reader = BufReader::new(stream);
    let snapshot: Response =
        tokio::time::timeout(Duration::from_secs(10), transport::read_frame(&mut reader))
            .await??
            .context("Daemon closed subscription")?;
    validate(&snapshot, true)?;
    if matches!(snapshot.state, State::Listening | State::Processing) {
        return Err(Error::new(
            1,
            "A dictation session is already active",
            "Finish it before running xflow listen",
        ));
    }
    let mut input = if seconds.is_none() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        // A detached stdin reader avoids a Tokio blocking task preventing Ctrl-C shutdown.
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                if line.is_err() || tx.blocking_send(()).is_err() {
                    break;
                }
            }
        });
        Some(rx)
    } else {
        None
    };
    loop {
        send(&mode_request(false, command, Delivery::None)).await?;
        if !out.quiet {
            eprintln!(
                "Listening{}",
                if seconds.is_none() {
                    " — press Enter to finish; Ctrl-C cancels"
                } else {
                    ""
                }
            );
        }
        let result = collect(&mut reader, &mut input, seconds, out).await;
        if result.is_err() {
            let _ = send(&Request::Cancel).await;
        }
        result?;
        if once || !interactive {
            return Ok(());
        }
    }
}
async fn collect(
    reader: &mut BufReader<tokio::net::UnixStream>,
    input: &mut Option<tokio::sync::mpsc::Receiver<()>>,
    seconds: Option<u64>,
    out: &Output,
) -> Result<()> {
    let stop = tokio::time::sleep(Duration::from_secs(seconds.unwrap_or(3600)));
    tokio::pin!(stop);
    let deadline = tokio::time::sleep(Duration::from_secs(4200));
    tokio::pin!(deadline);
    let mut stopped = false;
    loop {
        tokio::select! {
            response = transport::read_frame::<Response, _>(reader) => {
                let response = response?.context("Daemon disconnected before transcription completed")?;
                validate(&response, false)?;
                if response.state == State::Error { return Err(Error::new(1, response.message.unwrap_or_else(|| "Dictation failed".into()), "Check xflow doctor")); }
                if let Some(text) = &response.text { out.data(&response, text)?; return Ok(()); }
            }
            _ = &mut stop, if !stopped && seconds.is_some() => { send(&Request::Stop).await?; stopped = true; }
            value = async { match input { Some(rx) => rx.recv().await, None => std::future::pending().await } }, if !stopped && seconds.is_none() => {
                if value.is_none() { return Err(Error::new(1, "Input closed before recording finished", "Use --seconds N for scripting")); }
                send(&Request::Stop).await?; stopped = true;
            }
            _ = tokio::signal::ctrl_c() => return Err(Error::new(130, "Dictation cancelled", "The active recording was discarded")),
            _ = &mut deadline => return Err(Error::new(1, "Dictation timed out", "Check provider timeouts and xflow doctor")),
        }
    }
}
pub fn history(page: &args::Page, action: Option<&args::History>, out: &Output) -> Result<()> {
    use args::History::*;
    let list = |limit, offset, query| {
        request(&Request::History {
            limit,
            offset,
            query,
        })
    };
    let response = match action {
        None => list(page.limit as usize, page.offset, page.query.clone())?,
        Some(List(page)) => list(page.limit as usize, page.offset, page.query.clone())?,
        Some(Search {
            query,
            limit,
            offset,
        }) => list(*limit as usize, *offset, Some(query.clone()))?,
        Some(Show { id }) => request(&Request::HistoryGet { id: positive(*id)? })?,
        Some(Copy { id }) => request(&Request::HistoryCopy { id: positive(*id)? })?,
        Some(Paste { id }) => request(&Request::HistoryPaste { id: positive(*id)? })?,
        Some(Delete { id }) => request(&Request::HistoryDelete { id: positive(*id)? })?,
        Some(Clear { yes }) => {
            super::config::confirm(*yes, "Delete all dictation history?")?;
            request(&Request::ClearHistory)?
        }
        Some(Export { format, query }) => {
            let mut entries = Vec::new();
            loop {
                let response = list(1000, entries.len(), query.clone())?;
                let empty = response.history.is_empty();
                let total = response.total;
                entries.extend(response.history);
                if empty || total.is_some_and(|n| entries.len() as u64 >= n) {
                    break;
                }
                if entries.len() > 100_000 {
                    return Err(Error::new(
                        1,
                        "History export exceeded its bound",
                        "Export a narrower search",
                    ));
                }
            }
            match if out.json {
                args::Format::Json
            } else {
                *format
            } {
                args::Format::Json => out.text(&serde_json::to_string(&entries)?)?,
                args::Format::Csv => out.text(&csv(&entries))?,
                args::Format::Md => {
                    let mut text =
                        "| ID | Time (Unix) | Provider | Text |\n| --- | --- | --- | --- |"
                            .to_string();
                    for e in entries {
                        text.push_str(&format!(
                            "\n| {} | {} | {} | {} |",
                            e.id,
                            e.created_at,
                            markdown(&e.provider),
                            markdown(&e.text)
                        ));
                    }
                    out.text(&text)?;
                }
            }
            return Ok(());
        }
    };
    out.response(&response)?;
    Ok(())
}
fn positive(id: i64) -> Result<i64> {
    if id > 0 {
        Ok(id)
    } else {
        Err(Error::new(
            2,
            "History ID must be positive",
            "Run xflow history to find an ID",
        ))
    }
}
fn markdown(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', "<br>")
        .replace('\r', "")
}
fn csv(entries: &[xflow_core::ipc::HistoryEntry]) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut text = "id,created_at,provider,model,text\r\n".to_owned();
    for e in entries {
        text.push_str(&format!(
            "{},{},{},{},{}\r\n",
            e.id,
            e.created_at,
            quote(&e.provider),
            quote(e.model.as_deref().unwrap_or("")),
            quote(&e.text)
        ));
    }
    text.pop();
    text.pop();
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_mismatch_and_failed_replies_are_errors() {
        let mut response = Response::status(State::Idle, 0.0);
        assert_eq!(validate(&response, true).unwrap_err().code, 3);
        response.protocol = Some(PROTOCOL_VERSION);
        assert!(validate(&response, true).is_ok());
        assert_eq!(
            validate(&Response::error(State::Idle, "bad request"), false)
                .unwrap_err()
                .code,
            1
        );
        assert!(matches!(
            mode_request(false, true, Delivery::None),
            Request::Start {
                mode: Mode::Command,
                delivery: Delivery::None,
                ..
            }
        ));
    }
    #[test]
    fn csv_quotes_multiline_transcripts() {
        assert_eq!(
            csv(&[xflow_core::ipc::HistoryEntry {
                id: 7,
                text: "hello, \"you\"\nagain".into(),
                ..Default::default()
            }]),
            "id,created_at,provider,model,text\r\n7,0,\"\",\"\",\"hello, \"\"you\"\"\nagain\""
        );
    }
}
