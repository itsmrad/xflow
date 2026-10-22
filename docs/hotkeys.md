# Global dictation key

On macOS, `xflowd` listens for Fn/Globe by default. Set **System Settings → Keyboard →
Press 🌐 key to → Do Nothing** so macOS does not open its own picker or dictation.
Allow `xflowd` (or the terminal that launches it) under **Privacy & Security →
Input Monitoring**. If the passive event tap cannot be created, `xflow status`
reports an error with these instructions; CLI dictation remains available. After
granting access, restart the daemon or reload with `xflow config set hotkey.enabled true`.

```toml
[hotkey]
key = "fn"
double_tap_ms = 500
hold_threshold_ms = 500
enabled = true
```

Defaults follow Wispr Flow's published gestures (500 ms tap and double-tap windows):

- Press and hold: capture starts immediately. Release after `hold_threshold_ms` to
  transcribe and deliver the result through the configured injection method.
  The start sound and the pill wait until the key has been held for 150 ms or a
  double-tap is detected. A lone tap or a Fn+key shortcut plays and shows nothing.
- Double-tap: a quick tap (released before `hold_threshold_ms`) followed by a second
  press within `double_tap_ms` of that release. Recording stays on after release;
  the next press stops it, and a third press within `double_tap_ms` of the second
  cancels instead. `xflow watch` reports `Hands-free recording` while latched.
- A single short tap waits out the double-tap window, then discards capture
  without uploading, saving history, or producing an error. Escape cancels a
  hotkey recording. The tap is listen-only, so Escape and the key itself still
  reach the focused app.
- Pressing another key within 1 s of starting a hold (e.g. Fn+arrow) treats the
  hotkey as a modifier and cancels. A press arriving more than 10 s into a hold
  is taken as a lost key-up and stops the session. If macOS briefly disables the
  event tap, a key that is still held keeps its hold. A key-up that was missed
  meanwhile discards the hold.
- Repeated key-down events do not start additional sessions. The existing
  `recording.min_ms`, silence detection, maximum duration and cancellation guards
  still apply. Existing recording start/stop sounds follow `[sounds]`.

Allowed names: `fn`, `right_option`, `left_option`, `right_command`,
`right_control`, `right_shift`, `caps_lock`, and `f13` through `f19`. Chords use
one or more of `ctrl`, `alt`, `shift`, `cmd` followed by `a`–`z`, `0`–`9`,
`space`, `tab`, `enter`, `escape`, `backspace`, or `f1`–`f20`, for example
`ctrl+alt+space`. Names are lowercase. Letter/digit chords use physical ANSI
positions; named keys do not depend on the keyboard layout. Caps Lock's lock
behavior is not suppressed; hardware-specific Caps Lock press/release behavior
still needs validation.

Change the binding live while idle:

```sh
xflow config set hotkey.key right_option
xflow config set hotkey.key ctrl+alt+space
xflow config set hotkey.enabled false
```

The normal config editor validates the document and reloads the daemon. Reload
requires idle; an invalid configuration leaves the running config intact. If a
replacement event tap cannot be created, the rest of the config still applies, the
old binding keeps working and `xflow status` reports the error. Each reload retries
a missing tap, e.g. after granting Input Monitoring. Timing values are bounded:
`double_tap_ms` and `hold_threshold_ms` are 1–2000.

## Recording pill

The macOS pill is a small dark capsule just above the Dock on the screen under
the mouse, modeled on Wispr Flow's bar. While you are listening it shows a live
waveform. In hands-free mode it gets wider and adds ✕ (cancel) and ■ (stop)
buttons. While processing, the bars collapse into shimmering dots. On error it
flashes red with a shake, then fades. It never takes focus. Clicks pass through
it, except on the hands-free buttons. With Reduce Motion it shows and hides
without animating.

| Listening | Hands-free | Processing | Error |
|---|---|---|---|
| ![](img/pill-listening.png) | ![](img/pill-hands-free.png) | ![](img/pill-processing.png) | ![](img/pill-error.png) |

The pill runs as a transient sibling process, `xflow overlay-pill`. It receives
only state, level and a hands-free flag, over bounded stdin frames. It is drawn
with CALayers and animates at about 60 fps only while it is visible. It exits
when the session ends. `xflowd` never loads AppKit, and a missing or crashed
helper does not stop recording. `xflow` must be installed next to `xflowd` so
the helper can be found.

```sh
xflow overlay preview            # hidden: cycles listening, hands-free, processing, error
xflow overlay set enabled false
xflow overlay set position bottom-center
xflow overlay set scale 1.2
xflow overlay set opacity 0.9
```

On macOS these settings are stored in `overlay.json` next to `config.toml` and
apply to the next pill. GNOME keeps its own renderer and appearance settings.

## Linux

GNOME keeps the Shell extension, D-Bus commands, compositor shortcut grabs and
release handling. Its `smart` shortcut mode uses the same hold, double-tap,
third-press-cancel and lone-tap-cancel gestures, and a late second press discards
the tap and starts again. The 150 ms feedback delay, the Fn+key cancel and
stuck-key recovery are macOS-only. `hold` and `toggle` modes remain available. Linux
bindings and timing remain extension settings, rather than the macOS TOML key:

```sh
xflow overlay set hotkey-mode smart
xflow overlay set ptt-threshold-ms 500
xflow overlay set double-tap-ms 500
```

There is no evdev reader. Other compositors can continue binding CLI `start`,
`stop`, and `cancel` themselves.

## Verification

Pure Rust gesture tests cover hold/release, the double-tap boundary, lone-tap
timeout, hands-free stop, Escape and repeat/release noise. A daemon fixture tests
actions against fake capture/providers and proves a failed hotkey reload leaves
the old binding working. GNOME's pure JavaScript tests cover the corresponding
gestures; its compositor smoke harness is updated for double-tap semantics.

For physical acceptance on a Mac, run `xflow watch --json`, hold Fn while
speaking, and confirm `listening → processing → success` and insertion into the
original target. Repeat with double-tap/release/stop and with lone-tap/Escape
cancellation. Synthetic actions and helper screenshots do not prove that physical
Fn events, microphone capture, permissions, or destination paste work.

The event tap follows Apple's [CGEventTapCreate contract](https://developer.apple.com/documentation/coregraphics/cgevent/tapcreate(tap:place:options:eventsofinterest:callback:userinfo:)):
listen-only, with a dedicated CFRunLoop thread; modifier/Fn input uses
`flagsChanged`, ordinary keys use `keyDown`/`keyUp`.
