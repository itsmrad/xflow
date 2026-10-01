# xflow v1 shared contracts

Revision: 2026-10-01. Authoritative interface between the daemon, CLI, TUI, providers, platform
adapters and the GNOME extension. Code is the source of truth for exact types:
`crates/xflow-core/src/{lib,config,ipc}.rs`, `crates/xflow-platform/src/bridge.rs`. Change a contract
only through the v1 orchestrator so every consumer moves together.

## Configuration (`~/.config/xflow/config.toml`)

Strict TOML (`deny_unknown_fields`), every section optional, validated by `Config::validate()`.
`config/default.toml` is the documented template and must deserialize to `Config::default()`
(enforced by a test). MVP configs keep loading (`injection.clipboard_only` is a deprecated alias).

| Section | Keys | Consumer |
| --- | --- | --- |
| `[stt]` | provider, model, endpoint, protocol, api_key_env, language, vocabulary, timeout_secs | providers |
| `[cleanup]` | mode (raw/light/polished/custom), provider, endpoint, model, api_key_env, prompt, timeout_secs, app_context | providers, daemon |
| `[recording]` | max_seconds, silence_threshold, device, min_ms, auto_stop_secs, keep_warm_secs | platform audio, daemon |
| `[injection]` | method (paste/type/clipboard), clipboard_only, restore_clipboard, restore_delay_ms, trailing_space | platform desktop, extension |
| `[formatting]` | remove_fillers, fillers, spoken_punctuation | daemon text pipeline |
| `[dictionary]` | words, replacements = [{from, to}] | daemon text pipeline, providers (hints) |
| `[[snippets]]` | trigger, text | daemon text pipeline |
| `[[styles]]` | name, apps, mode, prompt | daemon (picks style by focused app id) |
| `[sounds]` | enabled, volume, start, stop, error | daemon/platform |
| `[notifications]` | enabled | daemon/platform |
| `[privacy]` | history, history_limit, offline | daemon, providers |
| `[ui]` | theme, themes = {name = {slot = color}}, mouse | TUI only |

Overlay appearance and global shortcuts are **not** in TOML: they are GSettings keys of
`org.gnome.shell.extensions.xflow` owned by the extension (CLI/TUI edit them via `gsettings`).
CLI/TUI edit TOML with comment-preserving writes (toml_edit), validate by deserializing the edited
document, write atomically (temp file + rename, mode 0600), then send `Reload`.

## Unix-socket IPC (`$XDG_RUNTIME_DIR/xflow/daemon.sock`)

Newline-delimited JSON, one request per connection (except `subscribe`), ≤ 64 KiB per frame.
`PROTOCOL_VERSION = 2`; `status` reports `version` and `protocol`.

| Request (`"command"`) | Fields | Reply / semantics |
| --- | --- | --- |
| `status` | — | state, level, version, protocol, provider, model, mode (while active) |
| `start`, `toggle` | `mode` ("dictation"/"command", default dictation), `context` (AppContext captured by the caller), `t0_us` (caller CLOCK_MONOTONIC µs at hotkey), `delivery` ("inject" default / "clipboard" / "none" — none leaves the desktop untouched so a subscribed client such as `xflow listen` can print the text) | Mic opens **before** any focus query; provider connection is warmed in the background |
| `stop`, `cancel` | — | unchanged |
| `last`, `copy_last`, `paste_last` | — | unchanged |
| `history` | `limit`, `offset` (default 0), `query` (case-insensitive substring) | `history` rows newest first, `total` = matching rows |
| `history_get` / `history_delete` / `history_copy` / `history_paste` | `id` | `entry` for get; injection outcome for paste |
| `clear_history` | — | unchanged |
| `stats` | — | `stats` (sessions, words, audio_ms, today, streak, wpm, latency p50/p95) |
| `reload` | — | re-read config; on error keep the running config and return the error |
| `subscribe` | — | snapshot, then every state/level event; the final event of a successful session carries `text`, `injection`, `entry`, `timings` |
| `shutdown` | — | unchanged |

`HistoryEntry` adds model, raw_text, app_id, language, duration_ms, latency_ms, mode. `Timings` carries
hotkey_ms, open_ms, audio_ms, stt_ms, cleanup_ms, inject_ms, total_ms. All new response fields are
optional on the wire. Requests marked "not available yet" in the contract branch are implemented by
the daemon worker.

## D-Bus (session bus)

**`org.xflow.Daemon`** at `/org/xflow/Daemon` (owned by `xflowd`):

- `Command(s request_json) → s response_json` — same JSON as the socket, restricted to
  `status, start, stop, toggle, cancel, copy_last, paste_last`. Errors: `InvalidArgs` (bad JSON),
  `AccessDenied` (request not allowed over D-Bus), `Failed` (daemon stopping).
- signal `Event(s json)` — `DesktopEvent {state, level, message, mode}`; levels ≤ 30 Hz while listening.

**`org.xflow.Shell`** at `/org/xflow/Shell` (owned by the GNOME extension):

- `Context() → s` — `{"app_id": s|null, "window_id": s|null, "selected_text": null}`.
- `Inject(s text, s options_json) → s result_json` — options
  `{"method": "paste"|"type", "terminal": bool, "restore_clipboard": bool, "restore_delay_ms": u32,
  "target": {"app_id", "window_id"}|null}`; result `{"outcome": "pasted"|"typed"|"clipboard_only",
  "message": s|null}`. Pastes with Ctrl+V (Ctrl+Shift+V when `terminal`) through a Clutter virtual
  keyboard after setting the clipboard; `clipboard_only` when the focused window no longer matches
  `target`, focus is unknown or the screen is locked. Restores the previous clipboard text after
  `restore_delay_ms` only if the clipboard still holds the injected text.
- `Selection() → s` — `{"text": s|null}` (PRIMARY selection, ≤ 64 KiB).
- `Version() → s`.

Rust platform code prefers `org.xflow.Shell.Inject` when the name has an owner and falls back to
`wl-copy` + `ydotool` (Wayland) or `xclip` + `xdotool` (X11). The terminal list and multiline-terminal
safety check stay in Rust (`safe_target`).

## Traits (`xflow-core`)

- `SpeechToText::model()` (display) and `warm()` (called at recording start; pre-connect only, no
  audio, no billing; errors ignored).
- `TextTransformer::transform(TransformRequest)` — text, mode, instructions (cleanup/style prompt),
  command (command mode), app_id (only with `cleanup.app_context`), vocabulary (dictionary words).
- `Desktop::selection()` — PRIMARY selection for command mode (default `None`).
- `InjectionOutcome::Typed`, `CleanupMode::Custom`, `Mode::{Dictation, Command}`.

## Ownership map (v1 workers)

| Area | Owner branch |
| --- | --- |
| Providers crate: 10+ STT providers, model catalog, key status/check, cleanup presets, resample/encode, HTTP/2, warm(), key caching | `itsmrad/feat-v1-providers` |
| Platform crate: audio devices/keep-warm/levels, injection methods + `org.xflow.Shell` client + fallbacks, selection, sounds, notifications | `itsmrad/feat-v1-platform` |
| Daemon + store + text pipeline: new IPC requests, history v2/stats, reload, dictionary/snippets/fillers/styles, command mode, auto-stop, timings | `itsmrad/feat-v1-daemon` |
| CLI (`xflow`) incl. setup wizard, config editing, completions, service/extension install | `itsmrad/feat-v1-cli` |
| TUI | `itsmrad/feat-v1-tui` |
| GNOME extension (pill, shortcuts, prefs, `org.xflow.Shell`) | `itsmrad/feat-v1-overlay` |
| CI, release automation, e2e harness | `itsmrad/feat-v1-ci` |
| Benchmarks, release profile, PERFORMANCE.md | `itsmrad/feat-v1-perf` |
