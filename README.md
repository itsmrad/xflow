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

## License

MIT. The implementation is original. Research credits [whisrs](https://github.com/y0sif/whisrs), also MIT; no upstream source code was copied. Contributions should retain small platform adapters, truthful capability reporting and reproducible performance evidence.
