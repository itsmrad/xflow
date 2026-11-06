# Optimization round 2 (Linux, 2026-11-06)

Starting point: 700ff03474fd859fc248ee356e260e37b6409ef9. Compiler: rustc 1.99.1; x86_64-unknown-linux-gnu; release fat LTO, one codegen unit, unwind. Never change limits.binary_pct (8%).

## Method and boundaries

The reported 11.27 MB → 12.14 MB CLI and 12.88 MB daemon have no supplied target, exact byte receipts or compiler provenance. Linux ELF measurements below are not a reconstruction of macOS Mach-O sizes. Compare d7a658c (before gateway/chat/meeting worker) with 700ff03 using the same compiler and Cargo.lock at each revision. Attribution uses GNU nm symbol sizes from otherwise identical release binaries with stripping disabled at the final binary; report text-symbol bytes separately from total stripped file bytes. Fat LTO inlines across crate boundaries, so symbol owners are attribution, not exact exclusive feature costs.

cargo-bloat is absent; no tools installed. GNU nm is the available equivalent. cargo tree --duplicates separates build/test-only duplication from linked code.

Two external research opinions were not launched: cooperative admission refused a child (available RAM minus worker reservation below 2 GiB; a second reading also reported CPU 99% above 85%). AWS Cursor quota is unknown, so Grok cannot be routed there. Scoped implementation proceeds from the approved user plan and internal measurements; this limitation is explicit, not an independent review claim.

## Daemon boundary

Source trace: xflowd → daemon/transport/store/providers. No call into xflow-app::meeting journal, transcription worker, context tools, chat or xflow-providers::llm model client. Linked-symbol verification follows. The microphone fanout lease is required for concurrent dictation/meeting capture (#159); removing it would remove functionality and is outside this optimization. Provider catalog entries used by key migration are also retained.

## Duplicate dependencies

Runtime: base64 0.22 (reqwest/provider) and 0.23 (hyper-util), hashbrown 0.15 (ratatui/lru) and 0.17 (rusqlite/indexmap), rustix 0.38 (crossterm) and 1.1 (tempfile), unicode-width 0.1 and 0.2 (ratatui). These are transitive major-version API differences; no blind lockfile unification. digest 0.11/sha2 0.11/nix 0.31 are rmcp dev-only, not a release duplication. serde_json preserve_order was added for editing external MCP client JSON; keep it to preserve user configuration order. HTTP/2 and native audio remain enabled.

## Mac desktop comparison required later

Use the primary checkout's .reports/optimize/RUN-COMPARISON.md with exclusive desktop lock and an empty AeroSpace workspace. Need signed matching before/after native macOS binaries, exact compiler/target/features/provenance, fixed app versions/settings, microphone/accessibility grants, matched ready-idle process trees, loaded local models reported separately, five-minute CPU/RSS/footprint samples, varied speech corpus and stop-to-file timestamps. Close Kivi/Wispr/Handy during Xflow synthetic-key tests and restore them afterward. Competitor measurements run separately under matched conditions. No native hotkey, microphone, paste, pill, battery or cross-product fastest/lightest claim is validated by Linux results.

## 700ff03 linked text attribution (bytes)

`cargo +1.99.1 rustc --manifest-path apps/cli/Cargo.toml --release --locked -p xflow-app --bin <binary> -- -C strip=none`, then `nm -S --size-sort --demangle`. Sum text (`t/T`) symbol sizes by defining crate; generic code remains attributed to its owning type. Baseline production file bytes: xflow **12,385,536**, xflowd **13,148,288**. For xflow, .text SHA256 is identical in symbol and production builds: 738beb0f87ae1f6590d48ceaf62b71683c71a359d93eca214bf24532a5bd6050. Re-stripping changes ELF bookkeeping by eight bytes, so use the original stripped production build for file-size deltas.

| Owner | xflow text B | xflowd text B |
| --- | ---: | ---: |
| C/other (primarily SQLite) | 2,179,862 | 2,180,149 |
| xflow_app | 1,055,041 | 418,136 |
| xflow CLI binary | 954,278 | — |
| core | 960,934 | 1,068,342 |
| zbus | not top 20 | 985,773 |
| zvariant | not top 20 | 516,569 |
| tokio | 300,243 | 672,216 |
| rustls | 364,967 | 364,974 |
| xflow_providers | 376,309 | 200,138 |
| toml_edit | 261,313 | 310,626 |
| clap_builder | 278,389 | 220,473 |
| h2 | 159,836 | 159,836 |
| flacenc | 120,817 | 120,817 |

Subsets, not additive to the owner table: CLI TUI **525,458 B**, meeting library **251,244 B** plus meeting CLI **151,905 B**, model gateway **137,713 B**, chat **135,408 B**. Daemon has **0 text-symbol bytes** under xflow_providers::llm, xflow_app::meeting, xflow_app::chat or xflow_app::tui. Shared microphone fanout has 2,806 B of named text; retaining it preserves the required microphone contract. No model gateway or meeting journal/worker code leaked into the daemon.

Both binaries contain FTS3 **156,705 B**, R-tree **30,880 B**, FTS5 **232,535 B** of named text. FTS5 is used by history and meeting search and stays enabled. The candidate disables only FTS3, its parenthesis option, R-tree and dbstat using libsqlite3-sys's documented LIBSQLITE3_FLAGS override. No schema or SQL changes. Normal Cargo builds from the repository root or apps/cli pick up .cargo/config.toml, including release packaging and CI. A caller building via --manifest-path from outside the repository may miss the optimization; functionality remains identical.

## Baseline runtime

Scoreboard isolated release daemon, no native hotkey, microphone, pill or inference; private D-Bus, no service activation, XFLOW_NO_KEYCHAIN=1. Three startup samples and one 30-second idle window, 5-second warmup. Before: median PSS **9,891 KiB**, RSS **12,144 KiB**, CPU **0.0%**, voluntary context switches **0/s**; cold startup p50 **26.49 ms**, warm p50 **5.02 ms**. Shared host was building other teams' work; timing changes below that noise are not claimed as wins. This is not the loaded native macOS product.

Daemon .text SHA256 also matches between production and symbol builds: 9d8b9a0c1b4033c36dd6723f9c6a7cee54038e0403ccb7b065d77e2bab477800.

## Retained change: trim unused SQLite extensions

Same release build, before → after:

| Binary | Before B | After B | Saved B |
| --- | ---: | ---: | ---: |
| xflow | 12,385,536 | 12,099,872 | **285,664 (2.31%)** |
| xflowd | 13,148,288 | 12,862,624 | **285,664 (2.17%)** |

Retain the change: both production binaries shrink without removing any SQL used by Xflow. The exact file savings include module data, initialization and code, not only the named FTS3/R-tree text above. No daemon architecture or meeting/gateway file was changed. limits.binary_pct remains 8%.

After runtime: PSS **9,896 KiB** (5 KiB higher), RSS **12,076 KiB** (68 KiB lower), CPU **0.0%**, voluntary context switches **0/s**. Cold/warm p50 **20.40/2.98 ms** versus **26.49/5.02 ms** before. Do not claim an idle-memory, CPU or startup win: PSS is flat and shared-host timing is uncontrolled. Only the binary reduction is a demonstrated improvement.

Validation: XFLOW_NO_KEYCHAIN=1 cargo +1.99.1 test --manifest-path apps/cli/Cargo.toml --workspace --locked --features xflow-app/test-support. Application/storage/integration suites passed, including meeting journal crash/recovery tests. The command stopped at an unrelated core wall-clock assertion: lists::tests::under_a_millisecond_on_two_thousand_chars observed 47.904859 ms under simultaneous shared-host builds; 137 other core tests passed, two ignored. No timing threshold or test was weakened. Remaining workspace suites were not reached by this command; CI is authoritative for the complete pass.

Independent Codex review (OCR delegate selection/rules): 2/2 files reviewed, no skips; config selected by OCR, report manually reviewed despite Markdown filtering. No product safety blocker or macOS functionality loss found. One validation issue fixed: add .cargo/** to the scoreboard production-binary cache key so this config change cannot reuse old binaries. The one-line cache fix and later numeric documentation updates were not independently re-reviewed (one-pass rule). Rebased onto 026dfc9 (#170, TUI short History); size receipts above isolate this change on requested 700ff03, before that unrelated TUI integration.
