import type { NextConfig } from "next";
import { createMDX } from "fumadocs-mdx/next";

const security = [
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
      "script-src 'self' 'unsafe-inline' https://*.clerk.accounts.dev https://*.clerk.com https://challenges.cloudflare.com",
      "connect-src 'self' https://*.clerk.accounts.dev https://*.clerk.com https://clerk-telemetry.com",
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
