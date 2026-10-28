import { defineConfig } from "@neon/config/v1";

// Postgres: accounts, tokens, billing, usage. Realtime: the dashboard's live usage.
// Object storage: CLI release binaries, served through presigned URLs.
// No Neon Functions: every server path runs in the Next.js app on Vercel.
export default defineConfig({
  preview: {
    realtime: { allowedOrigins: ["http://localhost:3000"] },
  },
  auth: false,
  buckets: {
    releases: { access: "private" },
  },
  branch: (branch) => {
    if (branch.isDefault || branch.exists) return {};
    // New non-default branches auto-expire.
    return { ttl: "7d" };
  },
});
