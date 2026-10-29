import { clerkMiddleware, createRouteMatcher } from "@clerk/nextjs/server";
import { NextResponse } from "next/server";

const signedInPages = createRouteMatcher(["/dashboard(.*)", "/device(.*)"]);
// Browser-session APIs: the only /api/v1 routes that call Clerk. The rest authenticate themselves
// (xflow token, webhook signature) or are public, and never run this middleware; unknown paths 404.
export const SESSION_API = [
  "/api/v1/billing/checkout",
  "/api/v1/billing/portal",
  "/api/v1/device/decide",
  "/api/v1/tokens/:id/revoke",
  "/api/v1/usage/live",
];
export const sessionApi = createRouteMatcher(SESSION_API);

export default clerkMiddleware(async (auth, request) => {
  if (signedInPages(request)) {
    await auth.protect();
    return;
  }
  if (sessionApi(request)) {
    const { userId } = await auth();
    if (!userId) {
      return NextResponse.json({ error: { message: "sign in first" } }, { status: 401, headers: { "cache-control": "no-store" } });
    }
  }
});

export const config = {
  // Only routes that use Clerk. Marketing and docs pages skip it: running Clerk there costs a
  // handshake redirect round trip (~0.6 s on the dev instance) before first paint. The token-auth and
  // public APIs skip it too: /api/v1/transcribe would otherwise run middleware (and pay Vercel's origin
  // transfer for the audio twice) on every upload. /__clerk is the Frontend API proxy a production
  // instance on *.vercel.app needs (clerk.<domain> can't get DNS there).
  // Next reads this statically, so SESSION_API is spelled out; proxy.test.ts keeps the two in sync.
  matcher: [
    "/dashboard/:path*",
    "/device/:path*",
    "/sign-in/:path*",
    "/sign-up/:path*",
    "/api/v1/billing/checkout",
    "/api/v1/billing/portal",
    "/api/v1/device/decide",
    "/api/v1/tokens/:id/revoke",
    "/api/v1/usage/live",
    "/__clerk/:path*",
  ],
};
