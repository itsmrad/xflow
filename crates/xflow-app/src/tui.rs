use crate::transport::{connect, read_frame, request, write_frame};
use anyhow::Result;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    widgets::{Block, Gauge, Paragraph},
    Terminal,
};
use tokio::io::BufReader;
use xflow_core::ipc::{Request, Response};

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}
pub async fn run() -> Result<()> {
    let mut stream = connect().await?;
    write_frame(&mut stream, &Request::Subscribe).await?;
    let mut reader = BufReader::new(stream);
    let mut status = read_frame::<Response, _>(&mut reader)
        .await?
        .ok_or_else(|| anyhow::anyhow!("daemon disconnected"))?;
    enable_raw_mode()?;
    let _restore = Restore;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    // Keep framing outside select: canceling read_line midway can lose bytes.
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(16);
    let reader_task = tokio::spawn(async move {
        loop {
            let event = read_frame::<Response, _>(&mut reader).await;
            let finished = !matches!(&event, Ok(Some(_)));
            if events_tx.send(event).await.is_err() || finished {
                break;
            }
        }
    });
    struct ReaderGuard(tokio::task::JoinHandle<()>);
    impl Drop for ReaderGuard {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _reader_guard = ReaderGuard(reader_task);
    let mut input = EventStream::new();
    let mut transcript = String::new();
    loop {
        terminal.draw(|frame| {
            let rows = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(frame.area());
            frame.render_widget(
                Paragraph::new(format!(
                    "{:?}  {}",
                    status.state,
                    status.message.as_deref().unwrap_or("")
                ))
                .block(Block::bordered().title("xflow")),
                rows[0],
            );
            frame.render_widget(
                Gauge::default()
                    .block(Block::bordered().title("Microphone"))
                    .ratio(status.level.clamp(0.0, 1.0) as f64),
                rows[1],
            );
            frame.render_widget(
                Paragraph::new(transcript.as_str())
                    .block(Block::bordered().title("Last transcript (l to load)")),
                rows[2],
            );
            frame.render_widget(
                Paragraph::new("Space toggle • Esc cancel • l last • c copy • p paste • q quit"),
                rows[3],
            );
        })?;
        tokio::select! {
            response = events_rx.recv() => {
                match response { Some(Ok(Some(response))) => status = response, Some(Err(error)) => return Err(error), _ => break }
            }
            event = input.next() => {
                let Some(event) = event else { break; };
                match event? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        let command = match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char(' ') => Some(Request::toggle()), KeyCode::Esc => Some(Request::Cancel),
                            KeyCode::Char('l') => Some(Request::Last), KeyCode::Char('c') => Some(Request::CopyLast), KeyCode::Char('p') => Some(Request::PasteLast),
                            _ => None,
                        };
                        if let Some(command) = command {
                            let response = request(&command).await?;
                            if let Some(text) = &response.text { transcript = text.clone(); }
                            status = response;
                        }
                    }
                    Event::Resize(_, _) => (),
                    _ => continue,
                }
            }
        }
    }
    Ok(())
}
