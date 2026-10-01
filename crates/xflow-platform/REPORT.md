# Linux platform v1 delivery report

Implemented on `itsmrad/feat-v1-platform`, based on contracts `f510a2c`.
The initial API checkpoint is `199515b`; subsequent fixes/tests are on this branch.

## Public APIs

- `CpalCapture::new(&RecordingConfig) -> Result<CpalCapture>` starts only its
  owner thread; it never opens a microphone until `AudioCapture::start()`.
- `input_devices() -> Result<Vec<InputDevice>>`, with `name: String` and
  `default: bool`, enumerates devices without recording. These exports require
  the default `native-audio` feature.
- `sounds::play(Cue, &SoundsConfig)` returns immediately. Cues are `Start`,
  `Stop`, and `Error`. The root also re-exports `play` and `Cue`.
- `notifications::notify(&str, &str) -> async Result<()>`, also exported at the
  root, sends a native freedesktop D-Bus notification with a two-second deadline.

## Capture

Device selection uses exact CPAL names; a missing configured name lists available
names. Buffers are allocated before stream playback, so callbacks can retain
samples before `start()` acknowledges. No desktop query occurs in this layer's
capture start path. Existing sample-count, duration, finite-PCM, silence and
whole-frame bounds remain in force.

The ordinary data callback allocates nothing, uses one short buffer lock and
atomic level updates, and signals limits through a bounded nonblocking queue.
The error callback preserves its error even when that queue is full and never
waits on the worker that drops the stream. Smoothed RMS uses a 25 ms attack and
120 ms release time constant; the daemon controls the approximately 30 Hz publish
rate. First-callback elapsed microseconds are measured with `Instant` and logged
at debug from the owner thread on stop; warm activation has a separate debug
measurement. A tracing subscriber belongs to the caller.

`keep_warm_secs = 0` closes immediately. After a successful stop, a positive
value retains the stream until a single `recv_timeout` deadline, discarding all
idle samples and releasing the previous recording buffer. A new start swaps in
a fresh buffer and clears that deadline. Cancel, stream failure, duration limit
and owner shutdown close capture. Idle capture replacement on daemon reload is
safe: construction opens no stream and dropping the old object queues shutdown
to its stream owner thread. Shutdown is asynchronous.

## Text delivery

| Path | Behavior |
| --- | --- |
| GNOME Shell service | A cached session connection resolves and pins the unique `org.xflow.Shell` owner, then calls `Inject(text, options_json)` directly. Options carry method, terminal flag, clipboard restore settings, and app/window target. Results map to pasted, typed, or clipboard-only. The service performs the final current-focus/lock guard. |
| Wayland paste fallback | Save previous clipboard text when enabled, set CLIPBOARD with `wl-copy`, recheck focus, then dispatch ydotool keys through an existing daemon socket. If ydotool or its socket is unavailable, use wtype. No privileged daemon is started. |
| X11 paste fallback | Use `xclip` CLIPBOARD and `xdotool key --clearmodifiers`, after checking original app/window identity. Traditional xterm/rxvt/st shortcuts also fill PRIMARY and restore both selections when enabled. |
| Type fallback | Send text over stdin to `ydotool type --file -`, `xdotool type --file -`, or `wtype -`. Successful typing leaves the clipboard unchanged. Text never appears in helper argv or shell syntax. |
| Clipboard method / refusal | Copy without key synthesis. Missing/changed focus, terminal control characters including newlines, or a failed keyboard helper produce clipboard-only recovery. Failed copy is an error, preserving caller responsibility for the transcript. |

The Shell service is preferred for paste/type, on both X11 and Wayland. Clipboard
method uses the clipboard helper because the Shell contract has no clipboard
method. An error, timeout or malformed result after dispatching Shell `Inject`
propagates an error; repeating via fallback could otherwise duplicate delivery.

GNOME Console, Ptyxis, GNOME Terminal, kitty, foot and other modern terminals use
Ctrl+Shift+V. xterm/rxvt/st retain Shift+Insert with PRIMARY populated on fallback.
PRIMARY selection uses `Shell.Selection()` first, then `wl-paste --primary
--no-newline` or `xclip -o -selection primary`. Selection whitespace is preserved
exactly and limited to 64 KiB. Subprocess output is bounded while reading, rather
than after an unbounded allocation.

Clipboard restoration runs after the configured delay and only when the current
selection still equals the injected text. Keyboard failure or refused delivery
keeps the transcript copied. Unknown/nontext previous selections are not restored.
Sway has a small recursive focused-window adapter for the requested wlroots/wtype
path; other Wayland compositors without the Shell service retain clipboard-only
safety until an identity adapter exists.

## Cues and notifications

Original embedded mono PCM16 WAV cues last 90/100/140 ms and require no cache
files. Playback selects the first installed executable in order: pw-play,
paplay, aplay. Volume is a per-stream argument for the first two; aplay receives
attenuated WAV samples, without changing a mixer. Custom files work directly with
pw-play/paplay; the aplay custom-file fallback supports PCM16 WAV up to 8 MiB.
Disabled/muted cues return immediately. At most four cues overlap, playback has
a ten-second deadline, and children are waited for or killed on cancellation.

Notifications use zbus rather than a subprocess, validate text sizes, and request
a five-second display expiry within the two-second call deadline.

## Audit fixes and verification

Reproduced and fixed the traditional PRIMARY-vs-CLIPBOARD shortcut mismatch,
missing Ptyxis/Console classification, trimming selection text, subprocess output
allocation before its size check, terminal control-character injection risk,
and the unbounded D-Bus Command request. The coordinator confirmed the last issue
with a 65,604-byte request. `Command` now rejects requests over 64 KiB before JSON
parsing/forwarding using `InvalidArgs`, matching socket framing. Tests prove that
exactly 65,536 bytes are accepted and 65,537 bytes are rejected on an isolated bus.

Verification includes existing capture bounds tests, exact device matching,
warm-buffer reset/deadline behavior, RMS smoothing, persisted error with a full
wake queue, focus/terminal safety, injection option/result mapping, selection
parsing and sound player preference/WAV volume. Ignored tests exercise Shell,
Daemon and Notifications against stub services, plus three fully fake desktop
helper environments in child processes. Fake paths cover typing stdin, clipboard
restore and user-copy preservation, modern/traditional terminal shortcuts,
missing ydotool, all three sound players, muted/disabled cues and custom sound
files. They never record, synthesize real keys, or write a real clipboard.

Run through the required shared gate:

Final `scripts/check.sh` passed (format, workspace Clippy with warnings denied,
and 51 workspace unit tests). All three isolated bus tests passed, and the fake
helper suite passed its X11, ydotool and wtype child environments. All five
isolated-test entries remain ignored by default.

```sh
PATH=/home/mrad/.cache/xflow-dev/bin:$PATH sh scripts/check.sh
env -u WAYLAND_DISPLAY -u DISPLAY dbus-run-session -- \
  /home/mrad/.cache/xflow-dev/bin/cargo test -p xflow-platform \
  -- --ignored --nocapture --test-threads=1
```

Set TMPDIR to a short workspace-local directory when shared `/tmp` has a quota;
Unix socket paths must remain below the system socket-path length limit.

Observed single-shot cached Shell stub Inject round trips were 0.93–1.42 ms;
fake X11/ydotool/wtype paste dispatches were 12–25 ms on this shared machine.
These are software smoke observations, not end-to-end destination benchmarks.

## Limits and remaining live validation

No microphone, actual keyboard, real clipboard or paid service was exercised.
Physical hotkey-to-first-audio and audible cue quality still need authorized
desktop validation; software cannot establish zero clipped words on all hardware.
Helpers and Shell success acknowledge key dispatch, not application acceptance.
Fallback focus and clipboard compare/restore have unavoidable check/use races.
Previous clipboard restoration preserves text only. User-rebound paste shortcuts,
keyboard layouts, ydotool's Unicode support, application input acceptance, and
compositor virtual-keyboard availability need live verification. Shell uses its
contract's Ctrl+Shift+V terminal flag; traditional PRIMARY shortcuts are handled
specifically by fallback helpers. Very large Sway trees exceed the bounded focus
read and safely disable automatic keys. A dead session-bus connection requires
recreating the desktop object. Platform research spec `platform.md` was absent
at both initial and later checks.

Default shortcut references: [GNOME Terminal](https://help.gnome.org/gnome-terminal/txt-copy-paste.html),
[GNOME Console source](https://raw.githubusercontent.com/GNOME/console/main/src/kgx-application.c),
[kitty](https://sw.kovidgoyal.net/kitty/actions/), and
[wtype stdin/modifier handling](https://github.com/atx/wtype/blob/master/README.md).
Ptyxis's installed `org.gnome.Ptyxis.gschema.xml` also confirms Ctrl+Shift+V
as the `paste-clipboard` default; no GSettings values were read or changed.
The whisrs MIT reference was read for terminal safety and delivery ideas; no code
or assets were copied.
