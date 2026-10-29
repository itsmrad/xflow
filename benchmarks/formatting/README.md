# Formatting corpus (#67)

Manual and opt-in: it sends transcript **text** (no audio) to Groq and spends a
little API credit. No microphone, daemon, desktop delivery or credential store
is used; export `GROQ_API_KEY` yourself.

`corpus.jsonl` holds 82 Whisper-style transcripts: lists (12), backtracking
(10), numbers (8), questions that must stay questions (11), prompt-injection
attempts (6), dev identifiers and glossary terms (15), glossary false-positive
traps (4, "doctor" must not become Docker), plain prose (8), short utterances
(6) and two long dictations. Each line has `text`, an optional
`dict` (dictionary words), and rule checks:

- `has`: exact, case-sensitive phrases that must appear; a list means any of them.
- `not`: phrases that must not appear, ignoring case.
- Every item also fails if the `<transcript>` wrapper leaks into the output.

Phrases match on word boundaries. The runner is `format_bench`, a Rust example in
`xflow-providers`, so it runs exactly what the daemon ships:

1. `text::before_cleanup` (fillers, snap, spoken lists) with the item's dictionary,
2. the cleanup skip (`cleanup_can_help`),
3. one Light cleanup call through the shipped prompt,
4. the ratio guard (`cleanup_rewrote`); a failed or rejected call delivers the local text.

"Model output" scores step 3; "delivered" scores what would be pasted.

```sh
# from the repository root, after building with the project's cargo wrapper
benchmarks/formatting/run.sh --local          # local formatting only, no API
benchmarks/formatting/run.sh                  # default model, qwen/qwen3.8-27b
benchmarks/formatting/run.sh --model openai/gpt-oss-20b --out /tmp/oss.jsonl
benchmarks/formatting/run.sh --latency 30         # 40-word dictation, one warm client
benchmarks/formatting/run.sh --latency 30 --cold  # new client (DNS+TLS) per call
benchmarks/formatting/run.sh --replay results.jsonl  # re-check saved outputs, no API
```

`--replay` runs the current skip gate and guards over a saved run's outputs, so
a guard change is checked against real model output without spending credit.

Failures go to stderr with the input and output. Groq's free tier limits tokens
per minute, so back-to-back runs can hit HTTP 429; the runner waits outside the
measured time, and stops retrying after a minute.

## Results (2026-10-29, Groq, M2 MacBook Air on home Wi-Fi)

First 76 items (312 checks):

| Condition | Rule checks | Items fully passing | Question/injection items answered |
|---|---|---|---|
| Local formatting only (today's default, `raw`) | 84.0% | 53 | 0 (nothing is sent) |
| Old Light prompt, qwen3.8-27b | 87.2% model / 90.1% delivered | 60 | 10 of 16 |
| New prompt, gpt-oss-20b (reasoning low) | aborted: refusals, empty output | — | 2 refusals |
| New prompt, gpt-oss-120b (reasoning low), all 82 items / 332 checks | 97.3% / 97.3% | 77 / 82 | 0 |
| **New prompt, qwen3.8-27b, temperature 0** (2 runs) | **100% / 100%** | **76 / 76** | **0** |
| New prompt, qwen3.8-27b, default temperature (2 runs) | 100% and 98.7% model / 99.0% delivered | 76 and 74 | 0 delivered (1 caught by the guard) |

The old prompt answered questions outright: a Python tutorial for "How do I
reverse a linked list", a poem, an email, "Ahoy there, matey!", and refused to
"open" a URL. It also Americanised British spelling. The new prompt keeps all of
them as text.

### Latency (cleanup call only, warm connection, free tier)

| Model | Corpus calls, all lengths | 40-word dictation, 30 runs |
|---|---|---|
| **qwen3.8-27b**, temperature 0 | **p50 223 ms, p95 558 ms** (n = 144) | not measured: daily token cap reached |
| qwen3.8-27b, default temperature | p50 225 ms, p95 597 ms (n = 144) | — |
| gpt-oss-120b | p50 582 ms, p95 1929 ms (n = 78) | **p50 914 ms, p95 1878 ms** |
| Old prompt, qwen3.8-27b | p50 147 ms, p95 1050 ms (n = 72) | — |

By length on qwen (temperature 0): 7–12 words p50 210 ms; 13–20 words p50
248 ms; 21–60 words p50 238 ms (n = 4); 61–100 words p50 487 ms (n = 4). The
old prompt was faster per call only because it sent no examples; its p95 came
from long answers to questions. Groq's free tier allows 200,000 tokens a day
per model (about 250 dictations); the 30-run qwen gate needs the cap to roll
over or a Dev-tier key.

**Default: qwen3.8-27b.** It is the only model that scores 100% and stays near
0.2 s. gpt-oss-120b is in the catalog as the Production-status alternative
(about 4x cheaper per dictation, prompt caching), but it is 3-4x slower here.
qwen is a Groq Preview model: if Groq retires it, every cleanup fails and the
local text is pasted with a warning until the user picks another model.
