import { clerkMiddleware, createRouteMatcher } from "@clerk/nextjs/server";
import { NextResponse } from "next/server";

const signedInPages = createRouteMatcher(["/dashboard(.*)", "/device(.*)"]);
// Browser-session APIs. The rest of /api/v1 authenticates itself (xflow token, webhook signature) or
// is public, and an unknown path falls through to a 404.
const sessionApi = createRouteMatcher([
  "/api/v1/billing/(.*)",
  "/api/v1/device/decide",
  "/api/v1/tokens/(.*)",
  "/api/v1/usage/(.*)",
]);

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
  // handshake redirect round trip (~0.6 s on the dev instance) before first paint. /__clerk is the
  // Frontend API proxy a production instance on *.vercel.app needs (clerk.<domain> can't get DNS there).
  matcher: ["/dashboard/:path*", "/device/:path*", "/sign-in/:path*", "/sign-up/:path*", "/api/v1/:path*", "/__clerk/:path*"],
};
