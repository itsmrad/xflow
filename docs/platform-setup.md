# Native Linux desktop setup

The Ubuntu MVP uses CPAL/ALSA for capture and a GNOME Shell extension for shortcuts,
the floating pill, and focused-window identity. The daemon does not depend on GTK,
Electron, a WebView, or a local model runtime. The microphone opens on `start` and
closes on `stop`, `cancel`, capture error, or the recording duration limit. The
recording buffer is bounded by `recording.max_seconds`, 16 Mi input samples
(64 MiB PCM), and 12,499,978 complete input frames (the 25 MB mono WAV upload
limit), whichever comes first. A lightweight RMS detector
rejects silence while preserving internal pauses. It does not automatically stop
after a pause or classify noise versus speech.

## Ubuntu GNOME

Build dependencies and clipboard tools:

```sh
sudo apt install build-essential pkg-config libasound2-dev wl-clipboard xclip xdotool
cargo build --release
```

Install the extension runtime assets (run from the repository root):

```sh
mkdir -p "$HOME/.local/share/gnome-shell/extensions/xflow@xflow.local/schemas"
cp packaging/gnome-extension/{extension.js,logic.js,prefs.js,stylesheet.css,metadata.json} \
  "$HOME/.local/share/gnome-shell/extensions/xflow@xflow.local/"
cp packaging/gnome-extension/schemas/*.xml \
  "$HOME/.local/share/gnome-shell/extensions/xflow@xflow.local/schemas/"
glib-compile-schemas "$HOME/.local/share/gnome-shell/extensions/xflow@xflow.local/schemas"
```

Log out and back in on Wayland so GNOME discovers the extension, then run:

```sh
gnome-extensions enable xflow@xflow.local
gnome-extensions prefs xflow@xflow.local
```

The extension calls `org.xflow.Daemon.Command` asynchronously on the session bus.
It needs the v1 daemon running; it does not spawn the CLI or use `command-path`.
A missing daemon shows “xflow daemon not running — run: xflow daemon start”.
Appearance and shortcuts belong to GSettings, independently of daemon TOML.
The native preferences window has Appearance, Shortcuts and Behavior pages with
a static preview; changes take effect immediately.

| Action | Default shortcut / setting |
| --- | --- |
| Dictate | Super+Alt+Space (`toggle-shortcut`) |
| Voice command | Super+Alt+R (`command-shortcut`) |
| Cancel | Escape only while listening/processing; Super+Alt+Escape (`cancel-shortcut`) |
| Copy / paste last | Super+Alt+C / Super+Alt+V (`copy-shortcut` / `paste-shortcut`) |
| Explicit start / stop | Unassigned (`start-shortcut` / `stop-shortcut`) |

Smart mode starts recording at keypress. A tap shorter than `ptt-threshold-ms`
(default 300 ms) keeps hands-free recording running until the next press; a hold
stops on key or modifier release. Choose `hotkey-mode=hold` for always-held
push-to-talk, or `toggle` for tap-only control. The release watcher exists only
while a shortcut is held. Shortcuts ignore autorepeat and apply only in the
normal desktop session; changing appearance does not re-register shortcuts.
The command shortcut uses the same gesture rules and sends `mode=command`.

The default pill is a compact, off-white capsule with near-black rounded bars.
Listening shows a smoothed, center-weighted waveform; processing contracts to
five shimmer dots; success briefly shows a check; error shows an accent and a
short message. Command mode uses a muted violet accent. Idle is hidden by
default, with an optional static minimal handle. No recurring source runs while
idle. Motion respects both `animations` and the desktop’s `enable-animations`.

Use preferences or the following prefix for settings:

```sh
gsettings --schemadir "$HOME/.local/share/gnome-shell/extensions/xflow@xflow.local/schemas" \
  set org.gnome.shell.extensions.xflow theme 'auto'
```

Settings include `theme` (light/dark/auto), `position` (bottom-center, top-center,
bottom-left, bottom-right, top-left, top-right), `monitor` (primary/focused/pointer),
`margin`, `offset-x`, `offset-y`, `scale`, `opacity`, `waveform-bars`,
`waveform-style` (rounded/thin/square), `idle-style` (hidden/minimal), `animations`,
`show-messages`, `success-hold-ms`, `error-hold-ms`, `hotkey-mode` and
`ptt-threshold-ms`. Pointer monitor selection is sampled on activation and layout
updates, without idle polling. Placement uses the monitor work area and HiDPI
scale, and clamps offsets to keep the pill on screen. The old `position=top` and
`bottom` values remain aliases; old `width`, `height`, `animation` and
`command-path` settings are replaced by `scale`, `animations` and D-Bus control.

`org.xflow.Shell` provides `Context`, `Version`, `Selection` (PRIMARY, at most
64 KiB), and `Inject`. Native paste uses Clutter Ctrl+V or Ctrl+Shift+V for a
terminal after copying text and rechecking the captured window identity. Unknown
or changed focus returns `clipboard_only` and keeps text available for recovery.
Screen lock refuses selection and injection. Optional clipboard restoration
checks both the injection generation and the current text so it does not erase
a newer user copy. Short typing uses the native input method for Unicode, with
an ASCII keysym fallback when no input method owns focus; unsupported Unicode
there stays on the clipboard. Requests longer than 500 characters use paste.
A queued keyboard dispatch is not acknowledgement that the application accepted
it; users should verify their target application and keyboard layout.

Metadata targets GNOME 45–50. GNOME 50 omits the removed X11 input-region option.
Syntax, schemas, runtime states, services and cleanup were checked in an isolated
headless GNOME 50.1 session; older versions and physical application delivery
still need desktop acceptance. After updating installed JavaScript, log out and
back in to reload cached modules. See
[overlay validation](../packaging/gnome-extension/tests/VALIDATION.md) for checks,
screenshots and the manual acceptance checklist.

## Wayland automatic paste

`wl-copy` owns the clipboard. Paste uses `ydotool key` only when an existing
ydotool daemon socket is found, using `YDOTOOL_SOCKET`,
`$XDG_RUNTIME_DIR/.ydotool_socket`, or `/tmp/.ydotool_socket`. Install the
distribution's `ydotool` package and configure its daemon with permission to
open `/dev/uinput`; daemon service names and packaging differ across versions.
Follow the distribution's user/group and socket-permission instructions. XFlow
does not start a privileged daemon or modify device permissions.

Export the same `YDOTOOL_SOCKET` in the environment that starts the XFlow daemon
if your service uses a different path. Successful keyboard dispatch reports
`pasted`; it is not an accessibility acknowledgement that an application
accepted the text. Missing tools, unavailable daemon, failed keyboard commands,
changed focus, or undiscoverable original app/window identity report `clipboard_only` after
copy succeeds. Failed clipboard ownership reports an error, preserving the
transcript for `xflow last` and later copy.

GNOME focus identity comes from the extension's `org.xflow.Shell.Context` method;
without it, automatic paste is intentionally disabled. Text is passed to
clipboard helpers over stdin, never interpolated into shell commands. XFlow
does not refocus windows. Single-line terminals use Shift+Insert; multiline
terminal text and any text with unknown app identity remain clipboard
only so pasting cannot silently execute a newline. Focus is checked immediately
before keyboard dispatch, but another focus change can still race with synthetic
keys; use `injection.clipboard_only = true` when that residual risk matters.

## Desktop capability matrix

| Session | Clipboard | Automatic paste | Global shortcuts / overlay |
| --- | --- | --- | --- |
| Ubuntu GNOME Wayland | `wl-copy` | `ydotool`, original-window guard via extension | Bundled GNOME extension |
| Fedora GNOME Wayland | `wl-copy` | Same adapter; distro uinput/ydotool setup required | Same extension API; target validation pending |
| GNOME X11 | `xclip` | `xdotool`, original-window guard | Bundled GNOME extension |
| Other X11 desktops | `xclip` | `xdotool`, original-window guard | Bind CLI commands in desktop settings; native pill pending |
| wlroots Wayland (Sway, etc.) | `wl-copy` | Clipboard only until compositor focus adapter exists | Bind CLI commands in compositor config; layer-shell pill pending |
| KDE Plasma Wayland | `wl-copy` | Clipboard only until KWin focus adapter exists | Bind CLI commands in KDE settings; native overlay pending |
| macOS / Windows | Pending concrete adapter | Pending concrete adapter | Shared core traits available; no shipping integration |

On Fedora the native build dependency is `alsa-lib-devel` with `pkgconf-pkg-config`
and the C toolchain. GNOME Wayland does not support layer-shell; the tiny native
St/Clutter actor runs inside GNOME Shell and does not steal input focus. Separate
layer-shell and KWin/X11 integrations remain future adapters. An unavailable
session bus is nonfatal to CLI/TUI operation; the GNOME pill will be unavailable.

The pill hides in idle. Audio events update the waveform at no more than 30 Hz;
processing has no repeating animation timer. Success and errors use one-shot
hide timers. Extension disable disconnects bus and settings subscriptions,
removes shortcuts and actors, and terminates owned CLI subprocesses.

## Verification and manual release checks

```sh
cargo test -p xflow-platform
dbus-run-session -- cargo test -p xflow-platform -- --ignored
node --input-type=module --check < packaging/gnome-extension/extension.js
glib-compile-schemas --strict --dry-run packaging/gnome-extension/schemas
```

Automated checks cover bounded capture, finite normalized PCM/RMS, silence
rejection, terminal/newline and focus guards, and D-Bus signal delivery/shutdown.
They do not need a real microphone or modify the desktop clipboard. Before a
release, verify capture and cancel using a real microphone, ydotool/uinput setup,
paste into a text editor, changed focus and terminal multiline fallbacks,
extension enable/disable, shortcuts, multi-monitor position, and lock-screen
behavior. Measure hotkey-to-capture and overlay frame time in a real GNOME
session; syntax checks cannot establish those timings.

Primary API references:

- [CPAL 0.15.3](https://docs.rs/cpal/0.15.3/cpal/) (the implementation's pinned major/minor line).
- [GNOME extension module API](https://gjs.guide/extensions/topics/extension.html).
- [Mutter keybinding flags](https://gnome.pages.gitlab.gnome.org/mutter/meta/flags.KeyBindingFlags.html).
- [Mutter key handler callback](https://gnome.pages.gitlab.gnome.org/mutter/meta/callback.KeyHandlerFunc.html).
- [Layer-shell supported desktops](https://github.com/wmww/gtk-layer-shell#supported-desktops).
- [ydotool key and daemon socket manpage](https://github.com/ReimuNotMoe/ydotool/blob/master/manpage/ydotool.1.scd).
