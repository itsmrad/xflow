import { beforeAll, describe, expect, test } from "bun:test";
import { testDbUrl, useTestDatabase } from "./setup";

// Review M2 / S2 / S3: admission must bound spend, only count admitted requests, and expire leaks.
describe.skipIf(!testDbUrl)("proxy admission against Postgres", () => {
  let limits: typeof import("@/lib/server/limits");
  beforeAll(async () => {
    useTestDatabase();
    limits = await import("@/lib/server/limits");
  });

  const base = { perMinute: 100, perDay: 1000, concurrent: 2, leaseSeconds: 90, dayBudgetMicros: 1_000_000, monthBudgetMicros: 10_000_000 };
  const fresh = () => `test-${crypto.randomUUID()}`;

  test("concurrency is capped and released", async () => {
    const u = fresh();
    expect(await limits.admit("cleanup", u, 1, base)).toBe("ok");
    expect(await limits.admit("cleanup", u, 1, base)).toBe("ok");
    expect(await limits.admit("cleanup", u, 1, base)).toBe("concurrency");
    await limits.releaseSlot("cleanup", u);
    expect(await limits.admit("cleanup", u, 1, base)).toBe("ok");
  });

  test("parallel admits never exceed the cap", async () => {
    const u = fresh();
    const results = await Promise.all(Array.from({ length: 8 }, () => limits.admit("transcribe", u, 1, base)));
    expect(results.filter((r) => r === "ok")).toHaveLength(2);
  });

  test("refused requests don't burn the request windows (S3)", async () => {
    const u = fresh();
    const l = { ...base, perMinute: 2, concurrent: 1 };
    expect(await limits.admit("cleanup", u, 1, l)).toBe("ok");
    for (let i = 0; i < 5; i++) expect(await limits.admit("cleanup", u, 1, l)).toBe("concurrency");
    await limits.releaseSlot("cleanup", u);
    expect(await limits.admit("cleanup", u, 1, l)).toBe("ok");
    await limits.releaseSlot("cleanup", u);
    expect(await limits.admit("cleanup", u, 1, l)).toBe("minute");
  });

  test("the daily budget bounds spend and is shared across kinds (M2)", async () => {
    const u = fresh();
    const l = { ...base, dayBudgetMicros: 1000 };
    expect(await limits.admit("transcribe", u, 600, l)).toBe("ok");
    expect(await limits.admit("cleanup", u, 600, l)).toBe("budget_day");
    // Settling a reservation down frees budget; refused requests reserved nothing.
    await limits.settle(u, -500);
    expect(await limits.admit("cleanup", u, 600, l)).toBe("ok");
    expect(await limits.admit("cleanup", u, 301, l)).toBe("budget_day");
  });

  test("the monthly budget bounds spend across days (M2)", async () => {
    const u = fresh();
    const l = { ...base, dayBudgetMicros: 5000, monthBudgetMicros: 1000 };
    expect(await limits.admit("transcribe", u, 900, l)).toBe("ok");
    expect(await limits.admit("transcribe", u, 200, l)).toBe("budget_month");
  });

  test("settling never drives spend below zero", async () => {
    const u = fresh();
    const l = { ...base, dayBudgetMicros: 100 };
    expect(await limits.admit("cleanup", u, 50, l)).toBe("ok");
    await limits.settle(u, -10_000);
    expect(await limits.admit("cleanup", u, 100, l)).toBe("ok");
  });

  test("a leaked slot expires even while the user stays active (S2)", async () => {
    const u = fresh();
    const l = { ...base, leaseSeconds: 2 };
    expect(await limits.admit("transcribe", u, 1, l)).toBe("ok"); // leaked: never released
    const until = Date.now() + 2600;
    while (Date.now() < until) {
      expect(await limits.admit("transcribe", u, 1, l)).toBe("ok");
      await limits.releaseSlot("transcribe", u);
      await Bun.sleep(300);
    }
    const both = await Promise.all([limits.admit("transcribe", u, 1, l), limits.admit("transcribe", u, 1, l)]);
    expect(both).toEqual(["ok", "ok"]);
  });
});
