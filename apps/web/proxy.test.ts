import { expect, test } from "bun:test";
import { NextRequest } from "next/server";
import { sessionApi } from "./proxy";

// Review #7: only real session routes answer 401; any other /api/v1 path falls through to a 404.
test("session API matcher is exact", () => {
  const is = (path: string) => sessionApi(new NextRequest(`http://localhost${path}`));
  for (const p of ["/api/v1/billing/checkout", "/api/v1/billing/portal", "/api/v1/device/decide", "/api/v1/tokens/3f2a/revoke", "/api/v1/usage/live"]) {
    expect(is(p)).toBe(true);
  }
  for (const p of ["/api/v1/billing/nope", "/api/v1/usage/nope", "/api/v1/tokens/nope", "/api/v1/tokens/3f2a/revoke/x", "/api/v1/nope", "/api/v1/me"]) {
    expect(is(p)).toBe(false);
  }
});
