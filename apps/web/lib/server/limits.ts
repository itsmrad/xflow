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

/**
 * Takes one of `max` concurrent slots, in one statement (the row lock serialises racers).
 * A slot leaked by a killed function frees itself once no slot was taken for `leaseSeconds`.
 */
export async function acquireSlot(key: string, max: number, leaseSeconds: number): Promise<boolean> {
  const { rows } = await db.execute(sql`
    INSERT INTO rate_limits (key, window_start, count) VALUES (${key}, now(), 1)
    ON CONFLICT (key) DO UPDATE SET
      count = CASE WHEN rate_limits.window_start < now() - make_interval(secs => ${leaseSeconds})
                   THEN 1 ELSE rate_limits.count + 1 END,
      window_start = now()
    WHERE rate_limits.count < ${max} OR rate_limits.window_start < now() - make_interval(secs => ${leaseSeconds})
    RETURNING count`);
  return rows.length === 1;
}

export async function releaseSlot(key: string): Promise<void> {
  await db.execute(sql`UPDATE rate_limits SET count = greatest(count - 1, 0) WHERE key = ${key}`);
}
