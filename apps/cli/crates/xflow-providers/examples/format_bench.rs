//! Formatting corpus runner (#67): the daemon's post-STT path on text, without
//! audio. Local formatting, the cleanup skip, Light cleanup on real Groq, the
//! ratio guard, then the corpus rule checks. See benchmarks/formatting/README.md.
//! Manual and opt-in: it spends API credit. GROQ_API_KEY must already be set;
//! no credential store is read.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use xflow_core::{
    config::{CleanupConfig, Config},
    CleanupMode, TextTransformer, TransformRequest,
};

/// A 40-word dictation for the latency gate.
const FORTY: &str = "So I looked at the dashboard this morning and the error rate on the checkout service went up after the last deploy, actually after the one before it, so can you roll that back and then ping me when it's done";

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}
fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}
/// Whole-phrase match: alphanumeric needle edges must sit on a word boundary.
fn phrase(hay: &str, needle: &str, fold: bool) -> bool {
    let (hay, needle) = if fold {
        (hay.to_lowercase(), needle.to_lowercase())
    } else {
        (hay.to_owned(), needle.to_owned())
    };
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    hay.match_indices(&needle).any(|(i, _)| {
        (!needle.starts_with(char::is_alphanumeric) || edge(hay[..i].chars().next_back()))
            && (!needle.ends_with(char::is_alphanumeric)
                || edge(hay[i + needle.len()..].chars().next()))
    })
}
/// Failed checks; `has` entries are exact (a list means any of), `not` ignores case.
fn score(item: &Value, text: &str) -> (usize, Vec<String>) {
    let mut failed = vec![];
    if text.contains("<transcript") || text.contains("</transcript") {
        failed.push("wrapper tags".into());
    }
    let has = item["has"].as_array().cloned().unwrap_or_default();
    let not = item["not"].as_array().cloned().unwrap_or_default();
    for want in &has {
        let any: Vec<_> = match want {
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            v => v.as_str().into_iter().collect(),
        };
        if !any.iter().any(|a| phrase(text, a, false)) {
            failed.push(format!("missing {want}"));
        }
    }
    for bad in not.iter().filter_map(Value::as_str) {
        if phrase(text, bad, true) {
            failed.push(format!("unwanted {bad:?}"));
        }
    }
    (1 + has.len() + not.len(), failed)
}
fn transformer(model: &str) -> Result<std::sync::Arc<dyn TextTransformer>> {
    let config = CleanupConfig {
        mode: CleanupMode::Light,
        provider: Some("groq".into()),
        model: Some(model.into()),
        api_key_env: "GROQ_API_KEY".into(),
        ..CleanupConfig::default()
    };
    Ok(xflow_providers::build_transformer(&config, false)?.expect("light cleanup"))
}
/// One cleanup call; rate limits wait outside the measured time.
async fn clean(t: &dyn TextTransformer, text: &str, words: &[String]) -> Result<(String, u32)> {
    let mut attempt = 0;
    loop {
        let start = Instant::now();
        let result = t
            .transform(TransformRequest {
                text,
                mode: CleanupMode::Light,
                instructions: None,
                command: None,
                app_id: None,
                vocabulary: words,
            })
            .await;
        match result {
            Err(e) if format!("{e:#}").contains("HTTP 429") && attempt < 6 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            r => return r.map(|s| (s, start.elapsed().as_millis() as u32)),
        }
    }
}
fn percentile(mut ms: Vec<u32>, p: usize) -> u32 {
    if ms.is_empty() {
        return 0;
    }
    ms.sort_unstable();
    ms[(ms.len() * p).div_ceil(100).saturating_sub(1)]
}

#[tokio::main]
async fn main() -> Result<()> {
    let model = arg("--model").unwrap_or_else(|| "qwen/qwen3.8-27b".into());
    if let Some(runs) = arg("--latency") {
        let runs: usize = runs.parse()?;
        let mut t = transformer(&model)?;
        let mut ms = vec![];
        for _ in 0..runs {
            if flag("--cold") {
                t = transformer(&model)?;
                // What the daemon does while the user is still talking.
                if flag("--prewarm") {
                    t.warm().await?;
                }
            }
            ms.push(clean(&*t, FORTY, &[]).await?.1);
            // Free-tier Groq allows 8k tokens a minute; also realistic dictation pacing.
            tokio::time::sleep(Duration::from_secs(8)).await;
        }
        let cold = match (flag("--cold"), flag("--prewarm")) {
            (true, true) => "new client + warm()",
            (true, false) => "cold",
            _ => "warm",
        };
        println!(
            "{model} {cold} {runs} runs, 40 words: p50 {} ms, p95 {} ms, max {} ms",
            percentile(ms.clone(), 50),
            percentile(ms.clone(), 95),
            percentile(ms, 100)
        );
        return Ok(());
    }
    let path = std::env::args()
        .nth(1)
        .filter(|a| !a.starts_with("--"))
        .context("usage: format_bench <corpus.jsonl> [--model M] [--out results.jsonl] [--local | --replay results.jsonl] | --latency N [--cold]")?;
    let corpus = std::fs::read_to_string(&path)?;
    // Saved model outputs by id: re-check the skip and guards without API calls.
    let replay: std::collections::HashMap<String, (String, Option<u32>)> = match arg("--replay") {
        Some(file) => std::fs::read_to_string(file)?
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|r| {
                let ms = r["ms"].as_u64()? as u32;
                Some((
                    r["id"].as_str()?.to_owned(),
                    (r["output"].as_str()?.to_owned(), Some(ms)),
                ))
            })
            .collect(),
        None => Default::default(),
    };
    let dry = flag("--local") || flag("--replay");
    let t = if dry {
        None
    } else {
        Some(transformer(&model)?)
    };
    let mut out = String::new();
    let (mut checks, mut model_fail, mut final_fail, mut items_ok) = (0, 0, 0, 0);
    let (mut skipped, mut rejected, mut answered, mut ms) = (0, 0, vec![], vec![]);
    let items: Vec<Value> = corpus
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    for item in &items {
        let mut config = Config::default();
        config.dictionary.words = serde_json::from_value(item["dict"].clone()).unwrap_or_default();
        let local = xflow_core::text::before_cleanup(
            item["text"].as_str().context("text")?,
            &config,
            false,
        );
        let skip = !xflow_providers::cleanup_can_help(&local)
            || dry && !replay.contains_key(item["id"].as_str().unwrap_or(""));
        let id = item["id"].as_str().unwrap_or("?");
        let (output, took) = match (&t, replay.get(id)) {
            _ if skip => (local.clone(), None),
            (Some(t), _) => match clean(&**t, &local, &config.dictionary.words).await {
                Ok((s, took)) => {
                    ms.push(took);
                    // The shared free-tier key allows ~15 requests a minute.
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    (s, Some(took))
                }
                // Like the daemon: a failed call delivers the local text.
                Err(e) => (format!("ERROR {e:#}"), None),
            },
            (None, Some((s, took))) => {
                ms.extend(*took);
                (s.clone(), *took)
            }
            (None, None) => (local.clone(), None),
        };
        let guard = !skip
            && (took.is_none()
                || xflow_providers::cleanup_rewrote(
                    &local,
                    &output,
                    CleanupMode::Light,
                    &config.dictionary.words,
                ));
        let text = if guard { &local } else { &output };
        let (n, model_failed) = score(item, &output);
        let (_, failed) = score(item, text);
        checks += n;
        model_fail += model_failed.len();
        final_fail += failed.len();
        items_ok += usize::from(failed.is_empty());
        skipped += usize::from(skip);
        rejected += usize::from(guard);
        if matches!(item["cat"].as_str(), Some("question" | "injection"))
            && !model_failed.is_empty()
        {
            answered.push(id.to_owned());
        }
        if !failed.is_empty() || !model_failed.is_empty() {
            eprintln!("FAIL {id}: model {model_failed:?} final {failed:?}\n  in:  {local:?}\n  out: {output:?}");
        }
        out.push_str(&format!("{}\n", json!({"id": id, "local": local, "output": output, "skipped": skip, "rejected": guard, "ms": took, "failed": failed})));
    }
    if let Some(file) = arg("--out") {
        std::fs::write(file, out)?;
    }
    if checks == 0 {
        bail!("empty corpus");
    }
    let pct = |fail: usize| 100.0 * (checks - fail) as f64 / checks as f64;
    println!(
        "{} | {} items, {checks} checks | model output {:.1}% | delivered {:.1}% | items fully passing {items_ok} | skipped {skipped} | guard rejected {rejected} | question/injection items failing {} {:?} | cleanup p50 {} ms p95 {} ms",
        if !replay.is_empty() {
            format!("replay of {}", arg("--replay").unwrap_or_default())
        } else if dry {
            "local only".to_owned()
        } else {
            model
        },
        items.len(),
        pct(model_fail),
        pct(final_fail),
        answered.len(),
        answered,
        percentile(ms.clone(), 50),
        percentile(ms, 95),
    );
    Ok(())
}
