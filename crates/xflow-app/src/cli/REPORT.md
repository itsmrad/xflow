# XFlow v1 CLI completion report

Worktree: `feat-v1-cli`; branch: `itsmrad/feat-v1-cli`. Implementation and continuation work was performed by Codex, without Claude co-author attribution. The coordinator owns integration and release decisions.

## Recovery and implementation

Recovered predecessor session `104b929f-ace5-486e-a66c-f501566aa0a6` from its JSONL in bounded text/tool batches, omitting thinking signatures. It was the old orchestrator session: baseline tests, shared contracts and seven workers were completed or launched, but the CLI task was written and never launched before exhaustion. The initial CLI worktree was clean at `f510a2c`; continuation preserves all resulting commits and the saved listening checkpoint `3a49528`.

The CLI now uses clap derive for discovery and aliases, aligned human tables, global JSON/quiet/colour/config flags, stable errors and exit codes, lazy asynchronous runtime creation, numbered onboarding, and validated atomic configuration edits. It integrates the provider catalog/check/key/file-transcription APIs, shared transport/config/GSettings/path helpers, history and statistics, desktop/service installers, and the TUI entry point. Extension installation embeds every required runtime asset, compiles schemas and explains Wayland re-login.

`listen` subscribes before starting with delivery `none`, ignores snapshot and queued old completions, requires an observed Listening event before accepting Success text, and rejects Processing text. Idle after recording returns promptly without text. Ctrl-C is registered before Start and sends Cancel after the bounded pending Start completes. A pinned transport read preserves partially received frames when the recording timer or Enter sends Stop; an isolated split-frame regression covers this path.

Current reviewed dependencies include providers `1b58d82`, overlay `77ab635`, daemon `64b38ed` (merged at `8eafa7f`, retaining `b92709c` capture-completion and last-timings changes), and CI `dc5dd46` (merged at `a833389`). Both daemon-only end-to-end scenarios are retained; the SIGHUP test call supplies the CI helper's new optional protocol argument. Research CLI/PM files were unavailable when checked again; the live assignment and shared contract define the command tree.

## Final `xflow --help`

```text
Fast Linux dictation, from your terminal

Usage: xflow [OPTIONS] <COMMAND>

Commands:
  setup        Set up a provider, microphone and optional desktop integration
  start        Begin recording (bind to shortcut press for push-to-talk)
  stop         Stop recording and begin transcription
  toggle       Start or stop hands-free recording
  cancel       Discard the active recording or pending transcription
  status       Show daemon state, provider, model, version and timings
  watch        Subscribe to daemon events until interrupted
  listen       Print dictation to stdout without changing the desktop
  transcribe   Transcribe an audio file without a running daemon
  last         Print the last transcript
  copy         Copy the last transcript [alias: copy-last]
  paste        Insert the last transcript into the focused app [alias: paste-last]
  history      Browse, search and export local dictation history
  config       Inspect or edit validated, comment-preserving TOML configuration
  providers    List providers, inspect capabilities and select a default
  models       List a provider's models, marking the selected and default model
  key          Manage provider credentials in OS credential storage
  dictionary   Manage vocabulary and text replacements
  snippets     Manage spoken shortcuts that expand to saved text
  styles       Manage cleanup styles selected by focused application
  overlay      Inspect and change GNOME overlay/shortcut settings
  devices      List available microphone input devices
  stats        Show local dictation statistics
  doctor       Diagnose configuration and desktop dependencies without recording
  daemon       Control the systemd user daemon or view its logs
  service      Install or uninstall the graphical-session user service
  extension    Install or manage the bundled GNOME Shell extension
  completions  Generate shell completion scripts
  version      Show CLI and, when available, daemon versions
  tui          Open the terminal control centre
  help         Print this message or the help of the given subcommand(s)

Options:
      --json           Emit machine-readable JSON (watch emits one object per event)
  -q, --quiet          Suppress confirmations; requested data and errors remain visible
      --color <COLOR>  Control colour in human output [default: auto] [possible values: auto, always, never]
      --config <PATH>  Read/write an alternate configuration file
  -h, --help           Print help
  -V, --version        Print version

Start here: xflow setup
Explore: xflow <command> --help
Pipe text: xflow listen --seconds 5
Exit codes: 0 success, 1 operation failed, 2 usage, 3 daemon unavailable, 4 config, 5 provider, 6 doctor.
```

Subcommands:

```text
history     list search show copy paste delete export clear
config      path show get set unset edit validate reset
providers   list info test use
key         set remove status
dictionary  list add remove replace unreplace import export
snippets    list add remove
styles      list add remove
overlay     list get set reset
daemon      start stop restart status logs
service     install uninstall
extension   install uninstall enable disable status
```

Legacy `key <provider>`, `history -n`, `init`, `shutdown`, `clear-history`, `copy-last` and `paste-last` remain accepted. Shell completion supports Bash, Elvish, Fish, PowerShell and Zsh.

## Examples

```sh
xflow setup
xflow setup --yes --provider groq --model whisper-large-v3-turbo
xflow key set groq --stdin < /path/to/private-key-file
xflow toggle --command
git commit -m "$(xflow listen --once --seconds 5 --quiet)"
xflow transcribe meeting.wav --provider openai --model whisper-1
xflow history search postgres --json
xflow history export --format csv > dictations.csv
xflow config set recording.auto_stop_secs 3
xflow --config /path/to/alternate.toml config validate
xflow dictionary replace 'post grass' Postgres
xflow snippets add 'my signature' 'Regards, Alex'
xflow styles add chat --apps slack,discord --mode light
xflow overlay set position bottom
xflow service install --enable
xflow extension install
xflow doctor --json
xflow completions bash > xflow.bash
```

## Exit codes and output

| Code | Meaning |
| --- | --- |
| 0 | Success, including a closed output pipe |
| 1 | Operation or daemon command failed; recording ended without a transcript |
| 2 | Invalid arguments, or confirmation needed without an interactive terminal |
| 3 | Daemon unavailable/disconnected, invalid IPC, version or protocol mismatch |
| 4 | Invalid or unwritable configuration |
| 5 | Provider or credential operation failed |
| 6 | Required doctor checks failed |
| 130 | `listen` interrupted; cancellation attempted |

JSON goes to stdout; errors go to stderr as `{ok:false,error,hint,exit_code}`. `watch --json` and following daemon logs emit NDJSON. Requested data remains visible with `--quiet`; human colour respects terminal detection, `NO_COLOR`, `CLICOLOR_FORCE` and explicit `--color`. Setup produces one aggregate JSON object. A different `--config` file never reloads the default daemon.

## Verification and measurements

The complete acceptance harness passes with both the updated release and debug CLI, including the added split-frame test. It uses temporary XDG paths, same-user fake Unix sockets, loopback HTTP, synthetic audio files, and fake system/extension/editor tools. It covers config/personalization round-trips, rejected writes and editor rollback, private file modes, reload isolation, onboarding JSON, staged install paths containing spaces, catalog/models, non-billable checks, file transcription, human/JSON status, history formats, aliases, completion, protocol errors, stale results and cancellation. Focused CLI Clippy also passes with warnings treated as errors.

The final isolated `scripts/check.sh` passes after integrating daemon `64b38ed`, CI `dc5dd46` and the one-argument SIGHUP fixture adaptation. Formatting, strict schema validation, all 38 extension tests and workspace/all-targets Clippy with `-D warnings` pass. Rust results: 40 application library tests, nine CLI tests, all 11 daemon end-to-end scenarios, 15 core tests, 41 provider tests, 17 normal platform tests, three isolated D-Bus platform tests and both isolated fallback tests pass; doc tests pass. The mock explicitly handles non-billable GET/HEAD warm-up without consuming transcription plans while retaining strict POST/multipart checks. CLI release acceptance includes human/JSON retained-timings output.

The complete check ran with displays and the desktop session bus unset, a private `dbus-run-session`, `MISE_RUST_VERSION=1.98.1`, the shared cargo gate on PATH, worktree `target/compiler-tmp`, and worktree `target/check-{runtime,config,data}` XDG directories. Future runs can use shorter `target/t` and `target/v` paths to leave more room under Unix socket pathname limits; no global environment or gate changes are required.

Reproduce CLI acceptance and process measurements after a gated build:

```sh
env MISE_RUST_VERSION=1.98.1 TMPDIR="$PWD/target/compiler-tmp" \
  /home/mrad/.cache/xflow-dev/bin/cargo build --release --locked -p xflow-app --bin xflow
mkdir -p target/t
chmod 700 target/t
env PYTHONDONTWRITEBYTECODE=1 TMPDIR="$PWD/target/t" \
  python3 crates/xflow-app/src/cli/verify.py target/release/xflow
env PYTHONDONTWRITEBYTECODE=1 TMPDIR="$PWD/target/t" \
  python3 crates/xflow-app/src/cli/benchmark.py target/release/xflow
```

The [benchmark artifact](benchmark.json) records 100 complete process executions after ten warmups per command, using the release profile with thin LTO and one codegen unit. It includes spawning, dynamic loading, parsing, fake-daemon IPC for toggle and process exit. Python spawning and host scheduling are included; no baseline subtraction is applied. The daemon receives exactly one request per toggle; config path and help make no daemon requests. Results vary with host load and are not physical shortcut or microphone measurements. The original approximately 1 ms process aspiration is not claimed as achieved.

Recorded on Linux x86_64, kernel `7.0.0-34-generic`, at 2026-10-02 03:28 UTC with integrated checkpoint `8eafa7f`; the artifact includes the measured binary's SHA-256.

| Command | Minimum (ms) | Median (ms) | p95 (ms) |
| --- | ---: | ---: | ---: |
| `toggle --quiet` | 1.888 | 2.228 | 2.918 |
| `config path` | 1.745 | 2.148 | 3.337 |
| `--help` | 1.754 | 1.983 | 2.765 |

## Remaining scope limits

- Physical microphone quality, GNOME installation/re-login/shortcuts, desktop insertion, OS credential storage and real cloud-provider quality remain manual acceptance. Verification never touches the running user daemon, microphone, clipboard or real keyring, and makes no paid calls.
- Provider checking is explicitly non-billable and does not validate transcription quality. File transcription intentionally uploads the supplied file when invoked; `privacy.offline` restricts endpoints.
- Shell editor arguments use whitespace splitting without shell expansion. Dictionary text import/export handles vocabulary; JSON export additionally preserves replacements.
- `listen` requires `--seconds` in a pipe and cancels while operating its recording. The IPC contract has no session IDs; the progression guard relies on ordered subscription events and refuses an already-active snapshot. Cancellation is best-effort if the daemon becomes unreachable.
- Status displays last successful timings when inactive with the reviewed v1 daemon; fields remain optional for compatible older replies. CLI helpers do not invent missing timings or require extra hotkey status requests.
- No streaming-provider protocol, local model download or desktop-specific global shortcut implementation was added by the CLI worker. Those remain their component owners' scope.
