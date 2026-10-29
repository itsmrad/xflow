import { clerkMiddleware, createRouteMatcher } from "@clerk/nextjs/server";
import { NextResponse } from "next/server";

const signedInPages = createRouteMatcher(["/dashboard(.*)", "/device(.*)"]);
// CLI and provider endpoints authenticate themselves (xflow token, webhook signature) or are public.
const selfAuthApi = createRouteMatcher([
  "/api/v1/webhooks/(.*)",
  "/api/v1/device/code",
  "/api/v1/device/token",
  "/api/v1/transcribe",
  "/api/v1/cleanup",
  "/api/v1/me",
  "/api/v1/releases/(.*)",
]);
const sessionApi = createRouteMatcher(["/api/v1/(.*)"]);

export default clerkMiddleware(async (auth, request) => {
  if (signedInPages(request)) {
    await auth.protect();
    return;
  }
  if (sessionApi(request) && !selfAuthApi(request)) {
    const { userId } = await auth();
    if (!userId) {
      return NextResponse.json({ error: { message: "sign in first" } }, { status: 401, headers: { "cache-control": "no-store" } });
    }
  }
});

export const config = {
  // Only routes that use Clerk. Marketing and docs pages skip it: running Clerk there costs a
  // handshake redirect round trip (~0.6 s on the dev instance) before first paint.
  matcher: ["/dashboard/:path*", "/device/:path*", "/sign-in/:path*", "/sign-up/:path*", "/api/v1/:path*"],
};
