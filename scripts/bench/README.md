# Isolated performance probes

The v1 harness complements the original `scripts/benchmark.py` (its CLI and
schema stay compatible). Python uses only the standard library. Required host
tools: Linux `/proc`, `dbus-daemon`, Python ≥3.10, Cargo and Rust dependencies
from the root lockfile. GJS is optional; curl is needed only with `--network`.

All Cargo invocations on this shared development host **must** use its gate:

```sh
/home/mrad/.cache/xflow-dev/bin/cargo build --release --workspace --locked --offline
/home/mrad/.cache/xflow-dev/bin/cargo build --release --locked --offline --target-dir target --manifest-path scripts/bench/rust/Cargo.toml
python3 -B scripts/bench/baseline.py --daemon target/release/xflowd --cli target/release/xflow --bench target/release/xflow-bench --output docs/benchmarks/linux-baseline-v1.json
```

On an independent checkout use its own Cargo executable instead of the
host-specific gate, and omit `--offline` if dependencies are not cached.
The nested Rust package has its own workspace/lockfile and is intentionally
excluded from the product build. Keep both release profiles identical when
changing root settings. The standalone crate currently measures the MVP at
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
/home/mrad/.cache/xflow-dev/bin/cargo clippy --offline --locked --all-targets --target-dir target --manifest-path scripts/bench/rust/Cargo.toml -- -D warnings
/home/mrad/.cache/xflow-dev/bin/cargo test --offline --locked --target-dir target --manifest-path scripts/bench/rust/Cargo.toml
PATH=/home/mrad/.cache/xflow-dev/bin:$PATH sh scripts/check.sh
```

If host `/tmp` hits its quota, create `target/perf-tmp` and run the final check
with `TMPDIR="$PWD/target/perf-tmp"` as well. The profile driver already puts
compiler scratch and binary copies under `target/`; do not clear other workers'
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
