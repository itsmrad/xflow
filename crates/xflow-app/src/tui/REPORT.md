# XFlow v1 TUI implementation and verification

The TUI is a keyboard and mouse control center with ten screens, staged configuration edits,
live subscription updates, searchable history, secure provider-key forms, and six themes.
Public entry point remains `xflow_app::tui::run()`; the CLI owner provides `xflow tui`.

## Recovery and ownership

Recovered predecessor session `104b929f-ace5-486e-a66c-f501566aa0a6`. It was the original
orchestrator session; the TUI specification had been written but its worker had not launched.
Implementation began on the clean assigned contracts checkpoint and stayed on
`itsmrad/feat-v1-tui`. Codex produced the TUI implementation; commits have no false Claude
co-author attribution. The recovery continuation preserved all five pending Rust files at
`ba354ea` and completed their missing-key onboarding and verification.

TUI source is isolated under `src/tui/`, with the public entry in `src/tui.rs`. Shared
`transport.rs`, `config_edit.rs`, and `gsettings.rs` are reused; changes to those helpers and
other components arrive only through coordinator-authorized dependency commits.
Ratatui 0.29 and Crossterm 0.28 were deliberately retained with coordinator approval.
The only additional TUI dependency is `zeroize` for secret form input and payloads.

## Behavior

| Screen | Available actions and information |
| --- | --- |
| Home | State in text and symbols, live waveform, dictation/command mode, provider/model, transcript, timings, daemon version/protocol, connection and guided onboarding |
| History | Debounced server substring search, 30-row pages, final/raw text and app/language/timing details, copy/paste/delete/export |
| Dictionary | Add/edit/delete words and replacements with schema validation |
| Snippets | Add/edit/delete trigger and multiline text; duplicate-trigger validation |
| Styles | Name, matching apps, cleanup mode and prompt forms |
| Providers | Real STT and cleanup catalogs, environment/keyring/missing credential status, masked set/delete key, model selection and non-billing connection check |
| Settings | Every effective TOML key and optional fields; booleans/enums/range hints, schema validation, reset, staged save/revert and reload |
| Overlay | Extension schema discovery and type-aware GNOME GSettings editors, including appearance and shortcuts |
| Stats | Sessions/words/today/streak/WPM, latency percentiles, estimated time saved and recent session-word sparkline |
| Doctor | Config, daemon-related environment, extension/schema and clipboard/paste tool availability with fix hints |

Changing a provider clears routing/model/protocol/key-environment overrides for the old preset.
Choosing a highlighted preset's model activates that preset. Connection checks use the highlighted
preset without changing staged config. Cleanup activation preserves its chosen cleanup mode;
set that mode in Settings. Catalogs display immediately while credential status resolves.
Missing built-in cloud-provider credentials trigger Home onboarding even when config and daemon
already exist. Older credential lookups cannot overwrite a subsequently edited STT route.

Config changes remain staged until Ctrl+S; helper validation and a disk conflict check prevent
invalid writes or overwriting externally edited config. Failed saves retain the draft, which
can be exported. Shared atomic saves produce mode-0600 files. Overlay GSettings and key storage
are separate, immediate operations. Deletions and discarding unsaved edits require confirmation.

## Keymap

| Keys | Action |
| --- | --- |
| `1/h`, `2/H`, `3/d`, `4/n`, `5/y` | Home, History, Dictionary, Snippets, Styles |
| `6/v`, `7/s`, `8/o`, `9/a`, `0/D` | Providers, Settings, Overlay, Stats, Doctor |
| Tab / Shift+Tab | Next / previous screen |
| Arrow keys, j/k, Home/End | Select rows; k on Providers opens the masked key form |
| Space / C / x | Toggle dictation / start command mode / cancel |
| c / p / l | Copy / paste / retrieve last transcript; history uses the selected row |
| N / A / Enter / Delete | Add / add dictionary replacement / edit or activate / delete |
| `/`, PgUp/PgDn | History search, previous/next page |
| `[` / `]` | Scroll detail panes |
| e | Export selected history JSON; elsewhere export config draft to a new file |
| Ctrl+S / Ctrl+R / u | Save and reload / revert draft / reset selected TOML setting |
| b / m / k / K / T | STT-cleanup catalog / model / set key / delete key / connection check |
| i / r / t | Input device chooser / refresh screen / live theme chooser |
| `?`, Ctrl+P or `:` | Help overlay / fuzzy action palette |
| q or Ctrl+C | Quit with unsaved-edit protection |

Forms support UTF-8 cursor movement, Home/End, Backspace/Delete, Ctrl+U to clear, bracketed paste,
Tab/Shift+Tab between fields and arrow cycling of choices. Ctrl+J inserts a newline.
F2, Ctrl+Enter, or Ctrl+S submits a form; Enter submits a single field or advances to the next.
Esc closes forms/help without applying their edits. Mouse tabs, row selection, action buttons
and wheel scrolling follow `ui.mouse`; modal screens block background hit targets.

## Themes and responsive rendering

Built-ins: **paper** (default off-white/near-black), **midnight**, **catppuccin**, **nord**,
**gruvbox**, **high-contrast**. The live theme picker includes user definitions in `[ui.themes]`.
Slots: `bg`, `fg`, `muted`, `accent`, `error`, `selection`; validated hex or named colors.
Edit the effective `ui.themes` table in Settings to add a theme, or edit the config externally
and reload. Invalid theme edits roll back; invalid loaded themes fall back to paper with a notice.

Truecolor terminals use the full palette; other terminals receive a 16-color fallback.
`NO_COLOR` restores default terminal colors. State text, symbols, bold/reverse selection and
validation messages avoid color-only signaling. At 120x35, list/details can sit side by side;
at 80x24 they stack. Below 35 columns or 12 rows, a resize/quit fallback replaces the full UI.
All terminal-cell symbols are sanitized against control-byte injection from transcripts/config.

## Verification

- `cargo test -p xflow-app tui --locked` through the shared Cargo gate: **17 passed**.
  Covers reducer/forms, validation, stale search/detail/command/key results, real preset/model
  activation, anonymous loopback GET probe, save conflicts, private exports, masked keys,
  Unicode cursor editing, modal/mouse behavior, fragmented subscription frames and reconnect.
- Six exact TestBackend snapshots: Home, History, Settings at **120x35 and 80x24**.
  All ten screens additionally render at both sizes and at 34x11 and 1x1.
- `cargo build -p xflow-app --bin xflow --locked` through the shared Cargo gate.
- `python3 crates/xflow-app/src/tui/smoke.py`: four real-binary PTY runs, online/offline at
  **80x24 and 120x35**, with private XDG roots, unavailable private session bus and mock IPC.
  Checks dictation state events, history query, stats, word/multiline snippet save, theme save,
  reload requests, tiny resize, clean quit, original termios and alternate-screen restoration.
  Both connected one-second idle samples measured **0 CPU ticks**.
- Final isolated `scripts/check.sh`: pending authorized CI mock warm-up fixture adaptation.
  Prior check passed fmt, Clippy with warnings denied, 38 extension tests and all app unit tests;
  the daemon upload-only mock rejected the provider's legitimate GET /v1/models warm-up.

The UI awaits subscription/input/background jobs; animation ticks are enabled only during
listening/processing. Connected idle has no polling timer. Disconnected reconnect uses bounded
250ms–8s backoff. File/settings/device/diagnostic operations run off the UI task; provider calls
are asynchronous. A panic hook and RAII guard restore raw mode, mouse capture, bracketed paste,
alternate screen and cursor. Tests never use the real daemon, audio stream, clipboard, desktop
key synthesis, real keyring service, API keys or paid provider calls.

## Text screenshots

These are exact committed TestBackend text snapshots; colors are omitted by the text backend.
Full 120x35 snapshots are in `snapshots/` alongside these 80x24 examples.

### Home at 80x24

```text
 xflow                ╭Control center──────────────────────────────────────────╮
 Voice, at your pace. │Home  ·  Connected                                      │
                      ╰────────────────────────────────────────────────────────╯
╭Navigate─────────╮╭Live session───────────────────────────────────────────────╮
│› 1 Home         ││ ✓ DONE                                                    │
│  2 History      ││ Dictation  ·  groq / whisper-large-v3-turbo               │
│  3 Dictionary   ││ Space starts / stops · C command mode · x cancels         │
│  4 Snippets     ││                                                           │
│  5 Styles       ││                                                           │
│  6 Providers    │╰───────────────────────────────────────────────────────────╯
│  7 Settings     │╭Last transcript · PgUp/PgDn scroll─────────────────────────╮
│  8 Overlay      ││The best ideas start with a conversation.                  │
│  9 Stats        ││                                                           │
│  0 Doctor       ││                                                           │
│                 ││                                                           │
│                 │╰───────────────────────────────────────────────────────────╯
│                 │╭Latency & connection───────────────────────────────────────╮
│                 ││Session timing appears after dictation                     │
│                 ││Daemon 0.1.0 · protocol 2 · Connected                      │
╰─────────────────╯╰───────────────────────────────────────────────────────────╯
 Space Dictate   C Command   x Cancel   c Copy   p Paste

? Help  ·  Ctrl+P Actions  ·  t Theme  ·  Ctrl+S Save  ·  q Quit

```

### History at 80x24

```text
 xflow                ╭Control center──────────────────────────────────────────╮
 Voice, at your pace. │History  ·  Connected                                   │
                      ╰────────────────────────────────────────────────────────╯
╭Navigate─────────╮╭History 1–1 / 1 · / ───────────────────────────────────────╮
│  1 Home         ││› The best ideas start with a conversation.                │
│› 2 History      ││  #42  groq  430ms                                         │
│  3 Dictionary   ││                                                           │
│  4 Snippets     ││                                                           │
│  5 Styles       ││                                                           │
│  6 Providers    ││                                                           │
│  7 Settings     │╰───────────────────────────────────────────────────────────╯
│  8 Overlay      │╭Details · [ / ] scroll─────────────────────────────────────╮
│  9 Stats        ││The best ideas start with a conversation.                  │
│  0 Doctor       ││                                                           │
│                 ││#42 · groq · whisper-large-v3-turbo                        │
│                 ││App: code · language: en                                   │
│                 ││Audio: 2800ms · latency: 430ms                             │
│                 ││Mode: Dictation · timestamp: 1790859600                    │
│                 ││                                                           │
╰─────────────────╯╰───────────────────────────────────────────────────────────╯
 / Search   c Copy   p Paste   Del Remove   e Export

? Help  ·  Ctrl+P Actions  ·  t Theme  ·  Ctrl+S Save  ·  q Quit

```

## Scope limits and remaining acceptance

- Actual microphone/delivery/keyring writes and the user's live GNOME preferences were deliberately
  not exercised. Provider probes in verification use anonymous loopback; provider-owner tests cover
  adapters. Real cloud inference and native desktop acceptance remain integration checks.
- One-second idle CPU samples are evidence for these mock sessions, not a universal speed claim.
- Time saved uses a labeled 40 typed-WPM estimate. Activity shows the most recent 30 session word
  counts; the IPC contract does not provide calendar-bucket analytics.
- Custom/anonymous server credentials are resolved by the explicit connection check; automatic
  missing-key onboarding applies to built-in cloud presets. Catalog metadata is owned by providers.
- Doctor presents actionable command hints; it does not install services, tools or an extension.
- Arrays/compound TOML values use validated text editors rather than bespoke editors for every shape.
- Terminals narrower than 35x12 receive the safe resize fallback. Font glyph widths and truecolor
  appearance still warrant visual review in the user's preferred emulator.

The coordinator owns final component integration, approvals and interface decisions.
