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

export async function releaseSlot(key: string): Promise<void> {
  await db.execute(sql`UPDATE rate_limits SET count = greatest(count - 1, 0) WHERE key = ${key}`);
}

export type Admission = { minuteOk: boolean; dayOk: boolean; slot: boolean };

/**
 * The proxy's three checks in one round trip: per-minute and per-day fixed windows, plus one
 * of `concurrent` slots. The row lock serialises racers; a slot leaked by a killed function
 * frees itself once no slot was taken for `leaseSeconds`.
 */
export async function admit(
  prefix: string,
  limits: { perMinute: number; perDay: number; concurrent: number },
  leaseSeconds: number,
): Promise<Admission> {
  const { rows } = await db.execute<{ minute: number; day: number; slot: number | null }>(sql`
    WITH minute AS (
      INSERT INTO rate_limits (key, window_start, count)
      VALUES (${`${prefix}:min`}, to_timestamp(floor(extract(epoch FROM now()) / 60) * 60), 1)
      ON CONFLICT (key) DO UPDATE SET
        count = CASE WHEN rate_limits.window_start = excluded.window_start THEN rate_limits.count + 1 ELSE 1 END,
        window_start = excluded.window_start
      RETURNING count
    ), day AS (
      INSERT INTO rate_limits (key, window_start, count)
      VALUES (${`${prefix}:day`}, to_timestamp(floor(extract(epoch FROM now()) / 86400) * 86400), 1)
      ON CONFLICT (key) DO UPDATE SET
        count = CASE WHEN rate_limits.window_start = excluded.window_start THEN rate_limits.count + 1 ELSE 1 END,
        window_start = excluded.window_start
      RETURNING count
    ), slot AS (
      INSERT INTO rate_limits (key, window_start, count) VALUES (${`${prefix}:inflight`}, now(), 1)
      ON CONFLICT (key) DO UPDATE SET
        count = CASE WHEN rate_limits.window_start < now() - make_interval(secs => ${leaseSeconds})
                     THEN 1 ELSE rate_limits.count + 1 END,
        window_start = now()
      WHERE rate_limits.count < ${limits.concurrent}
         OR rate_limits.window_start < now() - make_interval(secs => ${leaseSeconds})
      RETURNING count
    )
    SELECT (SELECT count FROM minute) AS minute, (SELECT count FROM day) AS day, (SELECT count FROM slot) AS slot`);
  const row = rows[0];
  return {
    minuteOk: !!row && row.minute <= limits.perMinute,
    dayOk: !!row && row.day <= limits.perDay,
    slot: row?.slot != null,
  };
}
