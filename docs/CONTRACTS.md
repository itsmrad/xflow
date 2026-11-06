# xflow v1 shared contracts

Revision: 2026-10-02. Authoritative interface between the daemon, CLI, TUI, providers, platform
adapters and the GNOME extension. Code is the source of truth for exact types:
`apps/cli/crates/xflow-core/src/{lib,config,ipc}.rs`, `apps/cli/crates/xflow-platform/src/bridge.rs`. Change a contract
only through the v1 orchestrator so every consumer moves together.

## Configuration (`~/.config/xflow/config.toml`)

Strict TOML (`deny_unknown_fields`), every section optional, validated by `Config::validate()`.
When `privacy.history = true`, `history_limit` must be positive. Zero is accepted only with
history disabled; it must never silently erase saved rows at startup.
`config/default.toml` is the documented template and must deserialize to `Config::default()`
(enforced by a test). MVP configs keep loading (`injection.clipboard_only` is a deprecated alias).

| Section | Keys | Consumer |
| --- | --- | --- |
| `[stt]` | provider, model, endpoint, protocol, api_key_env, language, languages, vocabulary, domain_hints, timeout_secs, fallback (one catalog id, opt-in) | providers |
| `[cleanup]` | mode (raw/light/polished/custom), provider, endpoint, model, api_key_env, prompt, timeout_secs, app_context, rewrite_selection | providers, daemon |
| `[recording]` | max_seconds, silence_threshold, device, avoid_bluetooth, min_ms, auto_stop_secs, keep_warm_secs, ignore_speakers | platform audio, daemon |
| `[hotkey]` | key, double_tap_ms (500), hold_threshold_ms (500), enabled (true) | macOS event tap, daemon |
| `[injection]` | method (paste/type/clipboard), clipboard_only, restore_clipboard, restore_delay_ms, trailing_space, smart_spacing | platform desktop, extension |
| `[formatting]` | remove_fillers, fillers, spoken_punctuation, lists | daemon text pipeline |
| `[dictionary]` | words, replacements = [{from, to}], auto_learn, learned, never_learn | daemon text pipeline and correction learning, providers (hints) |
| `[vocab]` | dev (true), corp (true), sheet (false), finance (false); dev also on in terminals/editors, sheet in spreadsheet apps | daemon text pipeline, providers (hints with `stt.domain_hints`) |
| `[[snippets]]` | trigger, text | daemon text pipeline |
| `[[styles]]` | name, apps, mode, prompt | daemon (picks style by focused app id) |
| `[sounds]` | enabled, volume, start, stop, error | daemon/platform |
| `[notifications]` | enabled | daemon/platform |
| `[privacy]` | history, history_limit, offline, keep_failed_audio, window_terms | daemon, providers |
| `[ui]` | theme, themes = {name = {slot = color}}, mouse | TUI only |

Linux overlay appearance and shortcuts remain GSettings keys of
`org.gnome.shell.extensions.xflow` owned by the extension (CLI/TUI edit them via `gsettings`).
macOS uses `[hotkey]` for global input and a separate `overlay.json` appearance file
behind the same `xflow overlay` API. See [hotkey behavior](hotkeys.md); a hotkey reload
whose replacement event tap fails keeps the old binding and reports a status warning.
CLI/TUI edit TOML with comment-preserving writes (toml_edit), validate by deserializing the edited
document, write atomically (temp file + rename, mode 0600), then send `Reload`.

`stt.languages` stores up to three distinct language/region BCP-47 codes (two-letter
base languages, optional region; script/extension tags refused). Regions currently
use the base language. Empty means automatic detection, one language pins it, and a
set takes the adapter's multilingual path. Legacy `stt.language` remains readable;
setting either control atomically replaces the other. Unsupported model/language
sets and incompatible fallback routes are rejected before save. Whisper sets omit
`language` and put language style before vocabulary inside the existing prompt
budget. Reload rebuilds STT for the next dictation. No script accuracy guarantee,
romanization or quick-switch IPC is introduced; see `benchmarks/languages/README.md`.

## Unix-socket IPC (`$XDG_RUNTIME_DIR/xflow/daemon.sock`)

Newline-delimited JSON, one request per connection (except `subscribe`), ≤ 64 KiB per frame.
`PROTOCOL_VERSION = 3`; `status` reports `version` and `protocol`.

| Request (`"command"`) | Fields | Reply / semantics |
| --- | --- | --- |
| `status` | — | state, level, version, protocol, provider, model, mode (while active), optional daemon permissions (accessibility, input_monitoring, keyboard_events); last successful timings when inactive, cleared when a session fails; `learning`: count-only correction-learning diagnostics (watches, outcomes, reads, unread_max, late, merged, editing), dictionary entry counts, its `fingerprint` and whether an undo is available — never words |
| `start`, `toggle` | `mode` ("dictation"/"command", default dictation), `context` (AppContext captured by the caller), `t0_us` (caller CLOCK_MONOTONIC µs at hotkey), `delivery` ("inject" default / "clipboard" / "none" — none leaves the desktop untouched so a subscribed client such as `xflow listen` can print the text), `hold` (bool, default false: the client keeps the socket open; the daemon closes it when the session ends and cancels the session if the socket closes first, so a killed client never leaves the mic on. `xflow listen` and the TUI hold; `xflow start`, hotkeys and D-Bus don't) | Mic opens **before** any focus query; provider connection is warmed in the background |
| `stop`, `cancel` | — | unchanged |
| `last`, `copy_last`, `paste_last` | — | unchanged |
| `history` | `limit`, `offset` (default 0), `query` (case-insensitive substring) | `history` rows newest first, `total` = matching rows |
| `history_get` / `history_delete` / `history_copy` / `history_paste` | `id` | `entry` for get; injection outcome for paste |
| `history_update` | `id`, `text` | replaces the entry text (first text kept as `raw_text`); with `auto_learn`, learns close word corrections exactly like a corrected paste (see the tiers in ARCHITECTURE); command-mode entries learn nothing. Reply: updated `entry`; `message` = "Added 'X' to your dictionary" when something was learned |
| `clear_history` | — | clears persisted rows even with history disabled; also clears the in-memory last result/timings |
| `retry` | `full` (bool), `delivery` (as `start`), `discard` (bool) | idle only; transcribes the dictation kept by `privacy.keep_failed_audio` again, then cleans up and delivers it to the current focus. Parts already delivered are left out unless `full`; `discard` deletes it instead. Replies `processing`; the final event is a normal session's. Every status and event carries `failed_audio` (created_at, expires_at, audio_ms, partial) while one is kept |
| `stats` | — | `stats` (sessions, words, audio_ms, today, streak, wpm, latency p50/p95) |
| `reload` | — | idle only; stage services before swapping config; on error keep the running config and return the error; SIGHUP uses this path. A reload requested while one is in flight is queued (up to 8) and answered by one more read of config.toml |
| `undo_learned` | `op` (optional; the pill's change id, absent for the latest) | undo the latest automatic dictionary change once by restoring config.toml's exact previous bytes; refused, writing nothing, after any later save, a reload, a newer change, a restart or with a wrong `op`. `message` names the words |
| `subscribe` | — | snapshot, then every state/level event; the final event of a successful session carries `text`, `injection`, `entry`, `timings`. A learned-words event carries only `message` and leaves the state unchanged |
| `allow` | `grant` ("input_monitoring" / "accessibility" / "microphone") | macOS: xflowd asks the system, which lists xflowd in System Settings and prompts while undecided; replies with status. A microphone reply adds `permissions.microphone` (null while undecided), read by a short-lived `xflowd --microphone` child so AVFoundation never loads in the daemon |
| `shutdown` | — | unchanged |

`HistoryEntry` adds model, raw_text, app_id, language, duration_ms, latency_ms, mode, and `cleanup` (`applied`, `skipped`, `rejected`, `slow`, `failed` or `discarded`; absent when cleanup was off; history schema v3). `Timings` carries
hotkey_ms, open_ms, audio_ms, stt_ms, cleanup_ms, inject_ms, total_ms. All new response fields are
optional on the wire. macOS permission checks run in xflowd and refresh on explicit
status/subscribe/start/allow requests; a tap created before Input Monitoring was granted
is recreated on the first such request after the grant. keyboard_events is null with no tap, false before
any keyboard input, and true once the tap has received an event (not proof of a
specific Fn gesture). No permission polling runs at idle. The integrated daemon implements these requests; see [daemon behavior](DAEMON_V1.md) for retention, cancellation, delivery and timing semantics.

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
safety check stay in Rust (`safe_target`). macOS and Linux share the classifier:
macOS matches exact terminal bundle IDs/names; Linux retains WM_CLASS substrings.
macOS Console and unrelated app names containing `foot` are not terminals.

`AppContext.terminal_input` is an additive bool (serde default false) identifying
an integrated terminal from focused AX element metadata. It is not a capability
claim. macOS checks `AXTitle`, `AXDescription`, and `AXIdentifier` for known terminal
input labels, plus the `xterm-helper-textarea` entry in Chromium's `AXDOMClassList`
array (independent of localized labels); it never reads terminal contents to
classify focus. Only positively identified terminal input gets terminal guards
and read restrictions. Normal VS Code/Cursor/Zed/JetBrains buffers retain multiline
paste, selected-text commands, smart spacing, learning and paste confirmation.
Changed terminal-input classification copies only. Unrecognized integrated
terminals (including JetBrains/Zed or localized VS Code without the DOM class)
remain a #32 follow-up and retain ordinary-editor behavior. Native secure/protected
fields refuse injection; shell password prompts not exposed as secure AX fields
remain an unverified limitation.

Identified terminals skip the paste receipt read. A dispatched terminal paste
therefore reports `Uncertain`, keeps the dictation on the clipboard (no automatic
restoration), and holds the delivery lock up to `PENDING_PASTE` (currently 5 s).
This applies to terminal pastes that pass the input guard; control-character
payloads take the immediate copy-only path instead.

No live bracketed-paste capability is yet proven, so terminal newlines, tabs,
carriage returns, ESC/framing bytes and other controls still copy only. Process
names (`claude`, `codex`, `aider`) never grant a capability. Never wrap clipboard
text in raw `ESC[200~`/`ESC[201~`: the terminal must own that framing after its
foreground program enables mode 2004. Automatic Enter/send-it remains deferred.
No IPC version bump: the optional context field leaves existing enum variants
and requests unchanged, and the delivery adapter always rechecks live focus.

## Traits (`xflow-core`)

- `SpeechToText::model()` (display) and `warm()` (called at recording start; pre-connect only, no
  audio, no billing; errors ignored).
- `TextTransformer::transform(TransformRequest)` — text, mode, instructions (cleanup/style prompt),
  command (command mode), app_id (only with `cleanup.app_context`), vocabulary (dictionary words).
- `Desktop::selection()` — PRIMARY selection for command mode (default `None`).
- `Desktop::focused_text()` — focus context plus the value of the focused editable field, from one
  lookup, for correction learning (text `None` by default; macOS Accessibility only, `None` for
  secure fields and terminals).
- `learn::{Receipt, corrections, review}` and `DictionaryConfig::{adopt, promote, forget,
  fingerprint}` — pure correction learning shared by the paste observer and `history_update`.
- `read_field`, `BoundedReader`, `FieldSource`, `ReadGate` — guarded, bounded field reads and their stop protocol.
- `FieldEvent::{Changed, Focus, Merged}` — `Merged` is a value change folded into the one before it;
  it never schedules a read.
- `AudioCapture::microphone()` — last successfully opened input and selection notices; default `None`.
  Status/subscription snapshots carry the optional `microphone` string; older clients can omit it.
- `AudioCapture::finished()` — default `false`; true after capture reaches a device/buffer limit or
  fails. The daemon checks it on its existing listening-level tick, then calls `stop()` to retrieve
  the buffered clip or original error. Reset on start/stop/cancel; no new idle polling.
- `InjectionOutcome::Typed`, `CleanupMode::Custom`, `Mode::{Dictation, Command}`.
- `InjectionOutcome::Uncertain` (`"uncertain"`): macOS dispatched paste keys to a known editable,
  nonsecure, unchanged target but could not confirm insertion. The status says "Pasted — check it
  landed" (the pill stays silent); clipboard text and `last` remain available. Unknown focus is
  `ClipboardOnly`, with a short red "Copied — paste with ⌘V" pill when paste was requested. Uncertain delivery does not start correction learning.
- `GlobalHotkeys::received_events()` optionally reports whether a native tap has received keyboard input.
- `GlobalHotkeys::next_action()` adds `HandsFree` (latch notification without restarting capture);
  `reset()` clears a gesture when another control ends a session. Physical events are
  processed by the platform's pure timestamp state machine before reaching the actor.

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

### Explicit backup STT

`stt.fallback = ["openai"]` permits one backup, different from the primary, with a
stored keystore key. It uses that catalog entry's default endpoint and model.
Unknown ids and remote backups in offline mode are always rejected. Explicit
`config validate` and config saves also reject a missing backup key. Config reads,
daemon startup and reload remain usable: a missing/unreadable backup key disables
the backup and shows a doctor/status warning while the primary runs normally.
Each batch attempt (including any hint-free retry) gets
`min(stt.timeout_secs, 3 + 0.25 * audio_seconds)` seconds. At most two attempts run
sequentially, so two unavailable providers cannot consume two full default
30-second timeouts for a short clip. Async AssemblyAI upload/poll keeps its full
configured timeout instead of this batch deadline, for either route. A timed-out
upstream may still bill.

Connect/TLS errors, upload/read interruptions, 408, 429, 5xx and deadline expiration
use the other provider once. Primary 400/413 and 401/403 do not switch; the existing
400/500 hint-free retry runs first within the same deadline. A primary failure
puts the backup first for 60 seconds. While backup goes first, any backup error
probes the primary once; success clears the breaker. Otherwise the next request
after 60 seconds probes the primary. Only errors from an exhausted two-provider
chain suppress the daemon's one-time failed-segment recovery.

`Transcript.provider` is optional with a serde default for old payloads. History
reuses its existing provider column. Final status/watch JSON names the actual
provider (comma-separated ids when segments used both, with model left unset);
the success message names a backup that answered. No history migration or pill
text change is needed.

This route applies to daemon dictation. File transcription and connection checks
continue to target the selected primary provider explicitly.
