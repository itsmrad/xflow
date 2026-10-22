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
  is taken as a lost key-up and stops the session.
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

The macOS pill shows listening/processing and microphone level during either a
hold or hands-free recording. It is borderless, non-activating and click-through.
It runs as a transient sibling `xflow overlay-pill` process, fed only state and
level over bounded stdin frames. It exits on idle, completion, input closure or
daemon shutdown. A missing/crashed helper does not prevent recording.

`xflowd` does not load AppKit; only the helper loads it on demand. No hotkey or
pill polling timer runs at idle. The helper blocks on native events, awakened
by its input thread. `xflow` must be installed alongside `xflowd` for the helper
to be found.

```sh
xflow overlay set enabled false
xflow overlay set position bottom-center
xflow overlay set scale 1.2
xflow overlay set opacity 0.9
xflow overlay set theme dark
```

On macOS these appearance settings are stored beside `config.toml` in
`overlay.json` and apply to the next pill. The pill uses the main screen's visible
area and supports the existing top/bottom/left/right position names. GNOME keeps
its existing native renderer, appearance settings and richer waveform options.

## Linux

GNOME keeps the Shell extension, D-Bus commands, compositor shortcut grabs and
release handling. Its `smart` shortcut mode now uses the same hold/double-tap/
lone-tap-cancel gestures. `hold` and `toggle` modes remain available. Linux
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
