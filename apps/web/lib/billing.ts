import { Webhook, WebhookVerificationError } from "standardwebhooks";

export const USER_ID_METADATA_KEY = "xflow_user_id";

export type SubscriptionUpdate = {
  dodoSubscriptionId: string;
  dodoCustomerId: string;
  productId: string;
  status: string;
  /** Our users.id from checkout metadata, when present. */
  userId: string | null;
  currentPeriodEnd: Date | null;
  pastDueEndsAt: Date | null;
  cancelAtPeriodEnd: boolean;
  eventAt: Date;
};

/**
 * Verifies a Standard Webhooks signature (Dodo signs with it) and returns the parsed JSON.
 * Throws WebhookVerificationError on a bad signature, a stale timestamp or missing headers.
 */
export function verifyWebhook(secret: string, body: string, headers: Headers): unknown {
  const required = {
    "webhook-id": headers.get("webhook-id") ?? "",
    "webhook-timestamp": headers.get("webhook-timestamp") ?? "",
    "webhook-signature": headers.get("webhook-signature") ?? "",
  };
  return new Webhook(secret).verify(body, required);
}

export { WebhookVerificationError };

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function str(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 && value.length <= 256 ? value : null;
}

function date(value: unknown): Date | null {
  if (typeof value !== "string") return null;
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? null : d;
}

/** Extracts the fields we keep from a `subscription.*` event; null for every other event or a malformed one. */
export function subscriptionUpdate(event: unknown): SubscriptionUpdate | null {
  if (typeof event !== "object" || event === null) return null;
  const { type, timestamp, data } = event as Record<string, unknown>;
  if (typeof type !== "string" || !type.startsWith("subscription.")) return null;
  if (typeof data !== "object" || data === null) return null;
  const d = data as Record<string, unknown>;
  const customer = (typeof d.customer === "object" && d.customer !== null ? d.customer : {}) as Record<string, unknown>;
  const metadata = (typeof d.metadata === "object" && d.metadata !== null ? d.metadata : {}) as Record<string, unknown>;
  const dodoSubscriptionId = str(d.subscription_id);
  const dodoCustomerId = str(customer.customer_id);
  const productId = str(d.product_id);
  const status = str(d.status);
  const eventAt = date(timestamp);
  if (!dodoSubscriptionId || !dodoCustomerId || !productId || !status || !eventAt) return null;
  const userId = str(metadata[USER_ID_METADATA_KEY]);
  return {
    dodoSubscriptionId,
    dodoCustomerId,
    productId,
    status,
    userId: userId && UUID_RE.test(userId) ? userId : null,
    currentPeriodEnd: date(d.next_billing_date),
    pastDueEndsAt: date(d.past_due_ends_at),
    cancelAtPeriodEnd: d.cancel_at_next_billing_date === true,
    eventAt,
  };
}
