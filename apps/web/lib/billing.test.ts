import { describe, expect, test } from "bun:test";
import { randomBytes } from "node:crypto";
import { Webhook } from "standardwebhooks";
import fixture from "../test/fixtures/subscription.active.json";
import { subscriptionUpdate, verifyWebhook, WebhookVerificationError } from "./billing";

const secret = `whsec_${randomBytes(24).toString("base64")}`;
const body = JSON.stringify(fixture);

function signed(payload: string, at = new Date(), key = secret, id = "msg_test_1"): Headers {
  return new Headers({
    "webhook-id": id,
    "webhook-timestamp": String(Math.floor(at.getTime() / 1000)),
    "webhook-signature": new Webhook(key).sign(id, at, payload),
  });
}

describe("webhook signature verification", () => {
  test("accepts a correctly signed payload", () => {
    expect(verifyWebhook(secret, body, signed(body))).toEqual(fixture);
  });

  test("rejects a tampered body", () => {
    const headers = signed(body);
    const tampered = body.replace('"active"', '"expired"');
    expect(() => verifyWebhook(secret, tampered, headers)).toThrow(WebhookVerificationError);
  });

  test("rejects the wrong secret", () => {
    const other = `whsec_${randomBytes(24).toString("base64")}`;
    expect(() => verifyWebhook(secret, body, signed(body, new Date(), other))).toThrow(WebhookVerificationError);
  });

  test("rejects replays outside the 5-minute window", () => {
    const old = new Date(Date.now() - 10 * 60_000);
    expect(() => verifyWebhook(secret, body, signed(body, old))).toThrow(WebhookVerificationError);
  });

  test("rejects missing headers", () => {
    expect(() => verifyWebhook(secret, body, new Headers())).toThrow(WebhookVerificationError);
  });

  test("rejects a signature for a different message id", () => {
    const headers = signed(body);
    headers.set("webhook-id", "msg_other");
    expect(() => verifyWebhook(secret, body, headers)).toThrow(WebhookVerificationError);
  });
});

describe("subscription event parsing", () => {
  test("extracts the fields we store", () => {
    expect(subscriptionUpdate(fixture)).toEqual({
      dodoSubscriptionId: "sub_test_0001",
      dodoCustomerId: "cus_test_0001",
      productId: "pdt_test_pro",
      status: "active",
      userId: "00000000-0000-4000-8000-000000000001",
      currentPeriodEnd: new Date("2026-11-29T10:00:00Z"),
      pastDueEndsAt: null,
      cancelAtPeriodEnd: false,
      eventAt: new Date("2026-10-29T10:00:00Z"),
    });
  });

  test("ignores non-subscription events", () => {
    expect(subscriptionUpdate({ ...fixture, type: "payment.succeeded" })).toBeNull();
    expect(subscriptionUpdate(null)).toBeNull();
    expect(subscriptionUpdate("nope")).toBeNull();
  });

  test("rejects events missing ids", () => {
    expect(subscriptionUpdate({ ...fixture, data: { ...fixture.data, subscription_id: undefined } })).toBeNull();
    expect(subscriptionUpdate({ ...fixture, data: { ...fixture.data, customer: null } })).toBeNull();
    expect(subscriptionUpdate({ ...fixture, timestamp: "not a date" })).toBeNull();
  });

  test("drops metadata user ids that aren't UUIDs", () => {
    const evil = { ...fixture, data: { ...fixture.data, metadata: { xflow_user_id: "1 OR 1=1" } } };
    expect(subscriptionUpdate(evil)?.userId).toBeNull();
  });

  test("reads cancel-at-period-end and past-due deadlines", () => {
    const u = subscriptionUpdate({
      ...fixture,
      type: "subscription.past_due",
      data: { ...fixture.data, status: "past_due", cancel_at_next_billing_date: true, past_due_ends_at: "2026-11-02T00:00:00Z" },
    });
    expect(u?.status).toBe("past_due");
    expect(u?.cancelAtPeriodEnd).toBe(true);
    expect(u?.pastDueEndsAt).toEqual(new Date("2026-11-02T00:00:00Z"));
  });
});
