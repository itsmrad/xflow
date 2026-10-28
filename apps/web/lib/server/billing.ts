import "server-only";
import DodoPayments from "dodopayments";
import { sql } from "drizzle-orm";
import { db } from "@/db";
import type { SubscriptionUpdate } from "@/lib/billing";

export function dodo(): DodoPayments | null {
  const key = process.env.DODO_PAYMENTS_API_KEY;
  if (!key) return null;
  return new DodoPayments({
    bearerToken: key,
    environment: process.env.DODO_PAYMENTS_ENVIRONMENT === "live_mode" ? "live_mode" : "test_mode",
  });
}

export type ApplyResult = "applied" | "stale" | "unknown_user";

/**
 * Upserts a subscription from a webhook. Deliveries can arrive late or twice, so a row only
 * moves forward in event time. The user comes from checkout metadata, else the Dodo customer.
 */
export async function applySubscriptionUpdate(u: SubscriptionUpdate): Promise<ApplyResult> {
  const { rows } = await db.execute<{ applied: boolean | null; user_id: string | null }>(sql`
    WITH owner AS (
      SELECT id FROM users
       WHERE (${u.userId}::uuid IS NOT NULL AND id = ${u.userId}::uuid)
          OR dodo_customer_id = ${u.dodoCustomerId}
       ORDER BY (id = ${u.userId}::uuid) DESC NULLS LAST
       LIMIT 1
    ), link AS (
      UPDATE users SET dodo_customer_id = ${u.dodoCustomerId}
       WHERE id = (SELECT id FROM owner) AND dodo_customer_id IS DISTINCT FROM ${u.dodoCustomerId}
    ), up AS (
      INSERT INTO subscriptions (dodo_subscription_id, user_id, dodo_customer_id, product_id, status,
                                 current_period_end, past_due_ends_at, cancel_at_period_end, last_event_at)
      SELECT ${u.dodoSubscriptionId}, id, ${u.dodoCustomerId}, ${u.productId}, ${u.status},
             ${u.currentPeriodEnd?.toISOString() ?? null}::timestamptz, ${u.pastDueEndsAt?.toISOString() ?? null}::timestamptz,
             ${u.cancelAtPeriodEnd}, ${u.eventAt.toISOString()}::timestamptz
        FROM owner
      ON CONFLICT (dodo_subscription_id) DO UPDATE SET
        status = excluded.status, product_id = excluded.product_id,
        current_period_end = excluded.current_period_end, past_due_ends_at = excluded.past_due_ends_at,
        cancel_at_period_end = excluded.cancel_at_period_end, last_event_at = excluded.last_event_at,
        updated_at = now()
      WHERE subscriptions.last_event_at <= excluded.last_event_at
      RETURNING 1
    )
    SELECT (SELECT count(*) > 0 FROM up) AS applied, (SELECT id FROM owner) AS user_id`);
  const row = rows[0];
  if (!row?.user_id) return "unknown_user";
  return row.applied ? "applied" : "stale";
}
