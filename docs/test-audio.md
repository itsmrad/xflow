# Dictation QA without speakers

In a **debug build**, start `xflowd` with `XFLOW_TEST_AUDIO=/absolute/path/to.wav`
to replace microphone capture. The microphone is never opened, including when the
file is missing or invalid; a failed load reports an error instead. The daemon
logs `TEST AUDIO: using WAV instead of microphone` at startup. Ordinary release
builds ignore this variable; there is no config option or release feature to
enable WAV capture. `xflowd --help` describes the build's behavior.

Generate a spoken fixture on macOS without playing it:

```sh
say -o /tmp/xflow-qa.aiff 'Postgres is ready. Deploy the API with TypeScript.'
afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/xflow-qa.aiff /tmp/xflow-qa.wav
```

Build from the repository root, stop any existing daemon through its normal
shutdown command (`xflow shutdown`), then run the debug daemon in the foreground
with your QA config and provider credentials supplied through the environment:

```sh
cd apps/cli
cargo build --locked -p xflow-app --bin xflowd
XFLOW_TEST_AUDIO=/tmp/xflow-qa.wav ./target/debug/xflowd --config /absolute/path/to/qa.toml
```

Use the shared `res-gate` and `cargo-locked` tools for builds on the team Mac.
Keep `XFLOW_TEST_AUDIO` scoped to this invocation; do not add it to a shell profile
or installed service. Do not enable `test-support` for desktop QA or set
`XFLOW_TEST_SINK` / `XFLOW_TEST_ROOT`: those belong to the separate isolated
synthetic-audio integration harness.

Focus a Slack draft or another editable test field, hold and release your configured
dictation hotkey, and check that the WAV transcript is pasted and the macOS mic
indicator stays off. For scripted runs, the normal `xflow start` / `xflow stop`
commands use the same capture path. Each start reloads the file and each stop
returns the **entire WAV**, regardless of how long you held the key. Cancel
discards that clip. Replace the WAV between recordings to test another phrase;
the source also survives config reloads. The level meter stays at zero; the normal
silence/maximum recording timers still apply, so leave `auto_stop_secs = 0` for
manual QA.

STT, the speech gate, formatting, cleanup, delivery, history, and the correction
observer all run normally. Provider calls may incur your usual charges, and
corrections can update the QA config's dictionary. Use a separate config/data
directory when you need isolated history and learned words. After a paste, fix a
word in the field and begin the next recording to exercise auto-learning.

PCM and float WAV use the existing decoder without resampling at capture. Inputs
must be regular files of at most 25 MB containing nonempty, finite PCM at
8–384 kHz with 1–32 channels; existing provider upload limits still apply. A
missing, empty, malformed, or oversized WAV fails that recording and can be fixed
before the next start. Short/silent WAVs still hit the normal speech gate.

Stop this foreground daemon with Ctrl+C when finished and restart the installed
daemon normally. Do not commit generated audio fixtures.

For a repeatable file-load timing and PCM fidelity check, run from `apps/cli`:

```sh
XFLOW_TEST_BENCH_WAV=/tmp/xflow-qa.wav cargo test --locked -p xflow-app --lib wav_capture_latency_and_fidelity -- --ignored --nocapture
```

It reports capture p50/p95 over 100 runs and asserts that every decoded sample,
sample rate, and channel count matches the existing decoder. This measures the
debug file source, not provider/network latency or transcription word accuracy.
