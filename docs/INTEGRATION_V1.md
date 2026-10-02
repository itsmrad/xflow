# xflow v1 integration — 2026-10-02

The recovered Orca coordinator integrated the existing feature branches on
`itsmrad/feat-v1`, preserving their commit history. Contracts PR #3 was merged
first; PR #4 carries the combined development build into `main`. All ten exact
Codex sessions (nine feature workers and research/integration) were resumed in
visible Orca terminals and retained for inspection.

## Accepted feature checkpoints

Each checkpoint below was verified as an ancestor of the combined tree with
`git merge-base --is-ancestor`. Existing GNOME 50 compatibility and the daemon's
capture-completion, delivery, cancellation and history fixes are preserved.

| Scope | Accepted commit | Recovery outcome |
| --- | --- | --- |
| Contracts/config | `f810583eae41cd87ba17c6d488831e4b73634345` | Private atomic config saves and bounded GNOME preferences reviewed and tested |
| Platform | `2377ac86c29e9d6f5bbefbc6910a89f57f161e0e` | Capture, native Shell integration and guarded utility fallbacks revalidated |
| Providers | `1b58d8261a305aebc9a88f8628754f848eb74782` | Batch adapters, model catalog, local protocols and safe warm-up verified with mocks |
| Overlay | `77ab63546d6b1d887b907501065a9947d92ec06c` | GNOME shortcuts, rendering/preferences and lifecycle fixtures revalidated |
| Performance | `588af17ae566868002350866a73b8f929dd67461` | Existing profile/baseline evidence and benchmark safety reviewed |
| CI | `dc5dd46c9082e0815cdb809e5c3fc2f5684ea965` | Fixed safe warm-up probes consuming upload plans; retained strict multipart assertions |
| Daemon | `9893393cf95749c5a37720c6a3e3e05eb8e9a839` | Integrated provider/CI updates and verified retention, delivery and reload behavior |
| CLI | `3f1262feb98fda79c4f08a1b41c48fa03ca11f76` | Fixed cancelled partial IPC reads; completed acceptance and latency report |
| TUI | `51974a4a200b50c7e84ece4d917ad6cc42c6dba3` | Completed onboarding/probes; configuration, history and PTY verification passed |

Research completed separately without speculative feature additions. The
integration worker preserved all merges and documentation before its final turn
stopped at a provider usage limit. The coordinator fenced that assignment,
retained its terminal, and finished directly on the same integration branch;
no successful `worker_done` was claimed for the interrupted integration attempt.

The final compatibility change replaces the deprecated atomic `fetch_update`
with a Rust 1.88-compatible compare/exchange loop. A concurrent test verifies
the four-cue limit and release/reuse of slots. Documentation now describes the
implemented v1 scope and distinguishes it from physical acceptance gates.

## Combined verification

Verification used private worktree-local XDG/TMPDIR state, a private D-Bus
session, synthetic audio, fake desktop tools and loopback providers. Cargo ran
through the shared build gate with local Rust 1.98.1. Real microphone,
clipboard, installed extension, service, credentials and paid APIs were not
used. Verification logs are retained in the integration worktree's `target/iv`.

- `scripts/check.sh`: formatting, Clippy with warnings denied, JavaScript syntax,
  all 38 extension tests, strict schema compilation, 140 Rust unit tests,
  11 daemon end-to-end tests, three private-bus tests and two fake-tool fallback
  tests passed on the final source tree.
- Production `cargo build --release --locked`, without test-support, passed.
- The production CLI acceptance script passed config/personalization,
  fake system tools, IPC/history/listen/cancel, errors and completions.
- Production TUI PTY smoke passed online and offline at 80×24 and 120×35:
  settings saved, terminal restored and tiny resize handled. Both connected
  idle samples consumed zero CPU ticks; this is a short observation.
- Short headless production benchmark passed with ten status samples and a
  one-second idle interval. It measures startup, daemon RSS/CPU and raw Unix
  status IPC only; it does not measure CLI process startup or dictation latency.
- All accepted feature checkpoints are ancestors; the final diff was checked
  for whitespace errors and merge markers.

GitHub CI separately checks current Rust stable and the workspace MSRV (1.88).
Only a successful run on the final PR head is accepted before the main merge.

## Practical limits

Physical microphone/shortcut behavior, real provider credentials, actual
destination paste and compositor performance remain unverified. Compatible
local servers/models are user-managed; managed models and streaming are future
work. Historical benchmark files retain their original fixture scope. The CLI
worker's 100-sample optimized fake-daemon result was 2.228 ms median and 2.918 ms
p95; the approximate 1 ms aspiration was not reached. This is a development
build, not a production-release certification.
