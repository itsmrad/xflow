# macOS Electron/Chromium delivery — issue #25

Measured 2026-11-02 on the owner's Apple M2 Mac, using the worktree's platform test
executable and existing Accessibility permission. No daemon was started, stopped,
replaced or reconfigured; the running Homebrew `xflowd` retained PID 43484.
Only a blank Chrome data-URL textarea and a temporary VS Code text file received
the sentinels `xflow25-00 ` through `xflow25-19 `.

| App | Successful pastes | Injector receipts | Attribute read-back / blocker |
| --- | --- | --- | --- |
| Chrome 156.0.8078.76 | 20/20, AX insertion receipts | 20 confirmed | 20/20 unchanged: Manual unsupported, Enhanced false |
| VS Code 1.144.0 | 20/20, exact saved scratch-file sequence | 20 uncertain; opaque AX value | 20/20 restored/unchanged: Manual false, Enhanced false |
| Slack | Not tested | — | Not installed in `/Applications` |
| Cursor | Not tested | — | Not installed in `/Applications` |
| Discord | Not tested | — | Not installed in `/Applications` |
| Notion | Not tested | — | Not installed in `/Applications` |
| Arc | Not tested | — | Not installed in `/Applications` |
| Linear desktop | Not tested | — | Not installed in `/Applications` |

These are injector trials, not microphone/provider or full dictation trials.
Initial probes refused to type until the explicitly titled scratch window was
focused. No permission prompts, real chats or documents were used.

## Root cause and behavior

The previous implementation wrote `AXManualAccessibility` without restoring it.
Chrome rejects that attribute (`kAXErrorAttributeUnsupported`); probing its
focused element initially returned no value. Reading the application's `AXRole`
then exposed `AXTextArea`. Chromium's
[application-role implementation](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/chrome/browser/chrome_browser_application_mac.mm)
activates native accessibility on that read. xflow now requests the app role for
the known Chromium/Electron bundle IDs, and for any app in `/Applications` or
`~/Applications` whose bundle has a framework containing Chromium's
`Resources/chrome_100_percent.pak` (Electron Framework, Edge, Brave, Helium…;
cached per bundle), and retains its bounded asynchronous focus polling. It never writes `AXEnhancedUserInterface`, avoiding that flag's
screen-reader debounce/window side effects. No visual animation certification
is claimed.

If Electron still needs `AXManualAccessibility`, xflow changes it only after a
successful boolean read, keeps the scoped guard through delivery/receipt polling,
and restores and reads back the original value on scope exit, including errors.
An originally enabled attribute is left enabled; an unreadable original is never
guessed. A cancelled caller cannot drop the guard before its pending blocking
delivery worker finishes (bounded by the existing five-second receipt hold).
A failed restoration read-back emits a warning; an unresponsive/exited target
cannot be guaranteed to accept restoration.

Previously, an empty AX value (VS Code) or disabled clipboard restoration could
report `Pasted` without a receipt. Now only a receipt confirms a paste; otherwise
the outcome is `uncertain`, the pill says **Pasted — check it landed**, and the
transcript stays copied and available through `last`. A field exposing no value
replies at once and an empty value within `restore_delay_ms`; the delivery lock
still waits for a late receipt up to five seconds. Unknown editability or a
missing/changed window copies without sending keys; its pill says **Copied**.
Secure fields remain refused. Uncertain pastes do not start correction learning.

## Reproduce safely

Open only a scratch textarea/file whose window title contains
`XFlow delivery scratch 25`, focus it, then run:

```sh
export CARGO_TARGET_DIR=/Users/mrad/code/projects/xflow/target-shared
export CARGO_BUILD_JOBS=3
# Separate this worktree's artifacts from other workers sharing the default target.
export CARGO_BUILD_TARGET=aarch64-apple-darwin
XFLOW_PASTE_APP=com.google.Chrome cargo test --manifest-path apps/cli/Cargo.toml \
  -p xflow-platform --lib native_chromium_paste_matrix --locked -- --ignored --nocapture
# For the scratch VS Code file, use XFLOW_PASTE_APP=com.microsoft.VSCode instead.
```

The opt-in test checks existing permissions, app/window identity, the scratch
title and secure-field status before typing. It checks both AX flags after each
trial and waits for the pending worker to release its delivery lock. An
uncertain receipt is an attempted paste, not proof of insertion: the VS Code
result above was independently verified by saving the explicitly named scratch
file and comparing all 20 sentinels in order.

Unit coverage includes known app/container classification, unknown/changed focus,
secure-field refusal, original true/false/unreadable flags, failed writes and
scope exits, explicit uncertain outcomes, pill event filtering, and daemon
`last` retention without correction learning. Linux D-Bus/GNOME runtime checks
remain outside this Mac run.
