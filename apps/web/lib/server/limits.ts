import "server-only";
import { sql } from "drizzle-orm";
import { db } from "@/db";

/** Fixed-window counter shared by all instances. True while `key` stays within `limit` per window. */
export async function withinLimit(key: string, limit: number, windowSeconds: number): Promise<boolean> {
  const { rows } = await db.execute<{ count: number }>(sql`
    INSERT INTO rate_limits (key, window_start, count)
    VALUES (${key}, to_timestamp(floor(extract(epoch FROM now()) / ${windowSeconds}) * ${windowSeconds}), 1)
    ON CONFLICT (key) DO UPDATE SET
      count = CASE WHEN rate_limits.window_start = excluded.window_start THEN rate_limits.count + 1 ELSE 1 END,
      window_start = excluded.window_start
    RETURNING count`);
  return (rows[0]?.count ?? Infinity) <= limit;
}

export type Admission = "ok" | "concurrency" | "minute" | "day" | "budget_day" | "budget_month";

export type AdmitLimits = {
  perMinute: number;
  perDay: number;
  concurrent: number;
  leaseSeconds: number;
  dayBudgetMicros: number;
  monthBudgetMicros: number;
};

/**
 * One round trip (db function xflow_admit, migration 0001): takes a concurrency slot, counts the
 * request and reserves `costMicros` of the user's daily and monthly budget — all or nothing.
 */
export async function admit(kind: string, userId: string, costMicros: number, l: AdmitLimits): Promise<Admission> {
  const { rows } = await db.execute<{ result: Admission }>(sql`
    SELECT xflow_admit(${userId}, ${kind}, ${l.perMinute}::int, ${l.perDay}::int, ${l.concurrent}::int,
                       ${l.leaseSeconds}::int, ${costMicros}::int, ${l.dayBudgetMicros}::int,
                       ${l.monthBudgetMicros}::int) AS result`);
  return rows[0]?.result ?? "concurrency";
}

export async function releaseSlot(kind: string, userId: string): Promise<void> {
  await db.execute(sql`UPDATE rate_limits SET count = greatest(count - 1, 0) WHERE key = ${`${kind}:${userId}:inflight`}`);
}

/** Corrects a reservation by `deltaMicros` once the real cost is known. */
export async function settle(userId: string, deltaMicros: number): Promise<void> {
  if (deltaMicros !== 0) await db.execute(sql`SELECT xflow_settle(${userId}, ${deltaMicros}::int)`);
}

/** Per-IP device-flow counters are the only rows keyed by something unbounded. */
export async function pruneIpLimits(): Promise<void> {
  await db.execute(sql`DELETE FROM rate_limits WHERE key LIKE 'device-%' AND window_start < now() - interval '1 day'`);
}
