import "server-only";
import { createRealtime, pgParam, rawSql } from "@neon/realtime/server";
import { db } from "@/db";

export type UsageRow = { kind: "transcribe" | "cleanup"; requests: number; audio_ms: number; output_tokens: number };

// Per-kind totals since $2 for one user. Ints are cast so the live and initial rows are both numbers.
export const USAGE_SQL = `SELECT kind::text AS kind, count(*)::int AS requests, coalesce(sum(audio_ms), 0)::int AS audio_ms,
       coalesce(sum(output_tokens), 0)::int AS output_tokens
  FROM usage_events WHERE user_id = $1 AND created_at >= $2 AND status < 400 GROUP BY kind`;

export function usageQuery(userId: string, since: Date) {
  return rawSql<UsageRow>(USAGE_SQL, [pgParam.text("uuid", userId), pgParam.timestamptz(since)]);
}

/** The same rows, read once for the server render. */
export async function initialUsage(userId: string, since: Date): Promise<UsageRow[]> {
  return (await db.$client.query<UsageRow>(USAGE_SQL, [userId, since])).rows;
}

/** Start of the current UTC day; the dashboard's window. Must be stable across renewals. */
export function usageWindowStart(now = new Date()): Date {
  return new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()));
}

let realtime: ReturnType<typeof createRealtime> | null = null;

export async function sealUsage(userId: string, since: Date) {
  const secret = process.env.NEON_REALTIME_SECRET;
  const dbName = process.env.DATABASE_URL && new URL(process.env.DATABASE_URL).pathname.slice(1);
  if (!secret || !dbName) return null;
  realtime ??= createRealtime({ secret, db: dbName });
  return realtime.seal({ query: usageQuery(userId, since) });
}
