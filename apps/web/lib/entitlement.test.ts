import { describe, expect, test } from "bun:test";
import { isEntitled } from "./entitlement";

const now = new Date("2026-10-29T10:00:00Z");
const days = (n: number) => new Date(now.getTime() + n * 86_400_000);
const sub = (status: string, currentPeriodEnd: Date | null = days(20), pastDueEndsAt: Date | null = null) => ({
  status,
  currentPeriodEnd,
  pastDueEndsAt,
});

describe("Pro entitlement", () => {
  test("no subscription, no Pro", () => expect(isEntitled([], now)).toBe(false));
  test("active within the period", () => expect(isEntitled([sub("active")], now)).toBe(true));
  test("active stays on through a late renewal webhook", () => expect(isEntitled([sub("active", days(-2))], now)).toBe(true));
  test("active but long past the period end is off", () => expect(isEntitled([sub("active", days(-4))], now)).toBe(false));
  test("past_due keeps access until its grace deadline", () => {
    expect(isEntitled([sub("past_due", days(-1), days(2))], now)).toBe(true);
    expect(isEntitled([sub("past_due", days(-1), days(-0.1))], now)).toBe(false);
  });
  for (const status of ["pending", "on_hold", "paused", "cancelled", "failed", "expired", "something_new"]) {
    test(`${status} is not Pro`, () => expect(isEntitled([sub(status)], now)).toBe(false));
  }
  test("any one entitled subscription is enough", () => {
    expect(isEntitled([sub("expired"), sub("active")], now)).toBe(true);
  });
});
