# Performance methodology and budgets

Revision: 2026-09-30. Performance is a release criterion. Numbers in the target table are provisional engineering budgets, not measured claims or user guarantees. No desktop or provider performance metric has been measured for this work; the entries below are targets and the prescribed collection methodology. Only reports containing an actual command, build/environment, sample count and result count as measurements. No benchmark report artifact is checked in yet; see [ROADMAP.md](ROADMAP.md) for the acceptance gate.

## Reference workload

First desktop baseline (target workload, not an observed environment): Ubuntu GNOME Wayland on a declared four-core-or-better machine with 8 GiB or more RAM, an actual built-in or USB microphone, and release binaries. Record CPU/model/RAM, kernel, Ubuntu/GNOME version, audio server/backend, device format, power profile, selected model, network location and build commit/features. Actual microphone and provider credential validation remain pending. This is a planned baseline; report actual measurement host details for each measured result.

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
