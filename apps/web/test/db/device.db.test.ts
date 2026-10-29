import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { testDbUrl, useTestDatabase } from "./setup";

// Review #3: two creators racing for the last slot must not both get one.
describe.skipIf(!testDbUrl)("device-code capacity against Postgres", () => {
  let db: typeof import("@/db").db;
  let sql: typeof import("drizzle-orm").sql;
  let MAX_LIVE_CODES: number;
  let POST: typeof import("@/app/api/v1/device/code/route").POST;
  beforeAll(async () => {
    useTestDatabase();
    ({ db } = await import("@/db"));
    ({ sql } = await import("drizzle-orm"));
    ({ MAX_LIVE_CODES } = await import("@/lib/server/device"));
    ({ POST } = await import("@/app/api/v1/device/code/route"));
  });
  afterAll(async () => {
    await db.execute(sql`DELETE FROM device_codes WHERE client_name IN ('cap-fill', 'cap-race')`);
  });

  test("at MAX_LIVE_CODES - 1, of two concurrent requests exactly one gets a code and the other 429", async () => {
    await db.execute(sql`DELETE FROM rate_limits WHERE key LIKE 'device-code:%'`);
    const { rows } = await db.execute<{ n: number }>(
      sql`SELECT count(*)::int AS n FROM device_codes WHERE expires_at > now() AND consumed_at IS NULL`,
    );
    const fill = MAX_LIVE_CODES - 1 - (rows[0]?.n ?? 0);
    await db.execute(sql`
      INSERT INTO device_codes (device_code_hash, user_code, client_name, expires_at)
      SELECT md5(random()::text || g), 'CAP-' || g || '-' || md5(random()::text), 'cap-fill', now() + interval '10 minutes'
        FROM generate_series(1, ${fill}) g`);

    const request = () =>
      POST(new Request("http://localhost/api/v1/device/code", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ client_name: "cap-race" }),
      }));
    const statuses = (await Promise.all([request(), request()])).map((r) => r.status).sort();
    expect(statuses).toEqual([200, 429]);
  });
});
