# Native Linux desktop setup

The Ubuntu MVP uses CPAL/ALSA for capture and a GNOME Shell extension for shortcuts,
the floating pill, and focused-window identity. The daemon does not depend on GTK,
Electron, a WebView, or a local model runtime. The microphone opens on `start` and
closes on `stop`, `cancel`, capture error, or the recording duration limit. The
recording buffer is bounded by `recording.max_seconds`; a lightweight RMS detector
rejects silence while preserving internal pauses. It does not automatically stop
after a pause or classify noise versus speech.

## Ubuntu GNOME

Build dependencies and clipboard tools:

```sh
sudo apt install build-essential pkg-config libasound2-dev wl-clipboard xclip xdotool
cargo build --release
```

Install the extension (run from the repository root):

```sh
mkdir -p "$HOME/.local/share/gnome-shell/extensions/xflow@xflow.local"
cp -R packaging/gnome-extension/. "$HOME/.local/share/gnome-shell/extensions/xflow@xflow.local/"
glib-compile-schemas "$HOME/.local/share/gnome-shell/extensions/xflow@xflow.local/schemas"
```

Log out and back in on Wayland so GNOME discovers the extension, then run:

```sh
gnome-extensions enable xflow@xflow.local
```

Ensure `xflow` is on the GNOME session's PATH. Alternatively, set `command-path`
to its absolute installed path using the local schema:

```sh
gsettings --schemadir "$HOME/.local/share/gnome-shell/extensions/xflow@xflow.local/schemas" \
  set org.gnome.shell.extensions.xflow command-path '/absolute/path/to/xflow'
```

The same `gsettings --schemadir …` prefix configures `toggle-shortcut`,
`cancel-shortcut`, `copy-shortcut`, `paste-shortcut`, optional `start-shortcut` and
`stop-shortcut`, `position` (`top` or `bottom`), `width`, `height`, `margin`,
`opacity`, and `animation`. Defaults are Super+Alt+Space to toggle,
Super+Alt+Escape to cancel, Super+Alt+C to copy, and Super+Alt+V to paste last.
Shortcut bindings apply in the normal desktop session, outside the lock screen.

The extension uses the GNOME 45+ ES module API. Its metadata lists 45–51 as
candidate versions using that API; this development environment validates JS
syntax and GSettings schemas, not behavior in all those GNOME sessions. Validate
on the target desktop before release.

The current GNOME adapter provides toggle and explicit start/stop shortcuts.
It does **not** provide verified press-and-release push-to-talk. Mutter's newer
`TRIGGER_RELEASE` flag exposes both transitions, but reliable integration must
identify callback event types and be tested across versions before enabling it.
Do not label a press-only binding push-to-talk. A compositor with independent
press/release bindings can bind `xflow start` and `xflow stop` to those events.

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
changed focus, or undiscoverable original focus report `clipboard_only` after
copy succeeds. Failed clipboard ownership reports an error, preserving the
transcript for `xflow last` and later copy.

GNOME focus identity comes from the extension's `org.xflow.Shell.Context` method;
without it, automatic paste is intentionally disabled. Text is passed to
clipboard helpers over stdin, never interpolated into shell commands. XFlow
does not refocus windows. Single-line terminals use Shift+Insert; multiline
terminal text and multiline text with unknown app identity remain clipboard
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
- [Layer-shell supported desktops](https://github.com/wmww/gtk-layer-shell#supported-desktops).
- [ydotool key and daemon socket manpage](https://github.com/ReimuNotMoe/ydotool/blob/master/manpage/ydotool.1.scd).
