import { beforeAll, describe, expect, test } from "bun:test";
import { sql } from "drizzle-orm";

// CI's disposable Postgres only; no app URLs or environment files are read here.
describe.skipIf(!process.env.TEST_DATABASE_URL)("atomic hosted chat caps", () => {
  let db: typeof import("@/db").db;
  beforeAll(async () => {
    process.env.DATABASE_URL = process.env.TEST_DATABASE_URL;
    db = (await import("@/db")).db;
  });
  const fresh = () => `chat-test-${crypto.randomUUID()}`;
  async function admit(user: string, cost: number, day = 1000, month = 2000, aiDay = 250, aiMonth = 1500) {
    const { rows } = await db.execute<{result:string}>(sql`SELECT xflow_admit_chat(${user},${cost}::int,${day}::int,${month}::int,${aiDay}::int,${aiMonth}::int) result`);
    return rows[0]!.result;
  }
  const release = (u: string) => db.execute(sql`UPDATE rate_limits SET count=0 WHERE key=${`chat:${u}:inflight`}`);
  test("one concurrent chat across tokens and instances", async () => {
    const u = fresh();
    const results = await Promise.all(Array.from({length:8}, () => admit(u, 50)));
    expect(results.filter(r => r === "ok")).toHaveLength(1);
  });
  test("AI daily and monthly caps are inside the shared budget", async () => {
    const u = fresh();
    expect(await admit(u, 200)).toBe("ok"); await release(u);
    expect(await admit(u, 51)).toBe("ai_budget_day");
    expect(await admit(u, 50, 1000, 2000, 1000, 249)).toBe("ai_budget_month");
    expect(await admit(u, 50, 249)).toBe("budget_day");
    expect(await admit(u, 50, 1000, 249)).toBe("budget_month");
    expect(await admit(u, 50)).toBe("ok");
  });
  test("shared dictation spend blocks chat without consuming AI allowance", async () => {
    const u = fresh();
    await db.execute(sql`SELECT xflow_admit(${u},'cleanup',100,100,2,90,900,1000,2000)`);
    expect(await admit(u, 200)).toBe("budget_day");
    const {rows} = await db.execute<{count:number}>(sql`SELECT count FROM rate_limits WHERE key=${`ai:${u}:day`}`);
    expect(rows[0]!.count).toBe(0);
  });
  test("UTC rollover resets daily sub-cap without resetting monthly", async () => {
    const u = fresh(); expect(await admit(u, 200)).toBe("ok"); await release(u);
    await db.execute(sql`UPDATE rate_limits SET window_start=window_start - interval '1 day' WHERE key=${`ai:${u}:day`}`);
    expect(await admit(u, 200, 1000, 2000, 250, 399)).toBe("ai_budget_month");
    expect(await admit(u, 200)).toBe("ok");
  });
});
