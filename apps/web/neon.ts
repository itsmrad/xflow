import { defineConfig } from "@neon/config/v1";

// Postgres: accounts, tokens, billing, usage. Realtime: the dashboard's live usage.
// Object storage: CLI release binaries, served through presigned URLs.
// No Neon Functions: every server path runs in the Next.js app on Vercel.
export default defineConfig({
  // Realtime ("preview" is Neon's name for the service tier, not an environment): the origins whose
  // browsers may open the dashboard's live usage socket.
  preview: {
    realtime: { allowedOrigins: ["https://xflowcli.vercel.app", "http://localhost:3000"] },
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
