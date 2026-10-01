use super::args::Color;
use anyhow::Result;
use serde::Serialize;
use std::io::{self, IsTerminal, Write};
use xflow_core::ipc::Response;

pub struct Output {
    pub json: bool,
    pub quiet: bool,
    pub color: bool,
}
impl Output {
    pub fn new(json: bool, quiet: bool, color: Color) -> Self {
        let color = match color {
            Color::Always => true,
            Color::Never => false,
            Color::Auto => {
                std::env::var_os("NO_COLOR").is_none()
                    && (std::env::var("CLICOLOR_FORCE").is_ok_and(|v| !v.is_empty() && v != "0")
                        || io::stdout().is_terminal())
            }
        };
        Self {
            json,
            quiet,
            color: color && !json,
        }
    }
    pub fn text(&self, text: &str) -> Result<()> {
        writeln!(io::stdout().lock(), "{text}")?;
        Ok(())
    }
    pub fn data(&self, value: &impl Serialize, human: &str) -> Result<()> {
        if self.json {
            self.text(&serde_json::to_string(value)?)
        } else {
            self.text(human)
        }
    }
    pub fn confirm(&self, message: &str) -> Result<()> {
        if self.json {
            self.data(&serde_json::json!({"ok":true,"message":message}), "")
        } else if !self.quiet {
            self.text(&clean(message))
        } else {
            Ok(())
        }
    }
    pub fn table(&self, headers: &[&str], rows: &[Vec<String>]) -> Result<()> {
        let text = table(headers, rows);
        if self.color {
            let (head, tail) = text.split_once('\n').unwrap_or((&text, ""));
            self.text(&format!("\x1b[1;37m{head}\x1b[0m\n{tail}"))
        } else {
            self.text(&text)
        }
    }
    pub fn response(&self, response: &Response) -> Result<()> {
        if self.json {
            return self.text(&serde_json::to_string(response)?);
        }
        if let Some(text) = &response.text {
            return self.text(text);
        }
        if let Some(entry) = &response.entry {
            return self.text(&entry.text);
        }
        if let Some(stats) = &response.stats {
            return self.table(
                &["METRIC", "VALUE"],
                &[
                    vec!["Sessions".into(), stats.sessions.to_string()],
                    vec!["Words".into(), stats.words.to_string()],
                    vec!["Audio (ms)".into(), stats.audio_ms.to_string()],
                    vec!["Sessions today".into(), stats.sessions_today.to_string()],
                    vec!["Words today".into(), stats.words_today.to_string()],
                    vec!["Streak (days)".into(), stats.streak_days.to_string()],
                    vec![
                        "Words/min".into(),
                        stats
                            .wpm
                            .map(|v| format!("{v:.1}"))
                            .unwrap_or_else(|| "-".into()),
                    ],
                    vec![
                        "Latency p50/p95 (ms)".into(),
                        format!(
                            "{} / {}",
                            optional(stats.latency_p50_ms),
                            optional(stats.latency_p95_ms)
                        ),
                    ],
                ],
            );
        }
        if response.total.is_some() || !response.history.is_empty() {
            self.table(
                &["ID", "TIME (UNIX)", "PROVIDER", "TEXT"],
                &response
                    .history
                    .iter()
                    .map(|e| {
                        vec![
                            e.id.to_string(),
                            e.created_at.to_string(),
                            e.provider.clone(),
                            e.text.clone(),
                        ]
                    })
                    .collect::<Vec<_>>(),
            )?;
            if !self.quiet {
                self.text(&format!(
                    "{} matching entries",
                    response.total.unwrap_or(response.history.len() as u64)
                ))?;
            }
            return Ok(());
        }
        let mut rows = vec![vec![
            "State".into(),
            format!("{:?}", response.state).to_lowercase(),
        ]];
        for (name, value) in [
            ("Provider", &response.provider),
            ("Model", &response.model),
            ("Daemon", &response.version),
            ("Message", &response.message),
        ] {
            if let Some(value) = value {
                rows.push(vec![name.into(), value.clone()]);
            }
        }
        if let Some(protocol) = response.protocol {
            rows.push(vec!["Protocol".into(), protocol.to_string()]);
        }
        if let Some(timings) = &response.timings {
            rows.push(vec![
                "Last latency (ms)".into(),
                format!(
                    "{} total; {} STT; {} audio",
                    timings.total_ms, timings.stt_ms, timings.audio_ms
                ),
            ]);
        }
        self.table(&["FIELD", "VALUE"], &rows)
    }
}
fn optional(value: Option<u64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
}
/// Keep untrusted table cells from emitting terminal controls or forging rows.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| r.iter().map(|v| clean(v)).collect())
        .collect();
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .map(|v| v.chars().count())
                .chain([h.len()])
                .max()
                .unwrap_or(0)
                .min(80)
        })
        .collect();
    let render = |row: &[String]| {
        row.iter()
            .enumerate()
            .map(|(i, cell)| {
                if i + 1 == headers.len() {
                    return cell.clone();
                }
                let cell: String = cell.chars().take(widths[i]).collect();
                format!(
                    "{}{}",
                    cell,
                    " ".repeat(widths[i].saturating_sub(cell.chars().count()) + 2)
                )
            })
            .collect::<String>()
    };
    std::iter::once(render(
        &headers.iter().map(|v| (*v).into()).collect::<Vec<_>>(),
    ))
    .chain(rows.iter().map(|r| render(r)))
    .collect::<Vec<_>>()
    .join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn human_and_json_status_golden() {
        assert_eq!(
            table(
                &["ID", "TEXT"],
                &[vec!["7".into(), "hello\x1b[31m\nworld".into()]]
            ),
            "ID  TEXT\n7   hello [31m world"
        );
        assert_eq!(
            serde_json::to_string(&Response::status(xflow_core::State::Idle, 0.0)).unwrap(),
            r#"{"ok":true,"state":"idle","level":0.0}"#
        );
    }
}
