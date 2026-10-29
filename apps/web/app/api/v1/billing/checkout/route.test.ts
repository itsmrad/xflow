import { expect, mock, test } from "bun:test";

// The route must refuse before it reaches Clerk or the database.
const untouched = () => {
  throw new Error("checkout touched auth or the database before refusing");
};
mock.module("server-only", () => ({}));
mock.module("@clerk/nextjs/server", () => ({ auth: untouched, currentUser: untouched }));
mock.module("@/db", () => ({ db: new Proxy({}, { get: untouched }) }));

test("checkout answers 503 'Pro is coming soon' until PRO_LAUNCHED=1, even with Dodo keys set", async () => {
  delete process.env.PRO_LAUNCHED;
  process.env.DODO_PAYMENTS_API_KEY = "test-key";
  const { POST } = await import("./route");
  const res = await POST(new Request("http://localhost/api/v1/billing/checkout", { method: "POST" }));
  expect(res.status).toBe(503);
  expect(await res.json()).toEqual({ error: { message: "Pro is coming soon" } });
});
