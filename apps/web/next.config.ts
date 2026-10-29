import type { NextConfig } from "next";
import { createMDX } from "fumadocs-mdx/next";

// A production deploy without these builds green and then answers 500 on every account route and
// /api/v1 (Clerk: "Missing secretKey"). Fail the build instead. Pro adds its own set once launched.
if (process.env.VERCEL_ENV === "production") {
  const required = ["NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY", "CLERK_SECRET_KEY", "DATABASE_URL"];
  if (process.env.PRO_LAUNCHED === "1") {
    required.push("DODO_PAYMENTS_API_KEY", "DODO_PAYMENTS_WEBHOOK_SECRET", "DODO_PRO_PRODUCT_ID", "GROQ_API_KEY");
  }
  const missing = required.filter((name) => !process.env[name]);
  if (missing.length) throw new Error(`production build is missing env: ${missing.join(", ")}`);
}

const origin = (url?: string) => {
  try {
    return url ? new URL(url).origin : "";
  } catch {
    return "";
  }
};
// The Clerk Frontend API host is encoded in the publishable key (pk_live_<base64 "host$">). A
// production instance serves it from clerk.<domain>; on *.vercel.app it is proxied at /__clerk ('self').
const clerkFapi = (() => {
  try {
    const host = atob(process.env.NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY?.split("_")[2] ?? "").replace(/\$$/, "");
    return /^[a-z0-9.-]+$/i.test(host) ? `https://${host}` : "";
  } catch {
    return "";
  }
})();
// The dashboard's live usage socket.
const realtime = origin(process.env.NEXT_PUBLIC_NEON_REALTIME_URL);
const csp = (directive: string, ...sources: string[]) => [directive, ...sources.filter(Boolean)].join(" ");

const security = [
  { key: "Cross-Origin-Opener-Policy", value: "same-origin-allow-popups" },
  { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
  { key: "Permissions-Policy", value: "camera=(), microphone=(), geolocation=()" },
  { key: "Strict-Transport-Security", value: "max-age=63072000; includeSubDomains; preload" },
  { key: "X-Content-Type-Options", value: "nosniff" },
  // ponytail: static pages can't carry per-request nonces, so inline scripts are allowed by 'unsafe-inline';
  // move to nonces (proxy.ts) only if marketing pages become dynamic anyway.
  {
    key: "Content-Security-Policy",
    value: [
      "default-src 'self'",
      csp("script-src", "'self'", "'unsafe-inline'", "https://*.clerk.accounts.dev", "https://*.clerk.com", "https://challenges.cloudflare.com", clerkFapi),
      // Clerk's bot protection probes *.clerk.com on non-443 ports, which a portless source never matches.
      csp(
        "connect-src",
        "'self'",
        "https://*.clerk.accounts.dev",
        "https://*.clerk.com:*",
        "https://clerk-telemetry.com",
        clerkFapi,
        realtime.replace(/^http/, "ws"),
        realtime.replace(/^ws/, "http"),
      ),
      "img-src 'self' data: https://img.clerk.com",
      "style-src 'self' 'unsafe-inline'",
      "font-src 'self'",
      "frame-src https://challenges.cloudflare.com",
      "worker-src 'self' blob:",
      "form-action 'self'",
      "frame-ancestors 'none'",
      "base-uri 'self'",
      "object-src 'none'",
    ].join("; "),
  },
];

const config: NextConfig = {
  poweredByHeader: false,
  reactStrictMode: true,
  async headers() {
    return [{ source: "/:path*", headers: security }];
  },
};

export default createMDX()(config);
