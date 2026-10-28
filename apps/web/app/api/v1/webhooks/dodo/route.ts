import { subscriptionUpdate, verifyWebhook, WebhookVerificationError } from "@/lib/billing";
import { error, json } from "@/lib/http";
import { applySubscriptionUpdate } from "@/lib/server/billing";

/** Dodo Payments webhooks (Standard Webhooks signatures). Only subscription.* events change state. */
export async function POST(request: Request) {
  const secret = process.env.DODO_PAYMENTS_WEBHOOK_SECRET;
  if (!secret) return error(503, "webhooks are not configured");
  const body = await request.text();
  if (body.length > 256 * 1024) return error(413, "payload too large");

  let event: unknown;
  try {
    event = verifyWebhook(secret, body, request.headers);
  } catch (e) {
    if (e instanceof WebhookVerificationError) return error(401, "invalid signature");
    throw e;
  }

  const update = subscriptionUpdate(event);
  if (!update) return json({ ok: true, ignored: true });
  const result = await applySubscriptionUpdate(update);
  if (result === "unknown_user") {
    // Not retryable: Dodo would redeliver forever. Logged with ids only, no customer data.
    console.warn(`dodo webhook for unknown user: subscription ${update.dodoSubscriptionId}`);
  }
  return json({ ok: true, result });
}
