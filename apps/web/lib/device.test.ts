import { describe, expect, test } from "bun:test";
import { cleanClientName, nextInterval, pollOutcome, type DeviceRow } from "./device";

const now = new Date("2026-10-29T10:00:00Z");
const row = (over: Partial<DeviceRow> = {}): DeviceRow => ({
  status: "pending",
  expiresAt: new Date(now.getTime() + 600_000),
  consumedAt: null,
  intervalSeconds: 5,
  previousPollAt: null,
  ...over,
});

describe("device flow polling", () => {
  test("unknown or consumed codes are invalid_grant", () => {
    expect(pollOutcome(null, now)).toEqual({ kind: "error", error: "invalid_grant" });
    expect(pollOutcome(row({ status: "approved", consumedAt: now }), now)).toEqual({ kind: "error", error: "invalid_grant" });
  });

  test("pending until the user decides", () => {
    expect(pollOutcome(row(), now)).toEqual({ kind: "error", error: "authorization_pending" });
  });

  test("approved codes issue a token", () => {
    expect(pollOutcome(row({ status: "approved" }), now)).toEqual({ kind: "issue" });
  });

  test("denied codes are access_denied", () => {
    expect(pollOutcome(row({ status: "denied" }), now)).toEqual({ kind: "error", error: "access_denied" });
  });

  test("codes expire after their TTL, even if approved", () => {
    expect(pollOutcome(row({ status: "approved", expiresAt: now }), now)).toEqual({ kind: "error", error: "expired_token" });
  });

  test("polling faster than the interval is slow_down and grows the interval", () => {
    const fast = pollOutcome(row({ previousPollAt: new Date(now.getTime() - 2000) }), now);
    expect(fast).toEqual({ kind: "error", error: "slow_down" });
    expect(nextInterval(5, fast)).toBe(10);
    expect(nextInterval(58, fast)).toBe(60);
    // Fast polling never short-circuits to a token.
    expect(pollOutcome(row({ status: "approved", previousPollAt: new Date(now.getTime() - 1000) }), now).kind).toBe("error");
  });

  test("polling at the interval is fine", () => {
    const ok = pollOutcome(row({ previousPollAt: new Date(now.getTime() - 5000) }), now);
    expect(ok).toEqual({ kind: "error", error: "authorization_pending" });
    expect(nextInterval(5, ok)).toBe(5);
  });

  test("client names are printable and bounded", () => {
    expect(cleanClientName("mbp.local")).toBe("mbp.local");
    expect(cleanClientName("evil\u0007\nname")).toBe("evilname");
    expect(cleanClientName("x".repeat(100))).toHaveLength(64);
    expect(cleanClientName("   ")).toBe("xflow CLI");
    expect(cleanClientName(42)).toBe("xflow CLI");
  });
});
