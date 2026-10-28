import "server-only";
import { sql } from "drizzle-orm";
import { after } from "next/server";
import { db } from "@/db";
import { error } from "@/lib/http";
import { isEntitled, type SubscriptionState } from "@/lib/entitlement";
import { bearerToken, sha256 } from "@/lib/tokens";

export type ApiCaller = { tokenId: string; userId: string; pro: boolean };

/** Resolves `Authorization: Bearer xf_...` to a live token and its Pro status, in one query. */
export async function apiCaller(request: Request): Promise<ApiCaller | null> {
  const token = bearerToken(request.headers.get("authorization"));
  if (!token) return null;
  const { rows } = await db.execute<{
    token_id: string;
    user_id: string;
    stale: boolean;
    subs: { status: string; current_period_end: string | null; past_due_ends_at: string | null }[] | null;
  }>(sql`
    SELECT t.id AS token_id, t.user_id,
           (t.last_used_at IS NULL OR t.last_used_at < now() - interval '5 minutes') AS stale,
           (SELECT json_agg(json_build_object('status', s.status,
                                              'current_period_end', s.current_period_end,
                                              'past_due_ends_at', s.past_due_ends_at))
              FROM subscriptions s WHERE s.user_id = t.user_id) AS subs
      FROM api_tokens t
     WHERE t.token_hash = ${sha256(token)} AND t.revoked_at IS NULL`);
  const row = rows[0];
  if (!row) return null;
  if (row.stale) {
    after(() => db.execute(sql`UPDATE api_tokens SET last_used_at = now() WHERE id = ${row.token_id}`));
  }
  const subs: SubscriptionState[] = (row.subs ?? []).map((s) => ({
    status: s.status,
    currentPeriodEnd: s.current_period_end ? new Date(s.current_period_end) : null,
    pastDueEndsAt: s.past_due_ends_at ? new Date(s.past_due_ends_at) : null,
  }));
  return { tokenId: row.token_id, userId: row.user_id, pro: isEntitled(subs, new Date()) };
}

export function unauthorized(): Response {
  return error(401, "missing, malformed or revoked xflow token; run `xflow login`");
}
