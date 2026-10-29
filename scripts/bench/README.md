# Isolated performance probes

The v1 harness complements the original `scripts/benchmark.py` (its CLI and
schema stay compatible). Python uses only the standard library. Required host
tools: Linux `/proc`, `dbus-daemon`, Python ≥3.10, Cargo and Rust dependencies
from `apps/cli/Cargo.lock`. GJS is optional; curl is needed only with `--network`.

All Cargo invocations on this shared development host **must** use its gate:

```sh
/home/mrad/.cache/xflow-dev/bin/cargo build --manifest-path apps/cli/Cargo.toml --release --workspace --locked --offline
/home/mrad/.cache/xflow-dev/bin/cargo build --release --locked --offline --target-dir apps/cli/target --manifest-path scripts/bench/rust/Cargo.toml
python3 -B scripts/bench/baseline.py --daemon apps/cli/target/release/xflowd --cli apps/cli/target/release/xflow --bench apps/cli/target/release/xflow-bench --output docs/benchmarks/linux-baseline-v1.json
```

On an independent checkout use its own Cargo executable instead of the
host-specific gate, and omit `--offline` if dependencies are not cached.
The nested Rust package has its own workspace/lockfile and is intentionally
excluded from the product build. Keep both release profiles identical when
changing `apps/cli/Cargo.toml` settings. The standalone crate currently measures the MVP at
`96e19eb`; updating core/provider APIs requires updating fixture constructors.

Add `--network` for three unauthenticated GET `/` probes per assigned host plus
cold/warm reqwest GET comparisons. No keys, user curl config, proxies, audio,
redirects or paid inference endpoints are used. Curl records DNS, TCP, TLS,
HTTP status, negotiated HTTP version, errors and totals. New curl processes
measure fresh connections; OS DNS caching and TLS infrastructure are uncontrolled.
Reqwest reads response bodies to permit reuse; servers may still close connections.
Cold minus warm is an observed GET comparison, not a guaranteed dictation saving.

Default daemon measurement: 30 fresh launches, 200 IPC samples and CLI `--json
status` samples, five seconds warmup, ≥60 seconds idle observation at 1 Hz.
Each daemon has a private D-Bus with **no service activation directories** and
fresh config/data/runtime/home/cache; inherited secrets and desktop variables
are excluded. Its configuration disables history and cloud access, and uses a
literal loopback STT endpoint that is never called. The driver starts and stops
only its own children. Scratch directories live under the checkout's ignored
`target/btmp` (short names keep Unix socket paths within Linux's limit); it never
connects to the real daemon or alters installed settings.
Startup includes spawn through a successful status reply (1 ms readiness polling);
it excludes private-bus setup. Filesystem cache is warm, config/data are fresh.

Raw observations accompany nearest-rank-index p50/p95/min/max/count and units.
CPU is summed process ticks / wall time; zero ticks means below resolution.
RSS/PSS, thread/fd counts, main-thread schedstat and context switches are recorded.
The last two are **not** whole-process wakeup counts; wakeups remain unmeasured.
The five-minute release CPU target needs a longer `--idle-seconds 300` run.

Rust suites use synthetic signals, a loopback HTTP mock and private D-Bus stubs.
The STT timings exclude fixture cloning and synthetic signal generation but
include product WAV encoding, provider request construction and response handling.
`multipart_build` includes copying a preencoded payload; it is a construction
surrogate rather than a separate measurement inside the product provider.
JSON parsing uses a generic value; it excludes network/stream/body limits.
WAV sample counts vary by duration: 30 at 5 s, 10 at 30 s, four at 120 s.
For `wav`, `--samples` scales these counts; it is not a universal count.

The 95-tap FIR decimator is a prototype with synthetic frequency-response
evidence, not a provider-quality recommendation or transcription accuracy test.
The keyring-crypto suite measures `num-bigint` arithmetic only; it neither
accesses keyring nor replicates the Secret Service implementation or lookup latency.
TUI TestBackend and byte-counter output measure CPU/diff work, excluding terminal
emulator rendering. GJS uses synchronous Gio calls as an RTT fixture; actual
extension commands must use async calls to keep the compositor responsive.
Typed level signals are a research comparison only: the authoritative contract
remains JSON `Event` (including mode), at most 30 Hz, with no interface change here.
Default spawn suite invokes `true`, `wl-copy --version`, `ydotool --help` only.
The inherited optional `spawn-fake-ydotool` fixture is never run by this harness.

## Release profile comparison

```sh
python3 -B scripts/bench/profiles.py --output docs/benchmarks/linux-profiles-v1.json
```

To resume after an environmental failure, add `--resume --profiles <remaining
comma-separated names>`; completed profiles stay in the artifact. The committed
decision retains thin LTO / opt 3 / cgu 1 with explicit `panic = "unwind"`.

This serially builds thin/fat LTO, opt 3/s, panic unwind/abort and cgu 1/16
variants through the gate, saves temporary copies of binaries and measures each
with the same daemon workload. It writes each finished result durably, and
does not edit Cargo.toml. `panic=abort` is comparison-only: the coordinator
requires unwind for TUI restoration. Restore the selected release build afterward
with the standard build command. Concurrent desktop/worker load is uncontrolled;
small timing differences cannot justify a profile change.

## Verification

```sh
python3 -B scripts/bench/test_baseline.py
/home/mrad/.cache/xflow-dev/bin/cargo fmt --manifest-path scripts/bench/rust/Cargo.toml -- --check
/home/mrad/.cache/xflow-dev/bin/cargo clippy --offline --locked --all-targets --target-dir apps/cli/target --manifest-path scripts/bench/rust/Cargo.toml -- -D warnings
/home/mrad/.cache/xflow-dev/bin/cargo test --offline --locked --target-dir apps/cli/target --manifest-path scripts/bench/rust/Cargo.toml
PATH=/home/mrad/.cache/xflow-dev/bin:$PATH sh scripts/check.sh
```

If host `/tmp` hits its quota, create `target/perf-tmp` and run the final check
with `TMPDIR="$PWD/target/perf-tmp"` as well. The profile driver already puts
compiler scratch and binary copies under `apps/cli/target/`; do not clear other workers'
temporary files or compiled caches.

Some sandboxes require normal permission escalation to bind private IPC sockets
and open the shared gate's lockfiles. Do not bypass a denied permission.

## Manual microphone probe (never automated)

`python3 scripts/bench/mic_open.py` asks the human to type `OPEN MICROPHONE`
before launching the Rust `mic-open` probe. It opens the default microphone,
discards samples immediately, records device/config/build/play/first-callback
durations and tests cached-device and direct 16 kHz mono configurations. No
audio is stored or transmitted. The worker has not run this probe; CPAL first
sample cost remains unmeasured. Keep-warm and first-open behavior need actual
device/backend traces; no generic millisecond estimate is substituted for them.

## Long dictation (macOS, Groq)

`long_dictation.py` streams generated `say` speech (3/6/12 minutes by default)
through a `test-support` xflowd as a live microphone and reports checkpoint
coverage, stop→text latency and daemon RSS. It needs `GROQ_API_KEY` in the
environment and never reads the Keychain; see its `--help`.

## Scoreboard

`scoreboard.py` measures the whole process tree (xflowd and its descendants, plus
the recording pill), not just the daemon. It uses only the standard library. On
macOS it reads `proc_pid_rusage`, which gives RSS, physical footprint, lifetime
peak footprint, CPU time and interrupt and package-idle wakeups. On Linux it reads
`/proc`: VmRSS, VmHWM, PSS from `smaps_rollup`, and CPU ticks. Linux has no
wakeup counter, so voluntary context switches over all threads stand in for one.

```sh
# macOS reference run: about 12 minutes (5 min idle, 30 dictations at real time).
cd apps/cli
cargo build --release --locked -p xflow-app --bin xflowd --bin xflow
cp target/release/xflowd target/release/xflow ../../target/scoreboard/
cargo build --release --locked -p xflow-app --features test-support --bin xflowd
cp target/release/xflowd ../../target/scoreboard/xflowd-test-support
cd ../..
set -a; . /path/to/groq.env; set +a          # GROQ_API_KEY; the Keychain is never read
python3 -B scripts/bench/scoreboard.py run --daemon target/scoreboard/xflowd \
  --cli target/scoreboard/xflow --session-daemon target/scoreboard/xflowd-test-support \
  --output docs/benchmarks/scoreboard-macos.json
python3 -B scripts/bench/scoreboard.py render docs/benchmarks/scoreboard-macos.json
```

`run` writes the JSON (raw samples, commit, OS, hardware, rustc and binary hashes)
and a Markdown table beside it. `render` puts that table between the
`scoreboard` markers in PERFORMANCE.md. Pass `--into README.md` once the README
carries the same markers. Never edit the numbers by hand.

What each part measures:

- **Startup.** *Cold* runs a fresh copy of the binary (a new inode, so macOS
  revalidates its code signature) with fresh XDG dirs. *Warm* relaunches that
  same copy on its existing dirs. Each is timed from spawn to the first
  successful status reply, polling every 1 ms. The OS file cache is never purged,
  because that needs root.
- **Idle.** One daemon, 5 s warmup, then 1 Hz tree samples for 300 s. Reports the
  median memory, CPU time over wall time, and wakeups per second.
- **Dictation.** A `test-support` xflowd plays a `say` phrase (48 kHz mono, the
  built-in mic's format, plus 1 s of silence) as its microphone (`XFLOW_TEST_AUDIO=wav`),
  calls Groq with `GROQ_API_KEY`, and writes the text to a private sink file. The
  harness sends `start` with the caller's monotonic `t0_us` and sends `stop` 0.5 s
  into the silence, the way a key release would. *Stop → text* runs from that stop
  request to the delivered event, as the client sees it. The daemon's own
  `timings` are kept too. Thirty runs share one daemon, so the connection pool is
  warm after the first.
- **Pill.** In test-support mode, xflowd never spawns the pill. The harness runs the
  real `xflow overlay-pill` itself and feeds it the daemon's event stream, the way
  xflowd does natively, so peak tree memory includes it. The pill appears on
  screen during the run. Nothing else does: no mic, no speakers, no clipboard,
  no keystrokes and no Keychain.
- **Wispr Flow.** `run` sums every running `/Applications/Wispr Flow.app` process
  and records the app version. It never launches the app. If Wispr isn't running,
  the table lists published third-party figures with sources and dates, labelled
  as not comparable.

Disclosed bypasses: every daemon runs with `[hotkey] enabled = false`, because
creating a keyboard event tap from a freshly built binary can raise the Input
Monitoring prompt. Real microphone open, key-down → first real sample and
pill-visible time need native probes. Those are queued for the runtime lane and
listed in the PR for #15.

### Linux CI gate

The `scoreboard gate` job in `.github/workflows/ci.yml` runs `scoreboard.py gate`
against [`docs/benchmarks/ci-baseline.json`](../../docs/benchmarks/ci-baseline.json).
It uses release `xflowd` and `xflow`, the only binaries the gate needs. They are
cached under a key built from the `apps/cli` sources, `Cargo.lock` and the rustc
version, so a change that doesn't touch the CLI skips the build. The gate takes
10 startup launches, then idles three daemons side by side for 20 s and reports
the median of their medians. Each daemon runs its own copy of the binary, so no
two share text pages, which would split their PSS. It fails when:

- a binary grows by more than `limits.binary_pct`. Sizes are byte-identical
  between runs; the margin absorbs drift in the stable toolchain;
- idle tree PSS grows by more than min(`limits.pss_kib`, `limits.pss_pct`);
- idle tree CPU exceeds `limits.cpu_percent` of one core;
- warm startup p50 exceeds the absolute `limits.startup_ms` budget. No relative
  timing is gated, so runner noise can't fail it;
- any helper process is running at idle.

Measured runner noise behind those limits:

<!-- scoreboard:begin -->
<!-- scoreboard:end -->

When a change grows a binary or memory on purpose, download the job's
`scoreboard-gate` artifacts and regenerate the baseline and this text in the
same PR, so a reviewer sees the growth:

```sh
python3 -B scripts/bench/scoreboard.py baseline gate-1.json gate-2.json gate-3.json
python3 -B scripts/bench/scoreboard.py render docs/benchmarks/ci-baseline.json --into scripts/bench/README.md
python3 -B scripts/bench/test_scoreboard.py   # also fails if any generated text drifts from its JSON
```
