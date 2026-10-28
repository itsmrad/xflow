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
 * Upserts a subscription from a webhook so a paying customer always ends up with Pro:
 * - The owner is the checkout's user (metadata), else the subscription's existing owner, else whoever
 *   holds the Dodo customer, else the newest account with the customer's email.
 * - The Dodo customer moves to the owner. A stale link on another account (deleted and re-created
 *   account, same email) is cleared first, so the unique index can't fail the delivery.
 * - Idempotent and ordered: a row only moves forward in event time; a replay rewrites the same state.
 * One transaction per delivery, serialised per customer.
 */
export async function applySubscriptionUpdate(u: SubscriptionUpdate): Promise<ApplyResult> {
  return db.transaction(async (tx) => {
    await tx.execute(sql`SELECT pg_advisory_xact_lock(hashtext(${`dodo:${u.dodoCustomerId}`}))`);
    const { rows } = await tx.execute<{ id: string }>(sql`
      SELECT id FROM (
        SELECT id, 1 AS rank FROM users WHERE id = ${u.userId}::uuid
        UNION ALL
        SELECT user_id, 2 FROM subscriptions WHERE dodo_subscription_id = ${u.dodoSubscriptionId}
        UNION ALL
        SELECT id, 3 FROM users WHERE dodo_customer_id = ${u.dodoCustomerId}
        UNION ALL
        (SELECT id, 4 FROM users WHERE ${u.customerEmail}::text IS NOT NULL AND lower(email) = lower(${u.customerEmail})
          ORDER BY created_at DESC LIMIT 1)
      ) owners ORDER BY rank LIMIT 1`);
    const owner = rows[0]?.id;
    if (!owner) return "unknown_user";

    await tx.execute(sql`UPDATE users SET dodo_customer_id = NULL WHERE dodo_customer_id = ${u.dodoCustomerId} AND id <> ${owner}`);
    await tx.execute(sql`
      UPDATE users SET dodo_customer_id = ${u.dodoCustomerId}
       WHERE id = ${owner} AND dodo_customer_id IS DISTINCT FROM ${u.dodoCustomerId}`);
    const upsert = await tx.execute(sql`
      INSERT INTO subscriptions (dodo_subscription_id, user_id, dodo_customer_id, product_id, status,
                                 current_period_end, past_due_ends_at, cancel_at_period_end, last_event_at)
      VALUES (${u.dodoSubscriptionId}, ${owner}, ${u.dodoCustomerId}, ${u.productId}, ${u.status},
              ${u.currentPeriodEnd?.toISOString() ?? null}::timestamptz, ${u.pastDueEndsAt?.toISOString() ?? null}::timestamptz,
              ${u.cancelAtPeriodEnd}, ${u.eventAt.toISOString()}::timestamptz)
      ON CONFLICT (dodo_subscription_id) DO UPDATE SET
        user_id = excluded.user_id, dodo_customer_id = excluded.dodo_customer_id,
        status = excluded.status, product_id = excluded.product_id,
        current_period_end = excluded.current_period_end, past_due_ends_at = excluded.past_due_ends_at,
        cancel_at_period_end = excluded.cancel_at_period_end, last_event_at = excluded.last_event_at,
        updated_at = now()
      WHERE subscriptions.last_event_at <= excluded.last_event_at
      RETURNING 1`);
    return upsert.rows.length === 1 ? "applied" : "stale";
  });
}
