# XFlow product requirements

Revision: 2026-09-30. This document defines intended product behavior and release gates; it does not certify that every gate has passed. The current workspace contains `xflow-core`, `xflow-providers`, `xflow-platform`, and `xflow-app`; the latter builds the `xflow` CLI and `xflowd` daemon. See [PERFORMANCE.md](PERFORMANCE.md) for measured smoke results and targets, and [docs/RESEARCH.md](docs/RESEARCH.md) for source provenance.

## Implementation status (2026-09-30)

The integrated MVP implements shared Rust contracts/configuration, batch Groq/OpenRouter/custom OpenAI-compatible provider adapters, bounded cpal capture, a GNOME bridge/extension, clipboard/paste adapter, event-driven CLI/TUI updates, and SQLite WAL history (default retention 500; configurable up to 100,000). Daemon cancel aborts an active job and uses a generation guard to reject stale completion. These implementation facts do not establish desktop acceptance. The GNOME shortcut is toggle-oriented; true press-and-release GNOME push-to-talk is not implemented. Provider credentials have not been validated against live accounts, and physical microphone capture, paste delivery, overlay lifecycle and direct uinput remain pending desktop validation. Headless smoke measurements are recorded in [PERFORMANCE.md](PERFORMANCE.md); desktop and live-provider performance remain unmeasured.

## Outcome and users

XFlow is an open-source desktop dictation tool for people who write messages, documents and technical text on Linux. A small background daemon turns a deliberate recording action into text in the user's chosen app, with recoverable output when automatic paste is unavailable. The first supported release targets Ubuntu GNOME; Fedora and macOS are the next targets, with Windows kept possible through separate platform adapters.

Success is a reliable, measured microphone-to-text vertical slice with a CLI, keyboard-driven TUI and tiny native recording pill. Linux is an explicit product focus: Wispr's [official terminal guide](https://docs.wisprflow.ai/articles/6478598909-using-flow-with-linux-wsl-and-terminal-applications) currently documents no native Linux app. We do not promise all-app insertion, all-desktop hotkeys, perfect recognition or parity with every Wispr feature.

## First release scope

| Requirement | MVP behavior | Acceptance evidence |
| --- | --- | --- |
| Recording | CLI start/stop/toggle/cancel and GNOME toggle path are implemented; start/stop can be bound to press/release events where a compositor provides them, but the GNOME shortcut is not true PTT | Real mic sessions, quick taps, repeated commands, permission loss, cancellation in listening and processing |
| Recognition | Groq and OpenRouter batch adapters, BYOK; custom OpenAI-compatible REST endpoint; explicit model/language configuration | Request-contract tests plus one authorized live transcription per advertised cloud backend |
| Cleanup | Raw default; optional independent LLM transformation with light/polished policies; retain raw output when optional cleanup fails | Cleanup failure, timeout and blank-output tests; review examples for meaning/technical-term preservation |
| Audio | Native cpal capture with bounded duration/memory, basic silence gating and audio levels | Built-in and USB input; native sample-rate/channel negotiation; silence avoids a paid request |
| Insertion | Clipboard paste using an available desktop utility; clipboard-only fallback and copy/paste-last recovery | Text editor/browser/terminal checks, missing-tool failures, Unicode, multiline text and changed focus |
| Feedback | Hidden idle native pill; listening, processing, success and error; audio-reactive waveform; keyboard-accessible TUI | GNOME extension install/disable/reenable, daemon restart, no focus theft, idle animation/timer inspection |
| History | Local SQLite WAL, default retention of 500 entries (configurable up to 100,000), list/clear and disable persistence | Retention boundary, no-history behavior, restart and filesystem permissions |
| Security | OS credential storage where available, environment credentials for CLI/headless use, no keys in TOML, no default secret/audio/transcript logs | Dummy-secret redaction, socket/data modes, keyring-unavailable behavior, offline rejection |
| Performance | Establish reproducible measurements for all seven requested metrics | Report environment, build profile, sample size and distributions; label unmeasured desktop metrics |

Physical capture, direct uinput, confirmed paste and overlay lifecycle are desktop release gates. Provider adapters and credential resolution exist, but credentials have not been validated against live provider accounts. Passing mocked HTTP, fixture capture or clipboard-disabled tests establishes software behavior, not end-to-end desktop support. A failed provider request must not trigger silent provider switching or send the recording to a different service.

## Session behavior

1. The user chooses a provider/model and credentials, then starts the daemon. Settings contain references to secrets, not secret values.
2. A configured shortcut or CLI action starts capture. The pill appears without becoming the focused app. Missing mic or permission produces a visible, recoverable error.
3. Stop finalizes audio and transcribes. Raw audio remains transient; basic silence gating can end without upload. Only one session is active.
4. Optional cleanup runs after STT. Cancel aborts a pending capture/provider job and rejects stale completions. If transcription has already finalized and been saved, cancel cannot undo that history entry or the `last` recovery text; a queued cancel is handled before injection starts when possible. `clear-history` cancels active work and clears both history and the in-memory last transcript.
5. Final text is pasted or copied. The TUI/CLI distinguish those outcomes. Automatic paste cannot prove the target app accepted text; copy-last remains available after uncertain delivery.
6. Daemon `Success`/`Error` state persists until the next recording or cancel. The GNOME pill hides success feedback after 1.5 seconds and error feedback after 4 seconds without changing daemon state. History follows the explicit retention preference.

Do not automatically execute dictated commands. In terminals, newline/paste behavior and terminal-specific shortcuts must be documented and tested. Until app/focus identity is available, the MVP cannot guarantee insertion into the original app if the user changes focus during transcription.

## Provider capabilities

Provider capabilities must be based on the wire protocol and selected model. Batch uploads are not described as streaming. Multilingual support and punctuation are provider/model properties, with a language override where supported. Vocabulary hints must fail visibly or be reported unsupported if the endpoint ignores them. BYOK does not imply the provider retains no data.

[Groq](https://console.groq.com/docs/speech-to-text) and [OpenRouter](https://openrouter.ai/docs/api/api-reference/stt/create-transcription) use distinct verified request formats. Deepgram/WebSocket streaming, arbitrary custom protocol mappings and local whisper.cpp are later backends. Configuring offline mode before a local engine is present must produce an error before any network request.

## Personalization and later product scope

| Feature | Intended behavior | Stage |
| --- | --- | --- |
| Personal dictionary/corrections | Manage local words and literal whole-word corrections; separate hints from replacements | After core slice |
| Vocabulary and developer terms | Bound provider hints; preserve identifiers, casing, punctuation and code in cleanup evaluations | Basic configured hints first, richer dictionary later |
| Filler removal/backtracking | Provider transcription plus optional cleanup; preserve meaning and negation; raw bypass | Optional cleanup first; quality corpus later |
| Snippets | User-created voice triggers expand plain text with deterministic precedence and word boundaries | Later |
| App writing styles | Explicit per-app preference; unknown context uses default | Later |
| App/context awareness | Minimal app identity by default; selected/surrounding text only with explicit opt-in | Later |
| Selected-text transforms | Capture the exact selection, preview/review where needed, report stale target and transform failure | Later |
| Voice command mode | Explicit mode with named text transforms; no implicit shell execution | Later |
| Streaming | Preview partials; insert finalized transcript once; cancellation never flushes stale partial text | Later |
| Offline | Optional isolated local engine with no cloud fallback | Later |
| Auto-learning | Observe only a bounded inserted span; trust narrow repeated spelling corrections; reversible dictionary promotion | Design only now |

Wispr's [dictionary](https://docs.wisprflow.ai/articles/4052411709-teach-flow-your-words-with-the-dictionary), [snippets](https://docs.wisprflow.ai/articles/5784437944-create-and-use-snippets), [styles](https://docs.wisprflow.ai/articles/2368263928-how-to-setup-flow-styles) and [cleanup](https://docs.wisprflow.ai/articles/4283510616-auto-cleanup-control-how-much-flow-edits-your-dictation-beta) establish useful interaction patterns; they are not shipped XFlow capability claims.

## Privacy and control

Local settings, bounded history and optional future dictionaries need no account. Telemetry is off by default; initial implementation must contain no telemetry sender. No raw audio persistence by default. Users can disable transcript history and clear it. SQLite deletion is logical deletion, not a promise of forensic erasure from backups or storage media.

Cloud STT sends recorded audio to the chosen provider. Optional LLM cleanup additionally sends transcript text. Optional future context access must have a separate control and minimal scope; reading a whole window for dictation is unnecessary by default. OS credential storage falls back to explicit environment credentials when unavailable, never to writing keys into configuration. Logs may contain states/timing/error categories, never unredacted credentials, provider bodies or transcripts.

## Overlay requirements

Native GNOME Shell rendering is acceptable for the Ubuntu slice; its required extension must be documented. The overlay is always above ordinary app content, hidden while idle, and does not capture focus. Waveform updates are coalesced and active only while listening. No idle frame loop. Configurable position/size/opacity/animation are product requirements to add incrementally; unsupported controls must not appear to work. wlroots/KDE/X11/macOS renderers are separate adapters, with CLI/TUI usable when no overlay exists.

## Release criteria

A desktop MVP release requires the acceptance rows above, a documented supported Ubuntu/GNOME version, setup/diagnostic guidance for audio/credentials/paste utilities/extension, recoverable errors under provider/network failure, and published measured performance. Smoke measurements may accompany a development scaffold, but must not be presented as evidence of latency through a physical mic, global hotkey, live provider or compositor. Fedora/macOS become supported only after their own recorded acceptance results. See [ROADMAP.md](ROADMAP.md) for the staged gates.
