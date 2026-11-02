# Dev-term STT benchmark (#63)

Manual and opt-in: sends synthetic speech to Groq and spends API credit.
The original 150 phrases cover 568 libraries, CLIs, products and camelCase identifiers.
An appended group of 15 Claude/cloud phrases covers homophones and real cloud usage.
Two installed macOS voices, Samantha (US) and Daniel (UK), produce 330 clips.
No speakers, microphone, daemon, desktop delivery or Keychain access is used.

Build the CLI from `apps/cli` using the project's resource gate and cargo-locked
wrapper. Then, from the repository root:

```sh
benchmarks/devterms/run.sh --self-test
benchmarks/devterms/run.sh --generate-only  # say + afconvert; no API calls
# Export GROQ_API_KEY privately before running this command.
benchmarks/devterms/run.sh                 # 2,004 paid requests, two workers
benchmarks/devterms/run.sh --summary
```

`XFLOW_BIN` can point at an existing binary. `--limit 2` is a small smoke run;
remove `results/` before switching from a smoke run to the full benchmark.
The runner resumes completed jobs from `results/transcripts.jsonl`; remove that
file to repeat a full run. WAVs and raw results are ignored by git.

Each group of 15 phrases shares a dictionary of up to 60 terms, in phrase order.
All six conditions use the same audio: Groq turbo/large-v3 × old 224-byte list,
new 200-estimated-token/896-byte list, and `Glossary: a, b, c.` under the same
budget including its wrapper. The old condition stops at first overflow; new
conditions skip overflowing terms. A loopback HTTP proxy inserts the selected
prompt into the CLI's otherwise unchanged multipart upload. The CLI config has
empty vocabulary so this harness can compare both prompt formats regardless of
which format the compiled CLI currently ships. The harness uses the checked-in
merge tables for selection; production selection, exact prompt fields and retry
behavior are separately tested in Rust. Every call uses `xflow transcribe` for
normal WAV decoding, audio encoding, authentication and response parsing.


Scoring counts exact, case-sensitive expected spellings with identifier
boundaries, without fuzzy matching. Punctuation and whitespace around terms do
not matter; changing camelCase or splitting a product name is a miss. Recall is
term occurrences found / term occurrences expected. Variants rotate order per
clip. Attempts are paced below 20 requests/minute/model, with benchmark-only
429 backoff. p50 STT latency measures the proxy's Groq HTTPS request including connection
setup, excluding quota waits; total CLI process latency (including waits) is
also retained for diagnostics. Two concurrent requests and
normal network/provider variance mean this is not a microbenchmark.

The runner also sends silence, quiet hiss, a click and the low-energy thump from
`daemon::speech_free_sessions_never_upload_save_or_deliver` to both models with
all three prompts. Raw silent outputs are recorded separately, never scored as
speech. The daemon rejects the first three before upload; the thump goes through
its low-energy `speech::suspect` filter. File transcription deliberately exposes
raw STT, so a raw hallucination is not proof of delivery.

## Offline estimator fixtures

```sh
uv run --with tiktoken==0.14.0 benchmarks/devterms/generate_tokens.py
```

`tokens.tsv` checks GPT-2 and Whisper multilingual counts for every corpus term,
with and without a leading space, plus adversarial Unicode/punctuation cases.
The runtime tables, `apps/cli/crates/xflow-providers/src/bpe/*-merges.txt`, contain
ASCII-letter merges below rank 8192, sorted by original rank, with a leading
space represented by `_`. Runtime estimation
uses only this prefix of each merge sequence and counts other pieces bytewise.
It takes the larger count for each piece. Omitting later merges overestimates;
it does not assume that arbitrary developer terms average four bytes/token.
These tables supply tokenization data, not dictionary hints or a dev pack.
No Python tokenizer or new dependency is needed at runtime or in Rust tests.

Tokenizer data is from OpenAI GPT-2/tiktoken and Whisper (MIT), see
[the tokenizer licence](../../apps/cli/crates/xflow-providers/src/bpe/LICENSE), and the pinned source in the generator.

## Spoken code (#33)

`spoken-code.tsv` is 240 spoken phrases → expected local output for the
`code.rs` grammar (casing commands, filenames, bracket pairs).
`spoken-code-prose.txt` is the 148-sentence prose control that must stay
byte-identical; both run as Rust tests. The opt-in STT check sends one
Samantha WAV per row to Groq turbo and scores the pipeline output:

```sh
# from apps/cli: cargo-locked build -p xflow-core --example spoken_code
# Export GROQ_API_KEY privately, then from the repository root:
python3 benchmarks/devterms/spoken_code.py            # 240 paid requests
python3 benchmarks/devterms/spoken_code.py --summary  # rescore, no network
```

## Context-gated terms

The appended Claude/cloud group measures provider prompt recall, including real
cloud usage that must survive. `xflow transcribe` returns raw STT; the local cue
corrector runs in the daemon's pre-cleanup pipeline. Offline regression and
latency checks need no credentials or paid requests:

```sh
cargo test --manifest-path apps/cli/Cargo.toml --locked -p xflow-core cues
cargo test --manifest-path apps/cli/Cargo.toml --locked --release -p xflow-core cues::tests::pipeline_latency -- --nocapture
```

The shipped packs (`apps/cli/crates/xflow-core/src/vocab/{dev,corp,sheet,finance}.tsv`)
contain exact heard forms and adjacent left/right cues. A safe form ("kube ctl",
"zlookup") corrects on its own only if every word is not English, not a name, not a
digit and not a single letter, and it is more than a case change. A cued form
("cloud", "swift", "gap") changes only lowercase input next to a non-English
technical cue ("sdk", "repo", "gpi"); English addressing cues ("ask", "hey",
"tell") also need a continuation ("to", "about", "for"). Blockers always win.
`[vocab]` selects packs: dev and corp everywhere, dev always in terminals and
editors, sheet in Excel, Numbers and LibreOffice Calc, finance only when enabled.
`shipped_data_is_small_unique_and_gated` enforces these rules against SCOWL plus
a test-only list (`vocab/english-extra.txt`: SCOWL words to size 70, names to 95
and modern words and short names) and the system word lists where installed
(required on macOS). `review_prose_is_unchanged_in_every_pack_combination` keeps
every sentence from the PR #113 reviews, as written and lowercased, unchanged in
all 16 pack combinations and seven app contexts. Addressing
Claude or Claude Code also requires a positive continuation (such as "to", "about"
or "for"), punctuation, or the end of the text after a left addressing cue.
Claude Code only accepts addressing cues such as "ask", "hey" and "tell".
Phrase matching stops at punctuation, line breaks, paths and identifiers. The same
`formatting.snap_to_dictionary` switch disables the shipped corrector. User
spellings, blocked terms and explicit replacements outrank the shipped cues.
Dictionary phrases conservatively protect every overlapping cue word: adding
"Google Cloud" suppresses the cloud cue, and "VS Code" suppresses cloud code,
even when the full dictionary phrase is absent. All-caps input keeps all-caps
corrections.
Pack selection reuses the focused app id the daemon already reads; no fuzzy
index, new provider call, download or additional model is involved.

## Shipped domain hints

`hints.py` compares `stt.domain_hints` off and on for 27 `say` clips (dev, corp,
finance, sheet and two plain-English controls), each in the app whose domains it
would hint. It mirrors `xflow_core::hints::select` and the provider's 100-token cut,
sends the result as `stt.vocabulary` through `xflow transcribe` (which reads its
own stored key), and reports term recall, missing final periods, upper-case
outputs, p50 and paired latency. 54 requests, about $0.01:

```sh
python3 benchmarks/devterms/hints.py --self-test   # no network
XFLOW_BIN=path/to/xflow python3 benchmarks/devterms/hints.py
python3 benchmarks/devterms/hints.py --summary
```
