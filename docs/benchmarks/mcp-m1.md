# MCP milestone 1 measurements — 2026-11-06

[Raw samples and binary hashes](mcp-m1.json). Baseline: `20e2c6b`; measured implementation source: `2779e64`, equivalent to delivered feature commit `68210a9` (the final cleanup changes history and documentation only).

Host: Mac14,2, Apple M2, 8 GB RAM, macOS 26.7.1, rustc 1.97.1. Sequential release builds use fat LTO, one codegen unit, stripping and unwind panics. All Cargo commands run through the project’s `cargo-locked` wrapper in this checkout’s `apps/cli`, using its own `target`; no shared target override. No version or rusqlite bump.

## Binary and package sizes

| Variant | `xflow`, bytes | Delta from baseline |
| --- | ---: | ---: |
| Baseline | 6,567,520 | — |
| Baseline + minimal read-only SQLite | 8,230,496 | +1,662,976 |
| Full M1 without JSON order preservation | 8,429,152 | +1,861,632 |
| Full M1 | 8,445,664 (8.05 MiB) | +1,878,144 |

The throwaway SQLite variant adds only a branch in baseline `main`: open an explicitly supplied temporary database with `Connection::open_with_flags(..., SQLITE_OPEN_READ_ONLY)`, run one `query_row("SELECT count(*) FROM sqlite_master", ...)`, and print the count. Its probe passes on a synthetic fixture. It contains no MCP or context code and is not committed. Original source bytes and lockfile are restored after every variant. The order-disabled variant changes only the serde_json feature, its optional lockfile edge, and `shift_remove` to `remove`; dependency versions stay fixed.

- MCP/context beyond SQLite: **215,168 B**, within the revised **250,000 B** budget.
- Isolated `preserve_order`: **16,512 B**, within **20,000 B**; retained.
- `xflowd`: **8,634,656 B (8.23 MiB)** before and after, **0 B** delta, within ±4 KB. `strings` finds **0** `search_dictations` occurrences. Equal byte counts do not assert byte-identical rebuilds; both hashes are recorded.
- Two-binary archive: **7,420,867 → 8,409,196 B**, **+988,329 B (+13.32%)**. This growth is an exact size finding, with no sampling noise. Both archives use the same deterministic USTAR entries, names, mode, timestamp and `gzip -9n`, matching release compression. macOS packaging normally signs/builds a `.pkg`; this comparison compresses the two binaries only, as allowed by the brief. No signing identity or installer was accessed.

## Database-free commands

Each primary condition has 20 fresh processes, alternating baseline/M1/baseline-repeat. Filesystem and code-sign caches are retained; `/usr/bin/time -l` overhead is included equally. These are process-start measurements, not cold-disk or first-install claims.

| Metric | Baseline | M1 |
| --- | ---: | ---: |
| `--version` p50 / max | 7.566 / 10.513 ms | 7.857 / 10.648 ms |
| `--version` maximum RSS | 7,864,320 B | 7,454,720 B |
| TUI first-frame p50 / max | 15.426 / 19.394 ms | 15.701 / 20.108 ms |
| TUI maximum RSS | 10,125,312 B | 10,027,008 B |

The initial `--version` p50 increase of 0.291 ms exceeded the first baseline-repeat median shift (0.097 ms), so three additional 20-sample rounds were collected. Their M1-minus-baseline p50 differences were −0.056, +0.367 and +0.288 ms; the baseline-repeat shift reached 0.605 ms. TUI baseline-repeat p50 shifted by 0.702 ms. The measured p50 differences stay within that observed variation; neither command’s measured maximum RSS grows. This does not establish a latency improvement or rule out smaller regressions.

First-launch warmups are retained separately: `--version` baseline/M1 547/558 ms and fresh TUI copies 238/225 ms. They include first-launch validation/cache costs, unlike the repeated samples above.

TUI safety: before and after every run, the only daemon must remain PID 73377, `/usr/local/bin/xflowd`. The TUI runs against that existing daemon, with temporary HOME/config/data, under `sandbox-exec` policy `(version 1)(allow default)(deny process-fork)`. A harmless child-launch self-check must fail before sampling. This is essential because reconnect can otherwise start a helper and install a per-user LaunchAgent. No service command is used. PTY size is 100×30; cursor-hide after buffer flush marks the first completed frame, then raw Ctrl-C exits. Terminal bytes are drained and discarded, never saved or printed. The expected service matched throughout, and no debug/test daemon survived. All other benchmark commands use isolated temporary runtime paths as well.

## MCP runtime budgets

Twenty fresh processes: initialize p50 **5.564 ms**, max **8.116 ms** (target p50 ≤50 ms). Maximum exit after stdin EOF: **0.545 ms** (target ≤1 s). File caches are retained, as above.

The fixture has 100,000 invented dictations, about 500 characters each, with a rare marker every 10,000 rows. Schema v3 exercises LIKE; v4 adds the external-content FTS5 table with `porter unicode61 remove_diacritics 2`, secure-delete, insert/delete/update triggers and a rebuild. The table/trigger DDL matches the context tests and gateway API handoff. No real history, client config, credential or model session is used.

| Search workload, 100 calls each | p50 | p95 | Max |
| --- | ---: | ---: | ---: |
| LIKE, newest | 0.374 ms | 0.630 ms | 7.856 ms |
| LIKE, rare term | 53.272 ms | 65.509 ms | 169.213 ms |
| LIKE, no match | 44.780 ms | 53.371 ms | 59.195 ms |
| FTS, rare term | 0.597 ms | 0.951 ms | 1.238 ms |
| FTS, common term | 57.037 ms | 61.869 ms | 106.115 ms |

All p95 values pass **≤100 ms**; two maximum outliers exceed 100 ms. Timings include JSON-RPC request/response over stdio, not model inference. Maximum RSS measured with `/usr/bin/time -l`, including five fresh-process confirmations per workload: rare LIKE **12,402,688 B**, missing LIKE **12,124,160 B**, common FTS **14,876,672 B (14.877 decimal MB / 14.19 MiB)**. The measured maximum passes **≤15 MB** with a small margin; it is not an upper bound for every history or host.

## Verification and remaining gates

- Exact macOS CI commands: workspace clippy with all targets, test-support and `-D warnings`; workspace tests with test-support; normal debug CLI build and generated docs. All pass. Generated CLI/config/provider references produce no diff.
- `fmt --all -- --check` and Rust **1.88.0** `check --workspace --locked` pass.
- Workspace: **533 passed**, 14 ignored by the normal suite; the subprocess self-test also reports its own pass. All **7** MCP protocol/rmcp 3.9.0 oracle tests pass.
- GNOME extension: **40 passed**, strict schema validation passes. Scoreboard unit tests: **21 passed**.
- Web: frozen install, lint, typecheck, **82 unit tests**, **14 PostgreSQL 16 tests**, unchanged migration generation, migrations applied twice, production build and asset budgets pass. Axe: **0 violations** on 42 routes × 2 themes × 2 widths. Overflow: **0** on 42 routes × 3 widths. UI checks pass; authenticated-page CLS is skipped without Clerk keys, as in CI.
- Linux private D-Bus bridge and scoreboard gate cannot run on this Mac (`dbus-daemon` absent; `/proc` measurements unavailable). The baseline isolation suite’s other two tests pass; its private-bus test cannot start. The existing Linux +5% CLI binary ratchet has **not** been reset using macOS data; the accepted SQLite growth needs a Linux artifact and an intentional gate update. No Linux CI pass is claimed.

The final upstream fetch still has `origin/main` at `20e2c6b`; the gateway-owned M0 SQLite/WAL-reset fix has not merged, so no rebase is needed. M1 does not bump rusqlite or migrate history. Historical scoreboard files keep their dated binaries and workloads; these M1 figures supersede their binary sizes for the current macOS build only.
