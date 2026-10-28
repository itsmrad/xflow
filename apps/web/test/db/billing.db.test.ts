import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import type { SubscriptionUpdate } from "@/lib/billing";
import { testDbUrl, useTestDatabase } from "./setup";

// Review M1: a webhook for a Dodo customer already linked to another account returned 500 forever.
describe.skipIf(!testDbUrl)("subscription webhooks against Postgres", () => {
  let db: typeof import("@/db").db;
  let apply: typeof import("@/lib/server/billing").applySubscriptionUpdate;
  let sql: typeof import("drizzle-orm").sql;
  const tag = `t${Date.now()}`;

  beforeAll(async () => {
    useTestDatabase();
    ({ db } = await import("@/db"));
    ({ applySubscriptionUpdate: apply } = await import("@/lib/server/billing"));
    ({ sql } = await import("drizzle-orm"));
  });
  afterAll(async () => {
    await db.execute(sql`DELETE FROM users WHERE clerk_user_id LIKE ${`${tag}%`}`);
  });

  const user = async (name: string, customer: string | null = null, email: string | null = null) => {
    const { rows } = await db.execute<{ id: string }>(
      sql`INSERT INTO users (clerk_user_id, dodo_customer_id, email) VALUES (${`${tag}-${name}`}, ${customer}, ${email}) RETURNING id`,
    );
    return rows[0]!.id;
  };
  const update = (over: Partial<SubscriptionUpdate>): SubscriptionUpdate => ({
    dodoSubscriptionId: `sub_${tag}_${Math.random().toString(36).slice(2)}`,
    dodoCustomerId: `cus_${tag}`,
    customerEmail: null,
    productId: "pdt_pro",
    status: "active",
    userId: null,
    currentPeriodEnd: new Date(Date.now() + 30 * 86_400_000),
    pastDueEndsAt: null,
    cancelAtPeriodEnd: false,
    eventAt: new Date(),
    ...over,
  });
  const subsOf = async (userId: string) =>
    (await db.execute<{ status: string }>(sql`SELECT status FROM subscriptions WHERE user_id = ${userId}`)).rows;
  const customerOf = async (userId: string) =>
    (await db.execute<{ c: string | null }>(sql`SELECT dodo_customer_id AS c FROM users WHERE id = ${userId}`)).rows[0]!.c;

  test("customer already linked to another account: the paying account gets the subscription", async () => {
    const customer = `cus_${tag}_m1`;
    const oldAccount = await user("old", customer);
    const payer = await user("payer");
    const u = update({ dodoCustomerId: customer, userId: payer });
    expect(await apply(u)).toBe("applied");
    expect(await subsOf(payer)).toEqual([{ status: "active" }]);
    expect(await customerOf(payer)).toBe(customer);
    expect(await customerOf(oldAccount)).toBeNull();
    // Redelivery is idempotent: same state, one row, no error.
    expect(await apply(u)).toBe("applied");
    expect(await subsOf(payer)).toHaveLength(1);
  });

  test("concurrent deliveries for one customer all succeed", async () => {
    const customer = `cus_${tag}_race`;
    await user("race-old", customer);
    const payer = await user("race-payer");
    const results = await Promise.all(
      Array.from({ length: 6 }, (_, i) =>
        apply(update({ dodoCustomerId: customer, userId: payer, dodoSubscriptionId: `sub_${tag}_race`, eventAt: new Date(Date.now() + i) })),
      ),
    );
    expect(results.every((r) => r === "applied" || r === "stale")).toBe(true);
    expect(await subsOf(payer)).toHaveLength(1);
  });

  test("an older event never overwrites a newer one", async () => {
    const payer = await user("order");
    const id = `sub_${tag}_order`;
    expect(await apply(update({ userId: payer, dodoSubscriptionId: id, dodoCustomerId: `cus_${tag}_o` }))).toBe("applied");
    const old = update({ userId: payer, dodoSubscriptionId: id, dodoCustomerId: `cus_${tag}_o`, status: "expired", eventAt: new Date(Date.now() - 3_600_000) });
    expect(await apply(old)).toBe("stale");
    expect(await subsOf(payer)).toEqual([{ status: "active" }]);
  });

  test("without metadata the owner is found by customer, then by email", async () => {
    const byCustomer = await user("by-customer", `cus_${tag}_bc`);
    expect(await apply(update({ dodoCustomerId: `cus_${tag}_bc` }))).toBe("applied");
    expect(await subsOf(byCustomer)).toHaveLength(1);

    const byEmail = await user("by-email", null, `Pay.${tag}@Example.com`);
    expect(await apply(update({ dodoCustomerId: `cus_${tag}_be`, customerEmail: `pay.${tag}@example.com` }))).toBe("applied");
    expect(await subsOf(byEmail)).toHaveLength(1);
    expect(await customerOf(byEmail)).toBe(`cus_${tag}_be`);
  });

  test("a later event for a known subscription keeps its owner even without metadata", async () => {
    const payer = await user("keeps");
    const id = `sub_${tag}_keeps`;
    await apply(update({ userId: payer, dodoSubscriptionId: id, dodoCustomerId: `cus_${tag}_k` }));
    expect(await apply(update({ dodoSubscriptionId: id, dodoCustomerId: `cus_${tag}_k`, status: "past_due" }))).toBe("applied");
    expect(await subsOf(payer)).toEqual([{ status: "past_due" }]);
  });

  test("no way to find the owner is reported, not thrown", async () => {
    expect(await apply(update({ dodoCustomerId: `cus_${tag}_nobody` }))).toBe("unknown_user");
  });
});
