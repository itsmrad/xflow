# XFlow v1 overlay validation

The extension runtime consists of `extension.js`, `logic.js`, `prefs.js`,
`stylesheet.css`, `metadata.json` and the XML schema under `schemas/`.
Do not install the tests or their temporary artifacts.

## Automated checks

- 38 focused JavaScript tests: waveform/theme/geometry, smart/hold/toggle,
  physical modifier masks, boundary parsing, command ordering/stale replies,
  locked/disabled services, focus changes, modifier cleanup, Unicode IM typing,
  and generation-safe clipboard restoration and disable/re-enable callback isolation.
- Module syntax checks for every runtime JavaScript file.
- Strict GSettings schema compilation.
- `scripts/check.sh` through the shared Cargo gate passed after merging the
  approved contract revision `f810583`, with workspace-backed `TMPDIR`: format,
  Clippy, 46 enabled Rust tests (one desktop bridge test remains ignored). The
  schema persistence/reset/range regression matches the v1 settings.
- Isolated headless GNOME Shell 50.1: all states, screenshots, D-Bus
  Context/Version/Selection and unknown-target injection, queued start/stop,
  actual virtual trigger-release and modifier-first-release push-to-talk,
  quick-tap latch/next-press stop, reduced motion, zero idle sources,
  removal of all sources and accelerators on disable, and clean re-enable.
- Preferences construction and visual rendering passed in the isolated Shell
  with `GSK_RENDERER=cairo`; the screenshot shows the native pages and preview.
  The default GPU renderer did not map this window in the headless environment
  within the test deadline, so physical GPU behavior remains manual acceptance.

The isolated bus forbids unrelated service autostarts. It uses temporary XDG
settings/data/runtime directories and a mock daemon, with no microphone,
provider calls, real clipboard, installed extension, or real daemon access.
All explicitly started processes are terminated after the smoke run.

## States

| State | Appearance and lifetime |
| --- | --- |
| Idle | Hidden by default; optional static 36×8 logical-pixel handle; no periodic source |
| Listening | Off-white 40-pixel capsule; dark center-weighted rounded bars, fast attack and slower decay; at most about 30 frames/s |
| Processing | Five compact dots with a traveling shimmer while motion is enabled |
| Success | Checkmark and optional short label, then collapse; default hold 900 ms |
| Error | Muted red border/icon and at most 72 message code points; default hold 4000 ms |
| Command | Muted violet accent during listening/processing; same gesture rules |

Dark reverses the capsule/bar colors; auto follows desktop color-scheme live.
Both motion controls must be enabled for animation. Transcripts are never logged.

## Captured artifacts

A successful state/service/PTT run is under:

`target/overlay-smoke/smoke-rz727ad0/`

It includes `idle.png`, `listening.png`, `processing.png`, `success.png`,
`error.png`, `command-dark.png`, `shell.log`, `prefs.log` and `results.json`.
These are local validation artifacts, excluded from Git. `prefs.png` shows the
rendered window. The optional reproducible harness is `tests/gnome-smoke.py`: run
`/usr/bin/python3 packaging/gnome-extension/tests/gnome-smoke.py`. It requires
GNOME 50, GJS and system Python GI; artifacts are written under `target/overlay-smoke`.

## Reproduce focused checks

```sh
node --test --test-isolation=none packaging/gnome-extension/tests/logic.test.mjs packaging/gnome-extension/tests/extension.test.mjs
for file in packaging/gnome-extension/*.js; do node --input-type=module --check < "$file"; done
glib-compile-schemas --strict --dry-run packaging/gnome-extension/schemas
PATH=/home/mrad/.cache/xflow-dev/bin:$PATH sh scripts/check.sh
```

If shared temporary storage is constrained, set `TMPDIR` to a directory under
this worktree’s `target/` before repository checks. Never bypass the Cargo gate.
The research specs `wispr.md` and `platform.md` remained absent through final
verification; implementation followed the assignment and the shared contract.

## Manual acceptance checklist

1. Install only the runtime assets and compile schemas, then log out/in on
   Wayland. Start the v1 daemon and open `gnome-extensions prefs xflow@xflow.local`.
2. Tap Dictate: listening appears immediately and remains after release. Press
   again: processing appears. Hold Dictate beyond 300 ms: releasing either the
   trigger key or a required modifier stops. Repeat in hold and toggle modes,
   with Num Lock/Caps Lock, and after editing a shortcut.
3. Confirm command-mode dictation uses the PRIMARY selection and distinct accent.
   Test explicit start/stop, configured cancel, copy-last and paste-last. Confirm
   plain Escape is available to applications when XFlow is idle.
4. Switch themes, every corner/center position and monitor policy. Change size,
   opacity, bars/style, offsets, message visibility and hold durations. Check
   mixed HiDPI monitors, fullscreen targets and monitors being added/removed.
5. Disable system animations and extension animations independently. Verify
   static feedback and no animation sources; verify hidden/minimal idle.
6. Verify native paste in a controlled editor and terminal with the desired
   layout. Change focus during recording: delivery must become clipboard-only.
   Check clipboard restoration preserves a new user copy. Verify Unicode typing
   in an IM-enabled client and clipboard recovery without usable IM focus.
7. Lock/unlock and disable/enable the extension. Confirm no locked injection,
   remaining Escape grab, repeated callbacks or recurring idle timer. With the
   daemon absent, confirm the explicit start-daemon message.

GNOME 45–49 and delivery to physical applications remain manual acceptance.
Raw keysym typing depends on the active layout; Unicode without IM focus remains
clipboard-only. Long type requests use paste. Dispatch reports do not guarantee
application acceptance. Rust retains terminal/multiline safety policy.
