# Changelog

## 0.2.0-beta — 2026-10-29

Beta for macOS 13+ (universal: Apple silicon and Intel; `.pkg` or Homebrew) and Linux (tarball).

The macOS binaries are signed with xflow's own long-lived certificate, not an Apple Developer ID, and are **not notarised** (Developer ID is pending). When you first run `xflow`, setup asks macOS for Accessibility and Microphone for `xflowd`, and asks for Input Monitoring only if your shortcut key can't be heard without it. Because every future release is signed with the same certificate, those permissions carry over to `.pkg` updates. A Homebrew upgrade may ask again until xflow ships as an app bundle (#19). Install from a terminal with `sudo installer -pkg xflow-0.2.0-beta.pkg -target /`. A `.pkg` downloaded in a browser is blocked until you allow it in **System Settings → Privacy & Security → Open Anyway**.

### Dictation
- **20-minute dictations.** Long recordings are cut into 30–60 s segments at a pause and transcribed while you are still talking, so stopping a 12-minute dictation gives you text in about 0.6 s. If one segment fails, it is retried once and the rest is still delivered, with a warning. The default limit goes from 2 to 20 minutes, and the cleanup step no longer truncates long text. (#85)
- **Your developer terms come out spelled right.** Words in your dictionary and vocabulary, including the ones xflow learned from your corrections, are restored after transcription, so "next js" becomes `Next.js`. This runs locally, after transcription and before cleanup. In a 156-sentence prose test it changed 0 words that should stay as they are. Turn it off with `formatting.snap_to_dictionary = false`. (#82)
- **More dictionary hints reach the speech model.** The hint list is now measured in tokens against Whisper's 224-token limit (up to 200 estimated tokens) instead of 224 bytes, which sends 1.8× more terms. Exact dev-term recall goes from 47% to 53% on the default model and from 50% to 61% on large-v3. (#89)
- **Spoken lists become lists.** "three roles one developer two designer three manager" becomes a numbered Markdown list. "First… second…" and "bullet point…" work too. This runs locally and works without AI cleanup. (#81)
- **Optional: ignore sound from your speakers** while dictating, using macOS echo cancellation: `xflow config set recording.ignore_speakers true`. It is off by default. (#58)

### macOS
- **Paste is about 40 ms faster.** The clipboard is read and written in-process instead of by running `pbcopy`/`pbpaste` five times per paste. (#88)
- **Plain `xflow` opens setup.** It walks through only the steps that are missing and ends with a "hold Fn and say something" test. The installer no longer opens a Terminal window by itself. (#61)
- **No more Keychain password prompts.** Provider API keys now live in xflow's own encrypted store (a 0600 file in your config directory). A key saved by an older version moves over the first time you run `xflow key status` in a terminal, and macOS asks you to confirm once. Or re-enter it with `xflow key set <provider>`, with no prompt at all. (#80)

### Fixes
- If `xflow listen` or the TUI exits while recording (closed terminal, `kill -9`), the recording stops instead of running until the time limit. `xflow start` and `xflow toggle` still leave the recording running, as before. (#87)
- Fixed a flaky macOS daemon test. (#83)

### For contributors
- The repo is a monorepo: the Rust workspace is in `apps/cli` and the website in `apps/web`. (#71) The website design spec is in `docs/web/DESIGN.md`. (#72)
- Release packages include the licences of embedded third-party data: SCOWL word lists and OpenAI's BPE tables.
- Debug builds of `xflowd` can dictate from a WAV file (`XFLOW_TEST_AUDIO`), for QA without a microphone. (#79)
- The backend for xflow Pro (accounts, CLI device sign-in, billing, hosted transcription) is in `apps/web`. The CLI cannot sign in to it yet. (#78)

## 0.1.0 (not published)

First version: the CLI, TUI and `xflowd` daemon; Fn hold-to-talk and double-tap hands-free on macOS; GNOME on Linux; a dictionary that learns your corrections; and BYOK speech-to-text and cleanup.
