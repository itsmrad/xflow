import { blob, REPO, WISPR } from "./site";

/**
 * Claims ledger (DESIGN.md §11). Every number or competitor fact rendered on the site
 * comes from here, so copy and evidence can't drift apart. BLOCKED rows are not exported.
 */
export const CLAIMS = {
  1: { text: "11,116 KiB RSS after a 5 s warm-up, Linux release build, headless.", href: blob("PERFORMANCE.md#measured-v1-optimization-baseline--2026-10-01") },
  2: { text: "Daemon startup p50 2.601 ms, p95 2.969 ms over 30 launches, warm filesystem cache.", href: blob("PERFORMANCE.md#measured-v1-optimization-baseline--2026-10-01") },
  3: { text: "README: telemetry is absent; secrets, audio, transcripts and selected text are not logged.", href: blob("README.md") },
  4: { text: "README: ten cloud batch transcription adapters plus custom and local servers.", href: blob("README.md") },
  5: { text: "README: audio remains in memory and is never saved or logged by default.", href: blob("README.md") },
  6: { text: "README: privacy.offline = true rejects non-loopback endpoints, including cleanup. No model is bundled.", href: blob("README.md") },
  7: { text: "README: history is local plaintext SQLite; disable it with privacy.history = false.", href: blob("README.md") },
  8: { text: "README: secure fields refuse injection; multiline text is never auto-pasted into terminals.", href: blob("README.md") },
  9: { text: "MIT licence, public repository.", href: `${REPO}/blob/main/LICENSE` },
  11: { text: "README: silence and brief clicks end with “Didn't catch anything”.", href: blob("README.md") },
  13: { text: `Wispr Flow pricing page, checked ${WISPR.checked}.`, href: WISPR.url },
} as const;

export type ClaimId = keyof typeof CLAIMS;
