# XFlow v1 daemon behavior and verification

This worker continued Claude session `479c0250-80aa-41c7-8dd2-4b38fcc44832`.
Its last completed operation was the uncommitted v2 store implementation; that
file was preserved before daemon work began. Subsequent implementation and review
were performed by Codex. The orchestrator owns integration and release approval.

## IPC and history

The socket implements protocol v2 history paging/search/get/delete/copy/paste,
stats, reload, existing session commands, and subscriptions. A successful session
publishes one final event containing text, delivery outcome, optional saved entry,
and timings. Inactive status retains the last successful timings; an active
session does not inherit them. `delivery = none` avoids desktop lookup and delivery
for dictation, while clipboard delivery copies without injecting.

History schema v1 migrates transactionally to v2, retaining existing rows and
adding model, original provider text, app, language, duration, latency, mode and
word count. Original provider text is stored when it differs from the final text.
Retention is applied on open and append; enabled history requires a positive
limit. Disabled history neither records nor exposes persisted rows. Explicit
clear still erases previously saved rows and resets the in-memory last result and
timings. A generation check under the database lock prevents an abandoned save
from restoring cleared rows.

History records a completed processing result before desktop delivery, preserving
recovery text if delivery fails or is canceled after processing. It is not a count
of successful pastes.

Database files and their WAL are private; symlinks and unowned/nonregular database
files are rejected. A corrupt, locked, or newer database does not stop dictation:
startup preserves it, disables persistence, and reports a persistent warning.
Repair followed by reload can restore persistence. Clear checks WAL truncation
and reports a busy database rather than claiming a complete erase.

Search escapes SQL LIKE metacharacters and returns the matching total separately
from the page. Pages are capped at 200 entries and trimmed to the 64 KiB wire
limit. A final event may omit original-text metadata or its optional entry to fit
that limit; text and timings remain. Oversized individual history replies return
an error. Malformed socket frames get a bounded protocol error without echoing
input; abandoned queued requests are skipped.

Stats count retained sessions and whitespace-delimited words, sum audio duration,
and calculate today's sessions/words and a streak using local calendar days. A
streak may end yesterday. WPM uses words from rows with a positive audio duration;
latency p50/p95 use nearest-rank percentiles of rows with recorded latency.

## Text and command processing

The order is spoken punctuation (opt-in), configured filler removal, whitespace
tidying, whole-word dictionary replacements, model cleanup, literal snippets,
then optional trailing space. Dictionary corrections reach the model; snippet
expansions never reach it. Longest matching phrases win, replacements scan the
original input once per stage, and inserted values are not recursively expanded.
Unicode letters, digits, underscores, combining marks and internal apostrophes
preserve word boundaries. Snippets ignore case and punctuation between words.

The first style whose app pattern matches the focused app id overrides cleanup
mode/prompt. App identifiers are sent to the transformer only with
`cleanup.app_context`; dictionary words are supplied as vocabulary hints. Raw
dictation skips model cleanup, but an explicitly configured transformer remains
available for styles and commands. Failed dictation cleanup retains the locally
corrected transcript and reports a warning.

Command mode captures the selection concurrently with context after audio opens.
The spoken instruction is locally corrected, then passed as `command`, with the
selection as transformer input. An empty selection requests generation. A missing
transformer is rejected before recording; failed command transformation leaves
the selection untouched. Successful output uses the normal delivery policy.

## Reload, capture and responsiveness

IPC reload and SIGHUP reread the path supplied to xflowd, including `--config`.
Reload is refused while listening, processing, or another reload is pending.
Config, providers, desktop and store settings are staged off the actor before
swapping; validation/build errors retain the running configuration. Audio is
reused when the recording configuration is unchanged and rebuilt when any of its
fields changes, so changed recording limits also take effect.

Microphone opening precedes background focus/selection lookup and a bounded
provider warm-up. Slow context lookup cannot delay stop/cancel. Processing waits
at most two seconds for context before using an unknown target, leaving desktop
safety checks to the platform. Network and SQLite work run outside the actor.

Clips shorter than `recording.min_ms` are discarded before upload. Listening ticks
at 25 Hz publish levels, reset continuous-silence time on speech, and stop on the
silence deadline, maximum recording deadline, or capture's `finished()` flag.
There is no idle level polling. Completed capture retrieves buffered audio or
surfaces the original device error. Session generations prevent canceled results
or stale context from affecting later sessions.

Once desktop delivery starts, cancel/shutdown lets it finish key release rather
than aborting it midway. New recording is briefly refused while that delivery
finishes; stale completion cannot overwrite current state. Shutdown also drains
explicit copy/paste operations. Sounds and notifications use platform hooks;
stderr logging honors `XFLOW_LOG` without recording transcripts or secrets.

## Timing meaning

`hotkey_ms` measures caller monotonic timestamp through capture opening;
`open_ms` measures capture start. `audio_ms` derives from clip frames/rate.
`stt_ms` measures stop through transcription completion, `cleanup_ms` measures
the optional transformer, and `inject_ms` measures desktop delivery when used.
`total_ms` measures stop through delivery completion, including context/history
work before delivery; it excludes recording time and the final latency database
update. History persists audio duration and this total latency. Missing optional
measurements remain absent instead of being fabricated.

## Verification

Tests use fake capture/desktop adapters, loopback mock HTTP, private XDG paths
under this worktree's target directory, and private session buses; they do not use
the real daemon, microphone, clipboard, keyring or paid providers. Every Cargo
invocation goes through `/home/mrad/.cache/xflow-dev/bin/cargo`.

Integrated dependencies: contracts `f810583`, platform `2377ac8`, CI `dc5dd46`
(including `e590354`),
and providers `1b58d82` (including `c2274dc`). Daemon audit checkpoints `d457cb0`
and `b92709c` are retained.

Passed during this continuation:

- `cargo test -p xflow-app -p xflow-core --lib --locked --features xflow-app/test-support`:
  40 app and 15 core tests, including the timing-retention, capture-finished,
  generation/clear, reload, command, silence and delivery-cancellation regressions.
- `cargo check -p xflow-app --bins --no-default-features --locked`: production
  binaries compile without the fake-adapter feature.
- Read-only Node execution of the overlay owner's `quick start/stop is serialized`
  regression: one passed. Extension shortcut ordering stays with its owner;
  no new daemon session/request contract was introduced.
- Full `sh scripts/check.sh`: formatting, extension JavaScript syntax/schema,
  strict workspace Clippy, 40 app / 15 core / 17 platform / 41 provider unit tests,
  all 11 daemon mock end-to-end tests, three private-bus platform tests, and two
  fake-helper fallback tests (covering X11, ydotool and wtype child fixtures).
  The end-to-end run includes both retained daemon-only corruption/SIGHUP tests
  and CI's new GET/HEAD warm-up regression. The only integration adaptation was
  adding `None` to the SIGHUP test's revised config helper signature.

Reproduce the full run from this worktree with short private paths:

```sh
mkdir -p target/t target/v/r target/v/c target/v/d
chmod 700 target/v/r
env -u DISPLAY -u WAYLAND_DISPLAY MISE_RUST_VERSION=1.98.1 \
  TMPDIR="$PWD/target/t" XDG_RUNTIME_DIR="$PWD/target/v/r" \
  XDG_CONFIG_HOME="$PWD/target/v/c" XDG_DATA_HOME="$PWD/target/v/d" \
  PATH="/home/mrad/.cache/xflow-dev/bin:$PATH" sh scripts/check.sh
```

The successful run's output is in `target/scratch/daemon-verify/full-check.log`.
One initial run used an overly deep TMPDIR and exceeded Linux's Unix-socket path
limit at fixture startup; shorter private paths resolved it without daemon code
changes. No full-suite failure remains.

## Remaining limits

SQLite LIKE provides ASCII case-insensitive search, without Unicode case folding.
The text pipeline uses Unicode lowercase without full case folding or canonical
normalization. Word stats use whitespace boundaries, so a CJK run counts as one
word. Final events can omit optional history metadata at the wire size limit.
At the server's 32-connection ceiling, excess connections are closed; clients
must handle transport failure. Cancellation cannot undo already-started desktop
delivery. Clearing SQLite rows/WAL is not secure erasure of filesystem snapshots
or backups. Real device capture, GNOME focus/selection/paste behavior, feedback
and cloud model quality remain manual integration checks for the orchestrator.
