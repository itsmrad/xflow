# xflow

Native, local-first dictation for Linux, written in Rust. This is an early Ubuntu GNOME MVP, with provider and platform interfaces for later Fedora/macOS/Windows adapters. It is not yet a production release; real desktop and provider acceptance tests remain required.

The workspace includes a small daemon, clap CLI, event-driven ratatui TUI, CPAL microphone capture, Groq/OpenRouter/OpenAI-compatible batch transcription, optional LLM cleanup, SQLite history, and a native GNOME Shell recording pill. No Electron, WebView, telemetry, or resident local model runtime.

## Build and run

The locked dependency set declares Rust 1.88 as its highest minimum compiler version (notably `instability`, `darling`, and ICU crates). Rust 1.88 is the declared lower bound; CI has not separately tested that compiler. Native development libraries are also required:

```sh
# Ubuntu
sudo apt install build-essential pkg-config libasound2-dev libdbus-1-dev wl-clipboard
# Fedora (build recipe; desktop validation is still pending)
sudo dnf install gcc gcc-c++ pkgconf-pkg-config alsa-lib-devel dbus-devel wl-clipboard
cargo build --release --locked
install -Dm755 target/release/xflow ~/.local/bin/xflow
install -Dm755 target/release/xflowd ~/.local/bin/xflowd
xflow init
xflow key groq                 # hidden input, OS Secret Service/keychain
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
xflow tui                     # Space toggle, Escape cancel, l last, c copy, p paste, q quit
xflow shutdown
```

`stop` acknowledges processing immediately. Subscribe with `watch` or `tui` for the final result. The transcript is retained before injection; `last` recovers it if desktop integration fails. Capture is capped at 120 seconds by default and the microphone closes outside recording. Audio remains in memory and is never saved or logged by default.

Daemon status retains `success` or `error` until the next recording or cancel. The GNOME pill hides its feedback after a short display; that visual reset does not change daemon status.

Cancel discards an active capture or pending provider job. Once a transcript has been finalized and saved, cancel cannot undo its history entry; a queued cancel is handled before injection starts when possible. `clear-history` cancels active work before clearing persisted history.

GNOME Wayland needs the bundled Shell extension for native shortcuts, focus discovery and overlay. Follow [platform setup](docs/platform-setup.md) to install it and configure clipboard/uinput support. It uses toggle and explicit start/stop shortcuts; true press/release push-to-talk is a release gate. CLI `start`/`stop` can be bound to press/release events on compositors that expose them. Unavailable paste or changed/unknown focus falls back to copying; automatic multiline paste into terminals is refused.

## Providers and privacy

See [default configuration](config/default.toml) and [provider contracts](crates/xflow-providers/README.md). Provider keys stay out of TOML; headless usage accepts environment variables. Provider calls require credentials and send recorded audio to the selected endpoint. Cleanup is separate and opt-in; failure preserves the original transcript.

For OpenRouter, set `stt.provider = "openrouter"`, store `xflow key openrouter` or supply `OPENROUTER_API_KEY`. OpenRouter uses its documented JSON/base64 audio format; Groq/OpenAI-compatible servers use multipart WAV. Streaming/Deepgram/custom WebSocket engines are planned and are not advertised as implemented.

For a local OpenAI-compatible sidecar, configure `provider = "custom"`, a full endpoint such as `http://127.0.0.1:8080/v1/audio/transcriptions`, and an explicit model. Set `privacy.offline = true` to reject non-loopback endpoints, including optional cleanup. A sidecar's own network behavior is outside this guard. No whisper model is bundled or silently downloaded.

History is local plaintext SQLite in a private directory, capped at 500 entries. Disable persistence with `privacy.history = false`; the last transcript remains in daemon memory for recovery. `clear-history` also clears that in-memory transcript. Clipboard contents are necessarily visible to the desktop clipboard manager and are not automatically restored. Telemetry is absent; secrets, audio, transcripts and selected text are not logged.

## Verification and performance

```sh
scripts/check.sh
node --input-type=module --check < packaging/gnome-extension/extension.js
glib-compile-schemas --strict --dry-run packaging/gnome-extension/schemas
cargo build --release --locked
scripts/benchmark.py --daemon target/release/xflowd --cli target/release/xflow --output docs/benchmarks/linux-headless.json
```

Tests use synthetic audio, fake desktops, and local mock HTTP; they do not make paid API requests or type into your desktop. The [headless result](docs/benchmarks/linux-headless.json) measures startup, idle RSS/CPU and status IPC only. Physical hotkey, microphone, cloud latency, injection and overlay measurements need a real GNOME session. See [performance methodology](PERFORMANCE.md), [product requirements](PRD.md), [architecture](ARCHITECTURE.md), [roadmap](ROADMAP.md) and [source research](docs/RESEARCH.md).

Optional user service: install [packaging/xflow.service](packaging/xflow.service) into `~/.config/systemd/user/`, then run `systemctl --user daemon-reload` and `systemctl --user enable --now xflow.service`. An environment variable exported in your shell is not automatically available to a systemd service; use OS credential storage or configure the service environment deliberately.

## CLI guide

Build the binaries, then run `scripts/install.sh` (or `--prefix PATH --bin-dir PATH` for a staged install). The script installs binaries atomically and prints the next step. Run `xflow setup` for numbered provider/model/microphone selection, hidden key input, an optional connection test, and optional GNOME/service installation. New GNOME extensions on Wayland need a logout/login before `xflow extension enable`. `--yes` accepts config defaults; desktop installation, activation and provider checks require their explicit flags in scripts:

```sh
xflow setup --yes --provider groq --model whisper-large-v3-turbo
xflow key set groq --stdin < /path/to/private-key-file
xflow service install --enable
xflow extension install
xflow doctor
xflow toggle --command                 # command mode, repeat to finish
xflow listen --once --seconds 5         # final text only on stdout, no desktop delivery
xflow transcribe interview.wav --provider openai --model whisper-1
xflow history search postgres --json
xflow history export --format csv > dictations.csv
xflow config set recording.auto_stop_secs 3
xflow config get stt.model --json
xflow dictionary add XFlow Postgres
xflow dictionary replace 'post grass' Postgres
xflow snippets add 'my signature' 'Regards, Alex'
xflow styles add chat --apps slack,discord --mode light
xflow overlay set position bottom
xflow completions bash > xflow.bash
```

The command tree is discoverable with `xflow --help` and `xflow <command> --help`:

```text
setup                  provider/model/key/microphone onboarding
start stop toggle cancel
status watch listen transcribe
last copy paste        copy-last / paste-last remain aliases
history                list search show copy paste delete export clear
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

`--json`, `--quiet`, `--color auto|always|never` and `--config PATH` are global flags. JSON data goes to stdout; errors are JSON objects on stderr with `error`, `hint` and `exit_code`. `watch --json` and `daemon logs --follow --json` emit newline-delimited JSON. Human tables honour `NO_COLOR`, `CLICOLOR_FORCE` and terminal detection; explicit `--color` wins. `--quiet` suppresses confirmations and progress while retaining requested data. `listen` and `transcribe` print plain text for pipes; `listen` subscribes before recording with `delivery=none`, ignores the prior transcript in the subscription snapshot, and cancels its recording on Ctrl-C. Without `--seconds`, an interactive `listen` records until Enter; it repeats unless `--once` is set. In a pipe, use `--seconds N` and it finishes after one dictation.

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

CLI acceptance uses [the isolated harness](crates/xflow-app/src/cli/verify.py): `python3 crates/xflow-app/src/cli/verify.py target/debug/xflow`. It creates temporary XDG directories, a fake Unix-socket daemon and fake system tools. It checks configuration/personalization, editor rollback, daemon reload, history formats, delivery-free listening and cancellation, errors, aliases, completions and installation without touching a real desktop, microphone, keyring or provider. Live GNOME onboarding, credential storage and real provider quality still need user acceptance.

## License

MIT. The implementation is original. Research credits [whisrs](https://github.com/y0sif/whisrs), also MIT; no upstream source code was copied. Contributions should retain small platform adapters, truthful capability reporting and reproducible performance evidence.
