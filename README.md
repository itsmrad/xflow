# xflow

Native, local-first dictation for macOS and Linux, written in Rust. This is a v1 development build with macOS Accessibility/CGEvent delivery and Ubuntu GNOME integration; Fedora and Windows desktop validation remain future work. It is not yet a production release; real desktop and provider acceptance tests remain required.

The workspace includes a small daemon, clap CLI, event-driven ratatui TUI, CPAL microphone capture, ten cloud batch transcription adapters plus custom/local servers, optional LLM cleanup, dictionary/snippets/per-app styles, SQLite history and statistics, and a native GNOME Shell recording pill. No Electron, WebView, telemetry, or resident local model runtime.

macOS now has configurable Fn/Globe hold-to-talk and double-tap hands-free input,
with an on-demand recording pill. See [hotkey setup and verification](docs/hotkeys.md)
for Input Monitoring, the Globe keyboard setting, live rebinding and current manual checks.

## Repository layout

- `apps/cli/`: Rust workspace (`Cargo.toml`, `Cargo.lock`, and `crates/`); build output is `apps/cli/target/`.
- `apps/web/`: reserved for the web application, supplied by a separate PR.
- `packages/`: reserved for shared packages; empty until needed.
- `scripts/`, `config/`, `packaging/`, and `docs/`: shared tooling, assets, and documentation.

The root `package.json` declares Bun workspaces for `apps/*` and `packages/*`.
Commands below run from the repository root. For direct Cargo commands, use
`--manifest-path apps/cli/Cargo.toml` or run Cargo from `apps/cli`.
`scripts/check.sh`, the installer, and the package scripts run from any directory.

## Build and run

The locked dependency set declares Rust 1.88 as its highest minimum compiler version (notably `instability`, `darling`, and ICU crates). Rust 1.88 is the declared lower bound; CI has not separately tested that compiler. Native development libraries are also required:

```sh
# Ubuntu
sudo apt install build-essential pkg-config libasound2-dev libdbus-1-dev wl-clipboard
# Fedora (build recipe; desktop validation is still pending)
sudo dnf install gcc gcc-c++ pkgconf-pkg-config alsa-lib-devel dbus-devel wl-clipboard
cargo build --manifest-path apps/cli/Cargo.toml --release --locked
install -Dm755 apps/cli/target/release/xflow ~/.local/bin/xflow
install -Dm755 apps/cli/target/release/xflowd ~/.local/bin/xflowd
xflow init
xflow key groq                 # hidden input; encrypted in xflow's keystore, no OS prompt
# Alternatively set GROQ_API_KEY in the daemon's environment.
xflowd                        # separate terminal; reads ~/.config/xflow/config.toml
```

```sh
xflow doctor
xflow toggle                  # start; repeat to stop and transcribe
xflow status
xflow cancel                  # discard capture or cancel pending transcription
xflow last
xflow copy-last
xflow paste-last
xflow history -n 10
xflow clear-history
xflow watch                   # event stream; --json is also available
xflow                         # in a terminal: setup until done, then the control centre (= xflow tui); ? opens the keymap
xflow shutdown
```

`stop` acknowledges processing immediately. Subscribe with `watch` or `tui` for the final result. The transcript is retained before injection; `last` recovers it if desktop integration fails. Dictations run up to 20 minutes (`recording.max_seconds`); long ones are transcribed in 30–60 s segments while you speak, so stop only waits for the last segment. Cancel discards the transcript, but segments of a dictation longer than a minute have already been sent to the provider. The microphone closes outside recording unless recording.keep_warm_secs is explicitly enabled. Before upload, 20 ms RMS frames must contain at least 80 ms above the configurable `recording.silence_threshold` floor (0.005 by default). Silence and brief clicks end with “Didn't catch anything”; vocabulary echoes and common silence hallucinations from clips that never get louder than 8x that floor are discarded before history or delivery. Audio remains in memory and is never saved or logged by default. `recording.ignore_speakers` is off by default (opt in with Settings → Speech → Ignore speaker sound, or `xflow config set recording.ignore_speakers true`). It reduces music, videos and calls playing on the computer in dictation: on macOS xflow records through Apple's voice processing (the FaceTime echo canceller), and falls back to the plain microphone if that cannot start or `recording.device` names a specific microphone. Voice processing can take longer to start and may briefly let new playback through while it adapts. On Linux, make the PipeWire/PulseAudio echo-cancel source your default input; `xflow doctor` shows how when this setting is on. The toggle does not change Linux audio routing.

Daemon status retains `success` or `error` until the next recording or cancel. The GNOME pill hides its feedback after a short display; that visual reset does not change daemon status.

Cancel discards an active capture or pending provider job. Once a transcript has been finalized and saved, cancel cannot undo its history entry; a queued cancel is handled before injection starts when possible. `clear-history` cancels active work before clearing persisted history.

GNOME Wayland needs the bundled Shell extension for native shortcuts, focus discovery and overlay. Follow [platform setup](docs/platform-setup.md) to install it and configure clipboard/uinput support. The extension implements toggle, hold-to-talk and smart press/release modes; physical shortcut and destination acceptance remain release gates. CLI `start`/`stop` can be bound to press/release events on compositors that expose them. Unavailable paste or changed/unknown focus falls back to copying; automatic multiline paste into terminals is refused.

## macOS

Build with `cargo build --manifest-path apps/cli/Cargo.toml -p xflow-app --locked` (Xcode Command Line Tools required), then install both `xflow` and `xflowd` beside each other, for example in `~/.local/bin`. Run `xflow` and `xflow doctor`. In a terminal, plain `xflow` opens the TUI, which walks through setup until it is complete; `xflow setup` reopens the guided setup even on a configured install and starts the LaunchAgent if xflowd isn't running (speech service, key, microphone, shortcut, then permissions); `xflow setup --yes` with flags is the scripted path.

Installer package: `scripts/package-macos-pkg.sh` builds release binaries and writes `dist/xflow-<version>.pkg` (pass a directory holding prebuilt `xflow`/`xflowd` to skip the build; `OUT=<dir>` changes the output directory). It installs both into `/usr/local/bin`, signed as `com.xflow.xflowd`/`com.xflow.xflow` with the `xflow-dev` certificate, hardened runtime and the microphone entitlement (`SIGN=<identity>` picks another and fails if it is missing, `KEYCHAIN=<path>` looks it up in that keychain file only; without `xflow-dev` it warns and signs ad-hoc); the scripts and pages live in `packaging/macos/`. Its postinstall moves older `~/.local/bin` copies to `~/.local/share/xflow-backup/`, and installs and starts the LaunchAgent for the logged-in user. It opens no window: the last installer page says “Open Terminal and type: xflow”, which runs setup (the full setup on a first install, otherwise only what is missing) and ends with a “hold Fn and say something” test.

The LaunchAgent is `~/Library/LaunchAgents/com.xflow.daemon.plist`; installation replaces/adopts an existing file with that label. It uses RunAtLoad and KeepAlive, and writes logs to `~/Library/Logs/xflowd.log`. `xflow daemon start|stop|restart|status` uses `launchctl` in your graphical login; stop unloads the agent so KeepAlive cannot restart it. `xflow daemon logs --follow` tails its log. `daemon start` reinstalls a plist whose binary no longer exists. Uninstall with `xflow service uninstall`; logs are retained. Store provider keys with `xflow key set`, since launchd does not inherit shell environment variables. Keys are encrypted at rest in xflow's own 0600 keystore, which guards against casual disclosure but not against malware running as you or a backup that holds both files; it never shows a Keychain prompt. See [docs/credentials.md](docs/credentials.md).

xflowd needs **Accessibility** (typing into apps; it also lets the shortcut's listen-only event tap hear keys) and **Microphone**; **Input Monitoring** is only a fallback, asked when the shortcut still isn't heard with Accessibility on (a status taken after a key was typed in the TUI shows no keyboard events). xflowd asks for nothing at start, so no prompt appears mid-install. `xflow` (the TUI) opens a permissions step by itself when one is off and asks one grant per screen, through xflowd: it opens the exact System Settings pane — find xflowd in the list and switch it on; you never add it yourself — and moves on once the switch flips, ending with a shortcut test. The hint about old, switched-on xflowd rows appears only after about 10 s of waiting or after Enter. With Fn as the shortcut, a Globe step follows unless System Settings › Keyboard › “Press 🌐 key to” is already Do Nothing (`AppleFnUsageType` 0); otherwise each press also opens emoji or switches input source. On a configured install it also checks the microphone once (no prompt when already allowed), so a reinstall shows only what is missing and nothing is asked at the first dictation. In Check-up, press Enter on any permission row to do the same. When xflowd isn't running, the TUI starts it through launchd itself (as `xflow daemon start` does), shows “Restarting xflow…” while an ask-again restart reconnects, and offers Enter to retry if starting failed (so `xflow daemon stop` while a TUI is open starts it again). A grant revoked later is never prompted for by xflowd: the next `xflow` offers the permissions step. A shortcut tap created before Accessibility (or Input Monitoring) was granted never sees keys; xflowd replaces it on the next status, Check-up or setup request after the grant, without a restart. Foreground execution may instead require grants for your terminal. Status, doctor and the TUI Check-up report xflowd's own checks without prompting; a created event tap with no keyboard events is reported as unverified.

Reinstalls and old builds: macOS lists a bare executable like xflowd by path and checks its code signature, so every older copy (`~/.local/bin`, earlier builds) can leave a look-alike “xflowd” row that is switched on but no longer matches. `tccutil reset` only accepts app bundle identifiers, so xflow cannot remove these rows itself. If a permission stays off although xflowd looks on, select each xflowd row, click −, then press Enter on it in Check-up (or in the setup permissions step) and switch on the row that appears. macOS asks only once per process, so Enter restarts xflowd (`launchctl kickstart -k`), which asks again as it starts, then reopens the pane.

Privacy grants follow the code signature's designated requirement. Ad-hoc, that is the build's hash (`cdhash H"…"`), so every rebuild leaves a stale, switched-on row; with a certificate it is `identifier "com.xflow.xflowd" and certificate leaf = H"…"`, stable across builds (check with `codesign -dr - "$(command -v xflowd)"`). Provider keys don't depend on the signature: they live in xflow's keystore, not the Keychain (keys older builds left there move once; see [docs/credentials.md](docs/credentials.md)). `xflow service install` re-signs `xflowd` with identifier `com.xflow.xflowd` (keeping a signature that already has it, such as the installer's), using a code-signing certificate named `xflow-dev` when one exists. Create it once (`security find-identity -p codesigning` then lists it as `CSSMERR_TP_NOT_TRUSTED`, which is fine for permissions):

```sh
P=$(openssl rand -hex 16)
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=xflow-dev" -keyout k.pem -out c.pem \
  -addext "basicConstraints=critical,CA:false" -addext "keyUsage=critical,digitalSignature" -addext "extendedKeyUsage=critical,codeSigning"
openssl pkcs12 -export -legacy -inkey k.pem -in c.pem -name xflow-dev -out x.p12 -passout pass:$P
security import x.p12 -k ~/Library/Keychains/login.keychain-db -P $P -T /usr/bin/codesign -T /usr/bin/security
rm -P k.pem x.p12; rm c.pem
```

Back up the `xflow-dev` identity (Keychain Access › export as .p12) before shipping a release with it: the requirement pins its certificate, so a new one resets every user's grants on their next update.

Delivery uses the frontmost app's bundle ID/name, window and focused Accessibility element. With the destination focused, trigger recording and finish it with `xflow toggle`; paste uses synthetic Cmd+V at the cursor, with the key that types "v" under Command in the current keyboard layout. `injection.method = "type"` sends Unicode key events, and `clipboard` copies only. Electron/Chromium apps are asked to expose Accessibility (`AXManualAccessibility`); when the focused element is still unknown, xflow attempts paste into the same window and keeps the dictation copied. A changed app/window or a known non-editable element falls back to copying, as do terminal control characters (including multiline input); secure fields refuse injection. `xflow last` retains recovery text. Command mode reads Accessibility selected text; the correction-learning hook reads the focused editable value and returns no text for secure fields or terminals. Clipboard restoration waits for the focused field to show the insertion (or, for an editable field that exposes no content, such as VS Code's editor, up to 5 s) and only restores while the clipboard still holds our injected text; later clipboard writes wait for that pending paste. A field whose content never shows the paste keeps the dictation copied; the pill and status say “Copied — press ⌘V to paste”. Restoration preserves previous text only; other formats are not restored.

The daemon creates no application windows or NSApplication and runs no Accessibility polling loop. Optional notifications and sound cues run short-lived system helpers. Physical delivery and permission behavior depend on the destination app exposing Accessibility; unsupported apps retain clipboard recovery.

## Providers and privacy

See [default configuration](config/default.toml) and [provider contracts](apps/cli/crates/xflow-providers/README.md). Provider keys stay out of TOML; headless usage accepts environment variables. Provider calls require credentials and send recorded audio to the selected endpoint. Cleanup is separate and opt-in; failure preserves the original transcript.

For OpenRouter, set `stt.provider = "openrouter"`, store `xflow key openrouter` or supply `OPENROUTER_API_KEY`. OpenRouter uses its documented JSON/base64 audio format; Groq/OpenAI-compatible servers use multipart WAV. Batch adapters also cover OpenAI, Deepgram, AssemblyAI, ElevenLabs, Gemini, Mistral, Together and DeepInfra. Fireworks is a cleanup provider, not an STT adapter. Streaming and arbitrary custom WebSocket protocols remain future work; provider/model quality and live credentials are unvalidated.

For a local OpenAI-compatible sidecar, configure `provider = "custom"`, a full endpoint such as `http://127.0.0.1:8080/v1/audio/transcriptions`, and an explicit model. The `local` catalog entry supports a preconfigured local server; `stt.protocol = "whisper-cpp"` selects the whisper.cpp server protocol. Set `privacy.offline = true` to reject non-loopback endpoints, including optional cleanup. A sidecar's own network behavior is outside this guard. No whisper model is bundled or silently downloaded.

History is local plaintext SQLite in a private directory, retained to 500 entries by default (configurable up to 100,000). Disable persistence with `privacy.history = false`; the last transcript remains in daemon memory for recovery. `clear-history` also clears that in-memory transcript. Clipboard contents are visible to the desktop clipboard manager. Optional text restoration is conditional on the clipboard still holding the injected text; full MIME restoration is not supported. Telemetry is absent; secrets, audio, transcripts and selected text are not logged.

## Verification and performance

```sh
scripts/check.sh
node --input-type=module --check < packaging/gnome-extension/extension.js
glib-compile-schemas --strict --dry-run packaging/gnome-extension/schemas
cargo build --manifest-path apps/cli/Cargo.toml --release --locked
scripts/benchmark.py --daemon apps/cli/target/release/xflowd --cli apps/cli/target/release/xflow --output docs/benchmarks/linux-headless.json
```

See [shared contracts](docs/CONTRACTS.md), [daemon behavior](docs/DAEMON_V1.md) and the CLI guide below for implemented v1 scope. Tests use synthetic audio, fake desktops, and local mock HTTP; they do not make paid API requests or type into your desktop. The [headless result](docs/benchmarks/linux-headless.json) measures startup, idle RSS/CPU and status IPC only. Physical hotkey, microphone, cloud latency, injection and overlay measurements need a real GNOME session. See [performance methodology](PERFORMANCE.md), [product requirements](PRD.md), [architecture](ARCHITECTURE.md), [roadmap](ROADMAP.md) and [source research](docs/RESEARCH.md).

Optional user service: install [packaging/xflow.service](packaging/xflow.service) into `~/.config/systemd/user/`, then run `systemctl --user daemon-reload` and `systemctl --user enable --now xflow.service`. An environment variable exported in your shell is not automatically available to a systemd service; use OS credential storage or configure the service environment deliberately.

## CLI guide

Build the binaries, then run `scripts/install.sh` (or `--prefix PATH --bin-dir PATH` for a staged install). The script installs binaries atomically and prints the next step. Run `xflow setup` (on macOS it opens the guided TUI setup) for numbered provider/model/microphone selection, hidden key input, an optional connection test with another try on a rejected key, and optional GNOME/service installation. New GNOME extensions on Wayland need a logout/login before `xflow extension enable`. `--yes` accepts config defaults; desktop installation, activation and provider checks require their explicit flags in scripts:

```sh
xflow setup --yes --provider groq --model whisper-large-v3-turbo
xflow key set groq --stdin < /path/to/private-key-file
xflow service install --enable
xflow extension install
xflow doctor
xflow toggle --command                 # command mode, repeat to finish
xflow listen --once --seconds 5         # final text only on stdout, no desktop delivery
xflow transcribe interview.wav --provider openai
xflow history search postgres --json
xflow history export --format csv > dictations.csv
xflow config set recording.auto_stop_secs 3
xflow config get stt.model --json
xflow dictionary add XFlow Postgres
xflow dictionary replace 'post grass' Postgres
xflow history edit 42 --text 'Deploy it on Kubernetes'   # or omit --text to use $EDITOR
xflow snippets add 'my signature' 'Regards, Alex'
xflow styles add chat --apps slack,discord --mode light
xflow overlay set position bottom
xflow completions bash > xflow.bash
```

**Learning from corrections** (`dictionary.auto_learn = true` by default). When you fix a word right after dictating, xflow learns it: the corrected word is added to your vocabulary as a recognition hint (it never becomes an automatic replacement), and `xflow watch` (plus a notification, when enabled) says "Added 'Kubernetes' to your dictionary". On macOS xflowd reads the field it pasted into twice through Accessibility: about 120 ms after the paste, then when you start the next dictation or after 60 s, only while focus is still on that field. It keeps just the pasted text and 16 characters on each side in memory, never the field. Correcting a transcript with `xflow history edit` uses the same rules, on every platform. Only close spellings are learned (at most four per dictation); case or punctuation changes, grammar or homophone fixes between everyday words (their/there), numbers, URLs, code, everyday words and secure fields are skipped. `xflow dictionary list` marks learned entries, and `xflow dictionary remove <word>` removes one for good.

The command tree is discoverable with `xflow --help` and `xflow <command> --help`:

```text
setup                  provider/model/key/microphone onboarding
start stop toggle cancel
status watch listen transcribe
last copy paste        copy-last / paste-last remain aliases
history                list search show copy paste delete edit export clear
config                 path show get set unset edit validate reset
providers              list info test use
models                 [provider]
key                    set remove status (legacy: key <provider>)
dictionary             list add remove replace unreplace import export
snippets               list add remove
styles                 list add remove
overlay                list get set reset
devices stats doctor
daemon                 start stop restart status logs
service                install uninstall
extension              install uninstall enable disable status
completions            bash elvish fish powershell zsh
version tui
```

`--json`, `--quiet`, `--color auto|always|never` and `--config PATH` are global flags. JSON data goes to stdout; errors are JSON objects on stderr with `error`, `hint` and `exit_code`. `watch --json` and `daemon logs --follow --json` emit newline-delimited JSON. Human tables honour `NO_COLOR`, `CLICOLOR_FORCE` and terminal detection; explicit `--color` wins. `--quiet` suppresses confirmations and progress while retaining requested data. `listen` and `transcribe` print plain text for pipes; `listen` subscribes before recording with `delivery=none` and accepts a successful transcript only after observing the new recording's Listening event. It ignores old results queued after the snapshot, exits promptly when a recording ends without text, and cancels on Ctrl-C even while Start awaits its reply. Without `--seconds`, an interactive `listen` records until Enter; it repeats unless `--once` is set. In a pipe, use `--seconds N` and it finishes after one dictation.

Config writes use the shared comment-preserving editor, validate the whole document, and reload an available daemon. An explicit `--config` pointing to a different file edits that file without reloading the default daemon. `config edit` edits a temporary copy using `$VISUAL` or `$EDITOR`, validates it, then atomically replaces the original; a failed editor or invalid file preserves the original. Editor commands support whitespace-separated arguments, without shell expansion. Resetting config or clearing history requires confirmation or `--yes`. Dictionary import/export uses one vocabulary word per line; JSON export also includes replacements. History supports `--limit`, `--offset`, `--query` and complete paged exports as JSON, CSV or Markdown.

`providers test` is an explicit non-billable credential/reachability check and uploads no audio. `transcribe` explicitly submits the file to the configured provider (subject to `privacy.offline`). `doctor` makes no provider calls and never opens the microphone; it reports desktop dependencies and actionable fixes, with separate manual checks for sound quality and text delivery. Key commands never print secrets; stdin and hidden-input keys are zeroized after use. File/config/catalog commands create no Tokio runtime; async I/O commands create a current-thread runtime on demand. The service belongs to `graphical-session.target` and stops with the graphical session.

| Exit code | Meaning |
| --- | --- |
| 0 | Success (including a closed output pipe) |
| 1 | Operation or daemon command failed |
| 2 | Invalid arguments or non-interactive confirmation required |
| 3 | Daemon unavailable, disconnected or protocol incompatible |
| 4 | Invalid/unwritable configuration |
| 5 | Provider or credential operation failed |
| 6 | Required doctor checks failed |
| 130 | `listen` interrupted and cancelled |

CLI acceptance uses [the isolated harness](apps/cli/crates/xflow-app/src/cli/verify.py): `python3 apps/cli/crates/xflow-app/src/cli/verify.py apps/cli/target/debug/xflow`. It creates temporary XDG directories, a fake Unix-socket daemon and fake system tools. It checks configuration/personalization, editor rollback, daemon reload, history formats, delivery-free listening and cancellation, errors, aliases, completions and installation without touching a real desktop, microphone, keyring or provider. Live GNOME onboarding, credential storage and real provider quality still need user acceptance.

CLI process startup is measured separately by [the fake-daemon benchmark](apps/cli/crates/xflow-app/src/cli/benchmark.py). Build with the release profile, then run `python3 apps/cli/crates/xflow-app/src/cli/benchmark.py apps/cli/target/release/xflow`; it measures spawn through exit over 100 samples after 10 warmups, including IPC for `toggle --quiet`. See [the recorded measurements](apps/cli/crates/xflow-app/src/cli/benchmark.json) and [the CLI completion report](apps/cli/crates/xflow-app/src/cli/REPORT.md) for results and scope limits. This measures CLI overhead; microphone opening, physical shortcuts and cloud transcription require separate measurements.

## License

MIT. The implementation is original. Research credits [whisrs](https://github.com/y0sif/whisrs), also MIT; no upstream source code was copied. Contributions should retain small platform adapters, truthful capability reporting and reproducible performance evidence.
