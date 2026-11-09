use xflow_app::harness::build_provider_with_cleanup;
// `xflow ask`: one question over the model gateway, streamed to stdout.
use super::{args::Ask, config, ipc, output::Output, Error, Result};
use serde_json::json;
use std::{
    io::{IsTerminal, Read, Write},
    path::Path,
    pin::pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use xflow_app::{
    chat::{self, TurnEnv, TurnEvent, MAX_OUTPUT_TOKENS, MAX_QUESTION_BYTES},
    context::{ContextEnv, Sources},
};
use xflow_core::config::AiConfig;
use xflow_providers::{
    find_provider,
    llm::{Event, LlmError, Stop},
    ProviderKind,
};

/// One plain line, carrying its own next step.
fn fail(code: u8, message: impl Into<String>) -> Error {
    Error::new(code, message, "")
}
fn question(args: &Ask) -> Result<String> {
    let text = match &args.question {
        Some(text) => text.clone(),
        None if std::io::stdin().is_terminal() => {
            return Err(fail(2, "No question: run xflow ask \"your question\""));
        }
        None => {
            let mut text = String::new();
            std::io::stdin()
                .take(MAX_QUESTION_BYTES as u64 + 1)
                .read_to_string(&mut text)?;
            text
        }
    };
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Err(fail(2, "No question: run xflow ask \"your question\""));
    }
    if text.len() > MAX_QUESTION_BYTES {
        return Err(fail(2, "The question is over 64 KiB; shorten it"));
    }
    Ok(text)
}
/// The config's model and endpoint belong to its provider, not to a `--provider` one.
fn selection(mut ai: AiConfig, args: &Ask) -> AiConfig {
    if let Some(provider) = args.provider.as_ref().filter(|p| **p != ai.provider) {
        ai.provider = provider.clone();
        ai.model = None;
        ai.endpoint = None;
    }
    if let Some(model) = &args.model {
        ai.model = Some(model.clone());
    }
    ai
}
fn explain(provider: &str, error: &LlmError) -> Error {
    let key_url = find_provider(ProviderKind::Chat, provider)
        .map(|p| format!(" (get one at {})", p.key_url))
        .unwrap_or_default();
    fail(
        5,
        match error {
            LlmError::NotLoggedIn if provider == "openai-compatible" =>
                "No API key: set XFLOW_AI_API_KEY for explicit openai-compatible, or the cleanup.api_key_env variable for cleanup defaults".into(),
            LlmError::NotLoggedIn if xflow_app::harness::is_harness(provider) => format!("Log in using the installed {provider} CLI, then retry"),
            LlmError::NotLoggedIn => format!(
                "No API key for {provider}: run xflow key set {provider}{key_url}"
            ),
            LlmError::ContextTooLong => {
                "The question and context are too long for this model: lower ai.max_context_tokens or use --no-context".to_owned()
            }
            LlmError::Offline => format!(
                "privacy.offline allows only loopback providers, so {provider} is refused: use provider custom with a loopback ai.endpoint, or turn offline off"
            ),
            other => xflow_providers::llm_problem(other)
                .and_then(|problem| {
                    let name = find_provider(ProviderKind::Chat, provider).map_or(provider, |p| p.name);
                    problem.explain(name, provider)
                })
                .unwrap_or_else(|| format!("{provider}: {other}")),
        },
    )
}
pub(super) fn consent(path: &Path, ai: &AiConfig, args: &Ask) -> Result<()> {
    if args.no_context
        || args.sources == "none"
        || args.yes
        || (args.sources == "dictations" && ai.consented.contains(&ai.provider))
    {
        return Ok(());
    }
    let provider = &ai.provider;
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(fail(
            2,
            format!(
                "{provider} is not allowed to receive your transcripts yet: pass --yes for this run, use --no-context, or run xflow ask in a terminal to save your choice"
            ),
        ));
    }
    eprintln!("{}", chat::consent_text(provider));
    let answer = config::ask("Allow it? [y/N] ", "n")?;
    if !matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
        return Err(fail(
            1,
            "Nothing was sent: answer y to allow it, or use --no-context",
        ));
    }
    // The answer holds for this run even if it cannot be remembered.
    if args.sources == "dictations" {
        if let Err(error) = save_consent(path, provider) {
            eprintln!("Warning: could not save your choice: {error}");
        }
    }
    Ok(())
}
fn save_consent(path: &Path, provider: &str) -> Result<()> {
    let lock = config::lock(path)?;
    // Read again under the lock: an edit made meanwhile is kept.
    let mut file = config::load(path)?;
    let mut consented = file.config().map_err(Error::config)?.ai.consented;
    if !consented.iter().any(|id| id == provider) {
        consented.push(provider.to_owned());
    }
    file.set_typed("ai.consented", &consented)
        .map_err(Error::config)?;
    file.save().map_err(Error::config)?;
    drop(lock);
    Ok(())
}

#[derive(Default)]
struct Answer {
    text: String,
    reads: Vec<serde_json::Value>,
    retrieval_ms: u64,
    first_token: Option<Duration>,
    input: u64,
    output: u64,
    cached: u64,
}
/// Off the runtime thread, so a stalled pipe cannot hold up Ctrl-C.
async fn write_stdout(bytes: Vec<u8>) -> Result<()> {
    let written = tokio::task::spawn_blocking(move || {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&bytes)?;
        stdout.flush()
    });
    written.await.map_err(anyhow::Error::from)??;
    Ok(())
}
impl Answer {
    async fn on(
        &mut self,
        event: TurnEvent,
        started: Instant,
        out: &Output,
        terminal: bool,
    ) -> Result<()> {
        match event {
            TurnEvent::Model(Event::Text(text)) => {
                self.first_token.get_or_insert_with(|| started.elapsed());
                self.text.push_str(&text);
                if !out.json {
                    // Model text can repeat a transcript: no terminal controls from it.
                    let shown = if terminal {
                        text.chars()
                            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
                            .collect()
                    } else {
                        text
                    };
                    write_stdout(shown.into_bytes()).await?;
                }
            }
            TurnEvent::Model(Event::Usage {
                input,
                output,
                cached,
            }) => {
                self.input += input;
                self.output += output;
                self.cached += cached;
            }
            TurnEvent::Model(_) => {}
            TurnEvent::Read {
                tool,
                arguments,
                bytes,
                ms,
            } => {
                self.retrieval_ms += ms;
                if !out.json && !out.quiet {
                    eprintln!(
                        "read: {tool} {} ({bytes} bytes)",
                        super::output::clean(&arguments.to_string())
                    );
                }
                self.reads
                    .push(json!({"tool":tool,"arguments":arguments,"bytes":bytes}));
            }
            TurnEvent::Notice(notice) => {
                if !out.json && !out.quiet {
                    eprintln!("note: {}", super::output::clean(&notice));
                }
            }
        }
        Ok(())
    }
}
fn stop_name(stop: Stop) -> &'static str {
    match stop {
        Stop::End => "end",
        Stop::ToolUse => "tool_use",
        Stop::Length => "length",
    }
}

pub fn run(path: &Path, args: &Ask, out: &Output) -> Result<()> {
    let question = question(args)?;
    let cfg = config::load(path)?.config().map_err(Error::config)?;
    let offline = cfg.privacy.offline;
    let budget = cfg.ai.max_context_tokens;
    let mut ai = selection(cfg.ai, args);
    if matches!(args.sources.as_str(), "meetings" | "both") {
        ai.fallback.clear();
    }
    if ai.provider.is_empty() && cfg.cleanup.provider.is_none() && cfg.cleanup.endpoint.is_none() {
        return Err(fail(4, "xflow ask is not set up: run xflow key set groq, then set ai.provider or cleanup.provider"));
    }
    let label = if ai.provider.is_empty() {
        cfg.cleanup.provider.as_deref().unwrap_or("cleanup")
    } else {
        &ai.provider
    };
    let (ai, gateway) =
        build_provider_with_cleanup(&ai, &cfg.cleanup, offline).map_err(|e| explain(label, &e))?;
    let provider = ai.provider.clone();
    consent(path, &ai, args)?;
    if xflow_app::harness::is_harness(&provider) {
        eprintln!("note: the installed CLI uses its own login and may retain logs, caches or sessions. xflow stores no CLI output. Context is bounded prefetch only.");
    }
    if !out.quiet && !out.json {
        for id in &ai.fallback {
            if *id != provider && !ai.consented.contains(id) {
                eprintln!("note: fallback {id} is skipped until you allow it: run xflow ask --provider {id} once in a terminal");
            }
        }
    }
    let model = ai
        .model
        .clone()
        .or_else(|| {
            find_provider(ProviderKind::Chat, &provider).map(|p| p.default_model.to_owned())
        })
        .unwrap_or_default();
    let mut context = ContextEnv::current().map_err(Error::config)?;
    context.config = path.to_owned();
    context.client = "xflow ask".into();
    let env = TurnEnv {
        context: Arc::new(context),
        model: model.clone(),
        max_context_tokens: budget,
        max_output_tokens: MAX_OUTPUT_TOKENS,
    };
    let sources = Sources {
        dictations: !args.no_context && matches!(args.sources.as_str(), "dictations" | "both"),
        meetings: !args.no_context && matches!(args.sources.as_str(), "meetings" | "both"),
    };
    let terminal = std::io::stdout().is_terminal();
    let started = Instant::now();
    let mut answer = Answer::default();
    let outcome = ipc::block_on(async {
        let (tx, mut rx) = mpsc::channel(64);
        let mut history = Vec::new();
        let turn = async {
            let result =
                chat::run_turn(&gateway, &env, sources, &mut history, &question, &tx).await;
            drop(tx);
            Ok::<_, Error>(result)
        };
        let consume = async {
            while let Some(event) = rx.recv().await {
                answer.on(event, started, out, terminal).await?;
            }
            Ok::<_, Error>(())
        };
        // Installed by this first poll, before anything leaves the machine.
        let cancel = pin!(tokio::signal::ctrl_c());
        // Dropping the turn aborts the HTTP body. A blocked read is abandoned at exit.
        tokio::select! {
            biased;
            _ = cancel => Ok(None),
            done = async { tokio::try_join!(turn, consume) } => done.map(|(result, ())| Some(result)),
        }
    });
    let outcome = outcome?;
    let total = started.elapsed();
    if !out.json && !answer.text.is_empty() && !answer.text.ends_with('\n') {
        writeln!(std::io::stdout())?;
    }
    let overhead_ms = gateway.overhead().as_secs_f64() * 1000.0;
    let first_ms = answer.first_token.map(|d| d.as_millis() as u64);
    let timing = json!({"retrieval_ms":answer.retrieval_ms,"gateway_overhead_ms":overhead_ms,
        "first_token_ms":first_ms,"total_ms":total.as_millis() as u64,
        "input_tokens":answer.input,"output_tokens":answer.output,"cached_tokens":answer.cached});
    if args.timing && !out.json {
        eprintln!(
            "retrieval {} ms, gateway overhead {overhead_ms:.2} ms, first token {} ms, total {} ms, tokens {} in ({} cached) {} out",
            answer.retrieval_ms,
            first_ms.map_or("-".into(), |ms| ms.to_string()),
            total.as_millis(),
            answer.input,
            answer.cached,
            answer.output
        );
    }
    let (ok, stop) = match &outcome {
        Some(Ok(stop)) => (true, Some(stop_name(*stop))),
        _ => (false, None),
    };
    // What arrived stays visible on failure too; errors go to stderr as usual.
    if out.json && (ok || !answer.text.is_empty()) {
        out.data(
            &json!({"ok":ok,"provider":provider,"model":model,"text":answer.text,"stop":stop,
                "reads":answer.reads,"timing":timing}),
            "",
        )?;
    }
    match outcome {
        None => Err(fail(130, "Cancelled")),
        Some(Ok(Stop::Length)) => {
            if !out.json && !out.quiet {
                eprintln!("note: the answer reached the output limit and may be incomplete");
            }
            Ok(())
        }
        Some(Ok(_)) => Ok(()),
        Some(Err(error)) => Err(explain(&provider, &error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_consent_keeps_the_file_and_repeats_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# keep this comment\n[privacy]\nhistory = true\n").unwrap();
        save_consent(&path, "openai").unwrap();
        save_consent(&path, "openai").unwrap();
        save_consent(&path, "anthropic").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep this comment"));
        let file = config::load(&path).unwrap();
        assert_eq!(file.config().unwrap().ai.consented, ["openai", "anthropic"]);
    }
    #[test]
    fn another_provider_does_not_inherit_the_configured_model_or_endpoint() {
        let configured = AiConfig {
            provider: "custom".into(),
            model: Some("local".into()),
            endpoint: Some("http://127.0.0.1:1/x".into()),
            ..AiConfig::default()
        };
        let args = |provider: Option<&str>, model: Option<&str>| Ask {
            question: None,
            provider: provider.map(str::to_owned),
            model: model.map(str::to_owned),
            timing: false,
            no_context: false,
            sources: "dictations".into(),
            yes: false,
        };
        let same = selection(configured.clone(), &args(Some("custom"), None));
        assert_eq!(same.model.as_deref(), Some("local"));
        let other = selection(configured.clone(), &args(Some("openai"), None));
        assert_eq!((other.model, other.endpoint), (None, None));
        let chosen = selection(configured, &args(Some("openai"), Some("x")));
        assert_eq!(chosen.model.as_deref(), Some("x"));
    }
}
