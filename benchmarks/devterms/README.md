# Dev-term STT benchmark (#63)

Manual and opt-in: sends synthetic speech to Groq and spends API credit.
150 phrases contain 568 distinct libraries, CLIs, products and camelCase identifiers.
Two installed macOS voices, Samantha (US) and Daniel (UK), produce 300 clips.
No speakers, microphone, daemon, desktop delivery or Keychain access is used.

Build the CLI from `apps/cli` using the project's resource gate and cargo-locked
wrapper. Then, from the repository root:

```sh
benchmarks/devterms/run.sh --self-test
benchmarks/devterms/run.sh --generate-only  # say + afconvert; no API calls
# Export GROQ_API_KEY privately before running this command.
benchmarks/devterms/run.sh                 # 1,824 paid requests, two workers
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
