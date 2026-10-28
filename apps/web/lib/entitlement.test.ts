import { describe, expect, test } from "bun:test";
import { proProductIds } from "./billing";
import { isEntitled } from "./entitlement";

const now = new Date("2026-10-29T10:00:00Z");
const PRO = ["pdt_pro"];
const days = (n: number) => new Date(now.getTime() + n * 86_400_000);
const sub = (status: string, currentPeriodEnd: Date | null = days(20), pastDueEndsAt: Date | null = null, productId = "pdt_pro") => ({
  productId,
  status,
  currentPeriodEnd,
  pastDueEndsAt,
});

describe("Pro entitlement", () => {
  test("no subscription, no Pro", () => expect(isEntitled([], now, PRO)).toBe(false));
  test("active within the period", () => expect(isEntitled([sub("active")], now, PRO)).toBe(true));
  test("active stays on through a late renewal webhook", () => expect(isEntitled([sub("active", days(-2))], now, PRO)).toBe(true));
  test("active but long past the period end is off", () => expect(isEntitled([sub("active", days(-4))], now, PRO)).toBe(false));
  test("past_due keeps access until its grace deadline", () => {
    expect(isEntitled([sub("past_due", days(-1), days(2))], now, PRO)).toBe(true);
    expect(isEntitled([sub("past_due", days(-1), days(-0.1))], now, PRO)).toBe(false);
  });
  for (const status of ["pending", "on_hold", "paused", "cancelled", "failed", "expired", "something_new"]) {
    test(`${status} is not Pro`, () => expect(isEntitled([sub(status)], now, PRO)).toBe(false));
  }
  test("any one entitled subscription is enough", () => {
    expect(isEntitled([sub("expired"), sub("active")], now, PRO)).toBe(true);
  });

  // Review S1: any active product used to grant Pro.
  test("an active subscription to another product is not Pro", () => {
    expect(isEntitled([sub("active", days(20), null, "pdt_other")], now, PRO)).toBe(false);
  });
  test("with no Pro product configured, nothing grants Pro", () => {
    expect(isEntitled([sub("active")], now, [])).toBe(false);
  });
  test("several Pro product ids can be configured", () => {
    expect(proProductIds(" pdt_monthly, pdt_annual ,")).toEqual(["pdt_monthly", "pdt_annual"]);
    expect(proProductIds(undefined)).toEqual([]);
    expect(isEntitled([sub("active", days(20), null, "pdt_annual")], now, proProductIds("pdt_monthly,pdt_annual"))).toBe(true);
  });
});
