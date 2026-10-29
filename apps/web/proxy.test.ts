import { expect, test } from "bun:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { NextRequest } from "next/server";
import { config, SESSION_API, sessionApi } from "./proxy";

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

// research-hosted #1: the middleware must not run on the token-auth and public APIs (audio uploads
// would pay origin transfer twice), and must run on every route that calls Clerk.
test("middleware matcher covers exactly the Clerk-using API routes", () => {
  const api = config.matcher.filter((m) => m.startsWith("/api/"));
  expect(api).toEqual(SESSION_API);

  const root = join(import.meta.dir, "app/api/v1");
  const walk = (dir: string): string[] =>
    readdirSync(dir).flatMap((f) => (statSync(join(dir, f)).isDirectory() ? walk(join(dir, f)) : f === "route.ts" ? [join(dir, f)] : []));
  const clerkRoutes = walk(root)
    .filter((f) => /@clerk\/nextjs\/server|sessionUserId/.test(readFileSync(f, "utf8")))
    .map((f) => "/api/v1/" + f.slice(root.length + 1, -"/route.ts".length).replace(/\[(\w+)\]/g, ":$1"))
    .sort();
  expect(clerkRoutes).toEqual([...SESSION_API].sort());
});
