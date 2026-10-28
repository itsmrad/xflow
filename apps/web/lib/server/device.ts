import "server-only";
import { sql } from "drizzle-orm";
import { db } from "@/db";
import { DEFAULT_POLL_INTERVAL_SECONDS, DEVICE_CODE_TTL_SECONDS, nextInterval, pollOutcome, type PollOutcome } from "@/lib/device";
import { newApiToken, newDeviceCode, newUserCode } from "@/lib/tokens";

export async function createDeviceCode(clientName: string) {
  // Old rows only matter until they expire; drop them so user codes can be reused.
  await db.execute(sql`DELETE FROM device_codes WHERE expires_at < now() - interval '1 day'`);
  for (let attempt = 0; ; attempt++) {
    const { deviceCode, hash } = newDeviceCode();
    const userCode = newUserCode();
    const { rows } = await db.execute(sql`
      INSERT INTO device_codes (device_code_hash, user_code, client_name, interval_seconds, expires_at)
      VALUES (${hash}, ${userCode}, ${clientName}, ${DEFAULT_POLL_INTERVAL_SECONDS},
              now() + make_interval(secs => ${DEVICE_CODE_TTL_SECONDS}))
      ON CONFLICT (user_code) DO NOTHING
      RETURNING id`);
    if (rows.length === 1) return { deviceCode, userCode };
    if (attempt >= 3) throw new Error("could not allocate a user code");
  }
}

export type PendingDevice = { userCode: string; clientName: string; createdAt: Date };

/** A code the signed-in user may still approve, or null. */
export async function pendingDevice(userCode: string): Promise<PendingDevice | null> {
  const { rows } = await db.execute<{ user_code: string; client_name: string; created_at: string }>(sql`
    SELECT user_code, client_name, created_at FROM device_codes
     WHERE user_code = ${userCode} AND status = 'pending' AND expires_at > now() AND consumed_at IS NULL`);
  const row = rows[0];
  return row ? { userCode: row.user_code, clientName: row.client_name, createdAt: new Date(row.created_at) } : null;
}

/** Approves or denies a pending code. False if it no longer exists, expired or was already decided. */
export async function decideDevice(userCode: string, userId: string, approve: boolean): Promise<boolean> {
  const { rows } = await db.execute(sql`
    UPDATE device_codes SET status = ${approve ? "approved" : "denied"}, user_id = ${userId}
     WHERE user_code = ${userCode} AND status = 'pending' AND expires_at > now() AND consumed_at IS NULL
    RETURNING id`);
  return rows.length === 1;
}

export type PollResult = { outcome: PollOutcome; interval: number; token?: string };

/** One CLI poll: records the poll time, applies slow_down, and on approval issues the token exactly once. */
export async function pollDevice(deviceCodeHash: string): Promise<PollResult> {
  const { rows } = await db.execute<{
    id: string;
    status: "pending" | "approved" | "denied";
    expires_at: string;
    consumed_at: string | null;
    interval_seconds: number;
    previous_poll_at: string | null;
    now: string;
  }>(sql`
    WITH prev AS (
      SELECT id, last_polled_at FROM device_codes WHERE device_code_hash = ${deviceCodeHash} FOR UPDATE
    )
    UPDATE device_codes d SET last_polled_at = now() FROM prev WHERE d.id = prev.id
    RETURNING d.id, d.status, d.expires_at, d.consumed_at, d.interval_seconds,
              prev.last_polled_at AS previous_poll_at, now() AS now`);
  const row = rows[0];
  const now = row ? new Date(row.now) : new Date();
  const outcome = pollOutcome(
    row ? {
      status: row.status,
      expiresAt: new Date(row.expires_at),
      consumedAt: row.consumed_at ? new Date(row.consumed_at) : null,
      intervalSeconds: row.interval_seconds,
      previousPollAt: row.previous_poll_at ? new Date(row.previous_poll_at) : null,
    } : null,
    now,
  );
  if (!row) return { outcome, interval: DEFAULT_POLL_INTERVAL_SECONDS };
  const interval = nextInterval(row.interval_seconds, outcome);
  if (interval !== row.interval_seconds) {
    await db.execute(sql`UPDATE device_codes SET interval_seconds = ${interval} WHERE id = ${row.id}`);
  }
  if (outcome.kind !== "issue") return { outcome, interval };

  // Consume the code and mint the token in one statement, so two racing polls can't both get one.
  const { token, hash, prefix } = newApiToken();
  const issued = await db.execute(sql`
    WITH used AS (
      UPDATE device_codes SET consumed_at = now()
       WHERE id = ${row.id} AND status = 'approved' AND consumed_at IS NULL AND expires_at > now()
      RETURNING user_id, client_name
    )
    INSERT INTO api_tokens (user_id, name, token_hash, prefix)
    SELECT user_id, client_name, ${hash}, ${prefix} FROM used
    RETURNING id`);
  if (issued.rows.length !== 1) return { outcome: { kind: "error", error: "invalid_grant" }, interval };
  return { outcome, interval, token };
}
