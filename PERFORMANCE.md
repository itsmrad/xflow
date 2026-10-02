# Performance methodology and budgets

The dated baselines below describe their named historical fixtures, not the combined v1 build. Integrated CLI measurements are in [the CLI report](crates/xflow-app/src/cli/REPORT.md); combined verification and short headless smoke are in [the integration report](docs/INTEGRATION_V1.md). Physical hotkey, microphone, live-provider, destination and compositor budgets remain unmeasured by those checks.

## Measured v1 optimization baseline — 2026-10-01

[linux-baseline-v1.json](docs/benchmarks/linux-baseline-v1.json) contains raw
observations and summaries from the MVP product at `96e19eb` on Ubuntu 26.04.1,
Linux 7.0.0-34, Ryzen 5 3600X (12 logical CPUs), rustc 1.98.1. Product release
settings: opt-level 3, thin LTO, one codegen unit, stripped, panic unwind.
This is an isolated software baseline, **not** acceptance of physical capture,
cloud inference, desktop paste or GNOME renderer targets. The current interface
target is the orchestrator's `CONTRACTS.md` at `f510a2c`; its new delivery/config
fields are not implemented by the MVP fixture. The typed signal experiment
does not propose an independent contract change.

[Reproduction and safety guide](scripts/bench/README.md) documents exact build,
collection, profile and verification commands. The original
`scripts/benchmark.py` stays compatible; use `scripts/bench/baseline.py` for the
extended schema. Every daemon launch uses fresh XDG paths, a private bus with no
service activation, history off, a loopback-only offline provider, and status
requests only. No real microphone, clipboard, keystrokes, keyring or paid
inference was exercised. Network probes send unauthenticated GET `/` only.

| Workload | p50 | p95 | Samples / boundary |
| --- | ---: | ---: | --- |
| Daemon startup | 2.601 ms | 2.969 ms | 30 launches; warm filesystem cache, fresh config; spawn → successful status; 1 ms polling; private-bus setup excluded |
| CLI `--json status` | 1.535 ms | 2.058 ms | 200; process spawn, runtime, socket and output |
| Unix status request | 0.075 ms | 0.112 ms | 200; new connection per request |
| Rust D-Bus Command, cached proxy | 0.145 ms | 0.200 ms | 200; stub actor, JSON request/reply; no capture |
| GJS/Gio D-Bus Command | 0.233 ms | 0.315 ms | 200; synchronous fixture; actual extension must use async calls |
| Context with fresh D-Bus connection | 0.388 ms | 0.545 ms | 200; includes connection/proxy/JSON |
| Context with cached proxy | 0.100 ms | 0.144 ms | 200 |
| D-Bus JSON event delivery | 0.132 ms | 0.165 ms | 200; paced fixture, not Shell frame time |
| Harmless `true` / `wl-copy --version` / `ydotool --help` spawns | 0.498 / 0.633 / 0.535 ms | 0.660 / 0.802 / 0.661 ms | 200 each; no paste/clipboard path |
| 5 s WAV encode: 48 kHz stereo → mono WAV | 1.416 ms | 1.863 ms | 30; WAV remains 48 kHz |
| 5 s WAV encode: native 16 kHz mono | 0.395 ms | 0.552 ms | 30 |
| 5 s FIR decimate + WAV, from 48 kHz stereo | 1.541 ms | 1.846 ms | 30; synthetic quality check only |
| Loopback STT: 5 s 48 kHz stereo / 16 kHz mono | 1.854 / 0.591 ms | 2.432 / 0.840 ms | 200 each; encode + multipart + HTTP + parse, no inference; input cloning excluded |
| Multipart construction, preencoded 16 kHz 5 s | 3.146 µs | 3.948 µs | 200; includes copying payload |
| Generic provider-response JSON parse | 0.221 µs | 0.259 µs | 200 batches of 10,000; no streaming/body limits |
| TUI TestBackend level redraw: 80×24 / 160×48 / 240×67 | 0.061 / 0.197 / 0.392 ms | 0.077 / 0.216 / 0.406 ms | 200 each; CPU/diff only, excludes terminal rendering |

After five seconds warmup, the daemon used 11,116 KiB RSS (10.86 MiB), 8,067 KiB
PSS (7.88 MiB), four threads and 12 fds. Sixty 1 Hz observations over 60.001 s
showed zero additional process CPU ticks. This means below the 0.0167% one-tick
resolution, not proof of zero CPU; the five-minute release target remains open.
Main-thread context switches and schedstat are retained, but they cannot establish
whole-process wakeups. Wakeups are explicitly unmeasured.

The seven release budgets below stay unchanged. Software startup is comfortably
below 100 ms on this host, and RSS below 30 MiB for the isolated workload. The
other five UX metrics need the physical/desktop/provider boundaries described
below. The old 50.9 ms startup smoke used 50 ms readiness polling and one launch;
the new 1 ms polling result is **not** a product startup improvement.

### Network evidence and limits

Each host had three fresh curl connections, zero transport errors and negotiated
HTTP/2. HTTP 404/421/307 root responses are successful transport probes; they do
not establish inference availability. The product-equivalent reqwest probe used
HTTP/1.1 on every host, matching the baseline's `default-features = false` and
absence of `http2`. Redirects were not followed.

| Host | DNS + TCP + TLS setup p50 (ms) | Reqwest root GET cold / warm p50 (ms) |
| --- | ---: | ---: |
| api.groq.com | 20.154 | 260.054 / 246.845 |
| api.openai.com | 19.823 | 26.866 / 6.208 |
| api.deepgram.com | 537.407 | 1121.936 / 269.249 |
| api.assemblyai.com | 573.294 | 836.751 / 287.584 |
| api.elevenlabs.io | 100.588 | 408.802 / 301.459 |
| generativelanguage.googleapis.com | 17.298 | 91.017 / 330.692 |
| api.mistral.ai | 21.968 | 223.652 / 278.991 |
| openrouter.ai | 22.496 | 91.395 / 66.074 |
| api.together.xyz | 27.637 | 269.787 / 255.634 |
| api.fireworks.ai | 525.466 | 793.766 / 251.490 |

Setup p50 ranges from 17 to 573 ms, but this is neither a guaranteed saving nor
a provider-location estimate. Warm minus cold can reverse under root-response
variability (Google/Mistral here). Root GET includes response latency; HTTP/2
benefit is not separately measured and keep-alive reuse is not instrumented at
the connection level. Prewarming during recording can move setup earlier when
the connection remains usable, and the authoritative `SpeechToText::warm()`
contract already permits this. Verify actual inference endpoints and cancellation
behavior without billed calls before choosing a warming method. Reqwest's
[Client documentation](https://docs.rs/reqwest/latest/reqwest/struct.Client.html)
describes its reusable connection pool; retain the client across sessions.

### Ranked optimization plan for the owning workers

Savings below are measured fixture differences or labelled estimates; they are
not additive, and no code-level change was made in another worker's area.

| Rank / owner | Exact baseline location → change | Expected saving and evidence | Acceptance / constraint |
| --- | --- | --- | --- |
| 1 — providers | `crates/xflow-providers/src/lib.rs::client`, `HttpProvider::transcribe_inner`; `crates/xflow-providers/Cargo.toml` → retain clients, enable `http2`, implement background `warm()` at recording start | Potentially move 17–573 ms setup off stop path when pool reuse succeeds; root cold/warm deltas range −240 to +853 ms, so saving is workload-dependent; no measured independent h2 gain | No audio, credentials or billing in warm; retain no-retry behavior; verify with mock connections and new contract |
| 2 — platform / providers | `audio.rs::open_stream`, `Recording::new`, `providers::encode_wav` → prefer supported native 16 kHz mono or incremental downmix/resampling during capture | 120 s f32 input 46.08 → 7.68 MB (38.4 MB analytic reduction); WAV 11.52 → 3.84 MB (3× upload reduction); 5 s native encode saves 1.02 ms and loopback STT saves 1.26 ms p50 | Device support and transcription accuracy unmeasured; 48 kHz stereo already encodes mono, so upload saving is 3×, not 6×; preserve bounded capture and whole frames |
| 3 — daemon / platform | `crates/xflow-app/src/daemon.rs::Engine::start`, `crates/xflow-platform/src/desktop.rs::shell_context` → open capture before awaiting focus, accept caller context; cache D-Bus proxy | Remove measured fresh context 0.388 ms from first-open path and avoid up to configured 500 ms timeout exposure; cached context saves about 0.288 ms p50 | Actual first audio remains unmeasured; the new contract already requires mic-before-focus; preserve target validation at injection |
| 4 — extension / CLI | `packaging/gnome-extension/extension.js::_command`, `crates/xflow-app/src/bin/xflow.rs::main` → persistent async `org.xflow.Daemon.Command` from Shell instead of per-key CLI exec | About 1.30 ms p50 / 1.74 ms p95 fixture difference: CLI 1.535/2.058 vs GJS 0.233/0.315 ms | Stub has no mic/workflow; compare real shortcut timestamps later; keep CLI for terminal users |
| 5 — platform / extension | `crates/xflow-platform/src/desktop.rs::copy_with`, `virtual_paste`; extension Shell `Inject` → in-extension clipboard and virtual-keyboard delivery with fallback | Avoid roughly 1.17 ms combined helper-spawn p50 (help/version surrogate); actual subprocess action/keyboard delays remain unmeasured in this run | Target/focus/lock guards, terminal multiline safety and conditional restore remain required; no real paste tests performed |
| 6 — TUI / daemon | `crates/xflow-app/src/tui.rs::run` draw closure; `daemon.rs::Engine::run` 50 ms levels → redraw on changed values only, cap/coalesce events | At 240×67, 20 redraws/s cost about 7.83 ms CPU/s (0.78% of one core, analytic); skipping 19 unchanged redraws saves up to 7.44 ms/s | Active UI is well below 2 ms CPU/frame fixture target; no idle animation; do not exceed contract's 30 Hz |
| 7 — providers / daemon | `crates/xflow-providers/src/lib.rs::Credentials::header`, called by `HttpProvider::transcribe_inner` → cache validated credentials in memory with explicit invalidation | Lookup saving unknown: no keyring accessed; arithmetic surrogate is ~0.586 ms per `num-bigint` modpow and cannot establish Secret Service latency | Preserve secret zeroization/redaction, reload/key-update invalidation and timeout bounds; measure with a mock service, never user keyring |
| 8 — daemon / extension | `crates/xflow-platform/src/bridge.rs::run_desktop_bridge`, `DesktopEvent` encoding → retain current JSON contract; avoid duplicate unchanged events | JSON encode p50 65 ns; JSON/typed event emit both ~17.4 µs; typed fixture saves 50 bytes/message, only 1.5 KB/s at 30 Hz | No evidence for a protocol rewrite; interface changes require orchestrator approval; measure new contract including `mode` |

The FIR prototype measured 0 dB gain at 1/3.4 kHz and −101.64 dB at 12 kHz,
but at 120 s stop-time decimate+encode took 55.60 ms versus 35.23 ms for native
48 kHz stereo encode. Moving that CPU work into capture or negotiating a native
format matters more than calling every resampler an optimization. Tone rejection
does not prove recognition accuracy. The 30 s router loopback measured
20.13 ms at 48 kHz mono versus 6.77 ms at 16 kHz; JSON/base64 expands WAV by
about 4/3 and adds another live allocation.

### Profile decision and verification

The selected profile remains thin LTO / opt 3 / cgu 1 / unwind / stripped,
with `panic = "unwind"` now explicit. All five profiles were compared with 30
launches, 200 IPC/CLI samples and 60 seconds idle each. The full comparison and
raw CPU validation are recorded in
[linux-profiles-v1.json](docs/benchmarks/linux-profiles-v1.json).

| Profile | Daemon / CLI bytes | Startup p50 / p95 (ms) | Idle RSS / PSS (KiB) |
| --- | ---: | ---: | ---: |
| thin / opt 3 / cgu 1 / unwind — selected | 9,929,168 / 2,319,784 | 2.562 / 3.259 | 8,772 / 5,659 |
| fat / opt 3 / cgu 1 / unwind | 9,101,584 / 2,142,448 | 2.675 / 3.093 | 7,876 / 4,793 |
| thin / opt s / cgu 1 / unwind | 6,970,320 / 2,046,840 | 2.638 / 3.088 | 9,276 / 6,153 |
| thin / opt 3 / cgu 1 / abort — ineligible | 8,941,456 / 2,082,032 | 2.512 / 2.764 | 10,476 / 7,540 |
| thin / opt 3 / cgu 16 / unwind | 11,816,016 / 2,828,488 | 2.601 / 3.727 | 11,572 / 8,462 |

Every profile observed zero idle CPU ticks. RSS/PSS vary with residency and
sharing; the isolated baseline's 11,116 KiB RSS and the profile run's 8,772 KiB
are separate observations, not evidence of a product memory reduction.
Build times reflect different cache states and are not comparable cold builds.
Desktop/other worker load was uncontrolled, so small startup differences are
not statistically established improvements.

Fat LTO saved 8.3% daemon and 7.6% CLI bytes, but WAV encoding was slower in
the affected workload. The follow-up thin/fat p50s were 8.882/10.207 ms at 30 s
(60/20/8 samples for 5/30/120 s), and 35.319/38.319 ms at 120 s. The initial fat
120 s result was 40.379 ms versus the original thin 35.231 ms. These comparisons
do not isolate causality under shared-host load, but do not justify changing a
speed-focused app to fat LTO. Opt=s is smallest without hot-path CPU acceptance;
cgu16 increases size and startup p95 here. Retain the established profile and
explicit unwind requirement, and revisit size tradeoffs on a controlled runner.
Never choose `panic=abort` here: the coordinator
requires unwinding for TUI terminal restoration. Cargo's
[profile reference](https://doc.rust-lang.org/cargo/reference/profiles.html)
documents the optimization, LTO and panic tradeoffs; tests force unwind and
cannot establish the safety of an aborting app.

Standalone format/Clippy and the focused resampler test pass; three Python safety
tests pass. `scripts/check.sh` passes through the shared gate with `TMPDIR` set
to the worktree's `target/perf-tmp`: 31 tests pass and one isolated-bus bridge
test is ignored by the ordinary suite. An initial run failed three SQLite tests
because host `/tmp` hit its quota, and a profile compiler hit the same quota.
Moving only test/build scratch files resolved the workspace failures. No product
crate edits or integration merges were needed for this MVP baseline.

Release workspace tests also pass under the fat-LTO candidate (31 passed, one
ignored), before rejecting it on CPU evidence. The final harness safety/smoke
checks use worktree-local temporary XDG roots to avoid the shared `/tmp` quota.

`scripts/bench/mic_open.py` is provided for a later human-consented device trace
and has not been run. CPAL open/play/first-callback time cannot be inferred from
the callback API alone; it remains explicitly unmeasured rather than assigned
a fabricated generic estimate. Live accuracy, first-sample latency, injection,
Shell rendering, true wakeups and five-minute idle measurements remain follow-ups.

Revision: 2026-09-30. Performance is a release criterion. Numbers in the target table are provisional engineering budgets, not measured claims or user guarantees. The recorded headless smoke observations below measure only their stated limited workload; they do not satisfy desktop/provider release criteria. Only reports containing an actual command, build/environment, sample count and result count as measurements. See [ROADMAP.md](ROADMAP.md) for the release gate.

## Reference workload

First desktop baseline (target workload, not an observed environment): Ubuntu GNOME Wayland on a declared four-core-or-better machine with 8 GiB or more RAM, an actual built-in or USB microphone, and release binaries. Record CPU/model/RAM, kernel, Ubuntu/GNOME version, audio server/backend, device format, power profile, selected model, network location and build commit/features. The current headless report does not record RAM, GNOME session, audio backend or selected provider/model. Physical microphone and provider credential validation remain pending. Report actual measurement host details for every result.

Default cloud daemon, no optional local model, hidden idle overlay, closed TUI: the steady-state baseline. Also report daemon with active TUI and extension, recording, processing and optional sidecars as separate workloads. GNOME renderer cost belongs partly to the Shell process; daemon-only RSS does not measure total UI cost.

## Seven required metrics

| Metric | Boundary | Provisional release target | Required method |
| --- | --- | --- | --- |
| Idle RSS | Daemon resident memory after 30 s warmup, no clients/audio/request | ≤ 30 MiB; investigate > 50 MiB | `/proc/PID/status` plus `smaps_rollup` PSS; ≥ 60 samples over 5 minutes; sidecars/Shell separately |
| Idle CPU | Daemon process CPU seconds / wall seconds; 100% is one logical core | < 0.1% mean over 5 minutes | process counters or pidstat; report wakeups if profiled; warmup excluded |
| Hotkey → recording | Native shortcut receipt to first captured audio sample | p95 ≤ 100 ms; first cold device open reported separately | monotonic trace points from actual shortcut and cpal callback; 50 activations |
| Stop → transcript | Stop receipt to final usable STT text, before optional cleanup/insertion | Local overhead p95 ≤ 50 ms; cloud end-to-end goal p50 ≤ 1 s, p95 ≤ 2.5 s for 5 s audio | 30 live requests per backend/model; duration/network distributions; cleanup separately |
| Text injection | Final-text-ready to adapter completion; destination visible-text time separately | Adapter p95 ≤ 150 ms for 100-character text | 50 editor/browser/terminal trials plus Unicode/multiline cases; compositor/app observation for visible delivery |
| Overlay frame time | Renderer update/draw cost while active, with idle renderer stopped | CPU update/draw p95 ≤ 2 ms; ≤ 30 FPS active; zero scheduled idle animation | GNOME/Shell profiler and timestamps, ≥ 60 s active; frame cadence and missed frames separately |
| Daemon startup | Process spawn to successful IPC status response | Warm p95 ≤ 100 ms, cold goal ≤ 250 ms | 30 launches with independent runtime/data dirs; cold/warm cache definitions recorded |

Cloud goals depend on provider, network and audio duration. Missing the cloud goal is not evidence of a Rust CPU bottleneck. No performance comparison to Wispr or whisrs is claimed without running an equivalent controlled workload.

## Recorded headless smoke result

[linux-headless.json](docs/benchmarks/linux-headless.json) records one successful status-only smoke run. It reports daemon startup of 0.0509 s for one launch, idle RSS of 10,212 KiB (about 9.97 MiB), idle CPU of 0.0% over 10 seconds, and status IPC p50/p95 of 0.0658/0.0765 ms across 100 samples. The host was Ubuntu 26.04.1 LTS, Linux 7.0.0-34-generic, x86_64, AMD Ryzen 5 3600X, with rustc 1.98.1; the release workspace was built locked and offline. Startup has one observation; zero CPU ticks were observed over the short idle interval, which does not establish the five-minute CPU budget. Status IPC latency is not one of the seven UX metrics.

The report used:

```sh
CARGO_HOME=/tmp/xflow-cargo RUSTC=/home/mrad/.rustup/toolchains/1.98.1-x86_64-unknown-linux-gnu/bin/rustc RUSTDOC=/home/mrad/.rustup/toolchains/1.98.1-x86_64-unknown-linux-gnu/bin/rustdoc /home/mrad/.rustup/toolchains/1.98.1-x86_64-unknown-linux-gnu/bin/cargo build --release --workspace --locked --offline
python3 scripts/benchmark.py --daemon target/release/xflowd --cli target/release/xflow --output docs/benchmarks/linux-headless.json
```

The harness used temporary config/data/runtime directories with history disabled and clipboard-only configured. It exercised daemon startup, status IPC, RSS and CPU only; it did not access the microphone, call a provider, exercise a hotkey, inject text or render the overlay. The artifact records those unavailable metrics as null. It is not evidence for the hotkey-to-recording, stop-to-transcript, text-injection or overlay targets above.

## Report measurements honestly

Reports must distinguish:

- **Software smoke:** daemon idle RSS/CPU/startup and status IPC against local fixtures. Useful for the core process and reproducibility; excludes real mic, native global shortcut, live provider, paste and overlay renderer.
- **Integration timing:** mock/fixture recording-to-transcript/injection timing. Tests sequencing and local overhead; mock latency is not speech recognition latency.
- **Desktop acceptance:** physical capture, actual shortcut/overlay and confirmed destination behavior.
- **Live provider timing:** authorized cloud calls with declared model, recording lengths and location; include failed/timeout counts.

Never populate unmeasured metrics with estimates, zeroes or a mock result labelled as production. If a fixture mode bypasses audio/provider/desktop, disclose each bypass. When a benchmark temporarily uses synthetic capture, its `start` response latency cannot stand in for hotkey-to-first-sample latency.

## Reproducible collection

Build release once and record the commit, Cargo.lock, toolchain and features. Run format/lint/relevant tests separately; compilation time is not daemon startup. Use isolated configuration/runtime/database paths and synthetic nonsensitive text. Establish a baseline before changing implementation. Prefer the repository's benchmark script for exact command flags; its report must retain raw observations and computed summaries.

For Linux idle sampling, read `VmRSS` and CPU jiffies from the same daemon PID using a monotonic wall clock and the host's clock-tick frequency. Observe at 1 s or slower cadence for the external measurement harness; no sampling thread belongs in the production daemon. A short smoke interval cannot substantiate the five-minute idle CPU target. CPU rounding to 0.0% means below the sample resolution, not physically zero.

Startup measures until IPC readiness, with a timeout and counted failed launches. Report median/p95/min/max and warm/cold treatment; a single startup observation is only a smoke reading. Disk/socket/keyring setup may dominate cold startup and must remain within the defined boundary.

For session timings, record monotonic events: action receipt, capture initialization, first sample, stop receipt, capture finalized, WAV ready, request sent, final STT response, cleanup start/end, insertion start/end. Persist only durations/state categories, never audio/text/keys. Provider total is upload + network + inference + download; report cleanup cost independently.

For native overlay work, separate daemon event publication, D-Bus delivery, Shell update/draw and presentation cadence. A 33 ms timer is a refresh interval, not a measured render cost. D-Bus signal send duration cannot prove overlay frame time. Disable idle animation and levels before counting idle wakeups; render only from state/level changes while active.

## Benchmarks, profiling and accuracy

Run 5 s/30 s/maximum-duration sessions, silence/noise/quiet speech, timeout/rate-limit and repeated cancel/restart. Measure peak recording/encoding/base64 memory as well as idle. PCM f32 memory is sample_rate × channels × seconds × 4 bytes; WAV encoding, multipart copies and base64 JSON add peaks. Duration alone is insufficient if native device rate/channel count grows. Enforce a hard sample budget and reject overflow rather than transcribe silently truncated data.

Use a local controlled HTTP server for latency decomposition and response/error-size tests; use a live provider only for external timing. For regressions, Linux `perf`, heap/allocation profiling and flamegraphs are useful when available. Profiling is follow-up work, not proof that checks passed just because tools were named. Isolate permission-dependent profilers from ordinary runtime.

Pair speed results with an accuracy corpus. Include multilingual phrases, developer identifiers, names, fillers, false starts and negation; preserve fixture licensing. Measure word/character error where appropriate and human-reviewed meaning preservation after cleanup. A faster transcription that loses terms or changes meaning fails product acceptance.

## Regression gates

On the same host/build mode, investigate > 10% repeatable regressions in startup, steady idle memory, local stop overhead or insertion, and any new idle timer/CPU activity. Re-run only affected workloads after the fix. Cloud variability needs repeated distributions and failure rates before assigning cause. Keep initial CI deterministic: protocol bounds, cancellation and fixture checks. Hardware/cloud thresholds belong in scheduled/manual acceptance until a stable runner exists.

A release report includes all seven metrics, with unmeasured entries explicitly marked and reasons. A development scaffold can publish partial smoke evidence while leaving desktop release gates open; [ROADMAP.md](ROADMAP.md) makes that distinction explicit.
