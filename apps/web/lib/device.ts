// Device authorization grant (RFC 8628) state machine, kept free of I/O so it is testable.

export const DEVICE_CODE_TTL_SECONDS = 600;
export const DEFAULT_POLL_INTERVAL_SECONDS = 5;
const SLOW_DOWN_STEP_SECONDS = 5;
const MAX_POLL_INTERVAL_SECONDS = 60;

export type DeviceRow = {
  status: "pending" | "approved" | "denied";
  expiresAt: Date;
  consumedAt: Date | null;
  intervalSeconds: number;
  /** The poll before this one, if any. */
  previousPollAt: Date | null;
};

export type PollOutcome =
  | { kind: "error"; error: "authorization_pending" | "slow_down" | "access_denied" | "expired_token" | "invalid_grant" }
  | { kind: "issue" };

export function pollOutcome(row: DeviceRow | null, now: Date): PollOutcome {
  if (!row || row.consumedAt) return { kind: "error", error: "invalid_grant" };
  if (row.expiresAt <= now) return { kind: "error", error: "expired_token" };
  if (row.status === "denied") return { kind: "error", error: "access_denied" };
  if (row.previousPollAt && now.getTime() - row.previousPollAt.getTime() < row.intervalSeconds * 1000) {
    return { kind: "error", error: "slow_down" };
  }
  if (row.status === "pending") return { kind: "error", error: "authorization_pending" };
  return { kind: "issue" };
}

/** The interval a client must use after this poll: RFC 8628 §3.5 adds 5 s on every slow_down. */
export function nextInterval(current: number, outcome: PollOutcome): number {
  return outcome.kind === "error" && outcome.error === "slow_down"
    ? Math.min(current + SLOW_DOWN_STEP_SECONDS, MAX_POLL_INTERVAL_SECONDS)
    : current;
}

/** Name the CLI sends for the token (usually the hostname). Printable, 1–64 chars. */
export function cleanClientName(input: unknown): string {
  if (typeof input !== "string") return "xflow CLI";
  const name = input.replace(/[\u0000-\u001f\u007f]/g, "").trim().slice(0, 64);
  return name || "xflow CLI";
}
