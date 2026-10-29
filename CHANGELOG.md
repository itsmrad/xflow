# Changelog

## 0.2.0-beta — 2026-10-29

Beta for macOS on Apple silicon (`.pkg` or Homebrew) and Linux (tarball).
The macOS build is not notarised by Apple yet; see the release notes for how to install it.

### Dictation
- **20-minute dictations.** Long recordings are cut into 30–60 s segments at a pause and transcribed while you are still talking, so stopping a 12-minute dictation gives you text in about 0.6 s. If one segment fails, it is retried once and the rest is still delivered, with a warning. The default limit goes from 2 to 20 minutes, and the cleanup step no longer truncates long text. (#85)
- **Your developer terms come out spelled right.** Words in your dictionary and vocabulary, including the ones xflow learned from your corrections, are restored after transcription, so "next js" becomes `Next.js`. This runs locally, after transcription and before cleanup. In a 156-sentence prose test it changed 0 words that should stay as they are. Turn it off with `formatting.snap_to_dictionary = false`. (#82)
- **More dictionary hints reach the speech model.** The hint list now uses Whisper's full 224-token budget instead of 224 bytes, which sends 1.8× more terms. Exact dev-term recall goes from 47% to 53% on the default model and from 50% to 61% on large-v3. (#89)
- **Spoken lists become lists.** "three roles one developer two designer three manager" becomes a numbered Markdown list. "First… second…" and "bullet point…" work too. This runs locally and works without AI cleanup. (#81)
- **Optional: ignore sound from your speakers** while dictating, using macOS echo cancellation: `xflow config set recording.ignore_speakers true`. It is off by default. (#58)

### macOS
- **Paste is about 40 ms faster.** The clipboard is read and written in-process instead of by running `pbcopy`/`pbpaste` five times per paste. (#88)
- **Plain `xflow` opens setup.** It walks through only the steps that are missing and ends with a "hold Fn and say something" test. The installer no longer opens a Terminal window by itself. (#61)
- **No more Keychain password prompts.** Provider API keys move to xflow's own encrypted store (a 0600 file in your config directory). Keys saved in the Keychain by older versions are moved over once, silently. (#80)

### Fixes
- If the `xflow` command that started a recording exits (closed terminal, `kill -9`), the recording stops instead of running until the time limit. (#87)
- Fixed a flaky macOS daemon test. (#83)

### For contributors
- The repo is a monorepo: the Rust workspace is in `apps/cli` and the website in `apps/web`. (#71, #72)
- Debug builds of `xflowd` can dictate from a WAV file (`XFLOW_TEST_AUDIO`), for QA without a microphone. (#79)
- The backend for xflow Pro (accounts, CLI device sign-in, billing, hosted transcription) is in `apps/web`. The CLI cannot sign in to it yet. (#78)

## 0.1.0

First release: the CLI, TUI and `xflowd` daemon; Fn hold-to-talk and double-tap hands-free on macOS; GNOME on Linux; a dictionary that learns your corrections; and BYOK speech-to-text and cleanup.
