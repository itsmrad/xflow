type Env = Record<string, string | undefined>;

/**
 * Names a production deploy can't run without; next.config.ts fails the build when any is missing.
 * Without them the build goes green and then answers 500 on every account route and /api/v1, or
 * (realtime) the dashboard loses live usage. Previews and dev stay permissive.
 */
export function missingProductionEnv(env: Env): string[] {
  if (env.VERCEL_ENV !== "production") return [];
  const required = [
    "NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY",
    "CLERK_SECRET_KEY",
    "DATABASE_URL",
    "NEON_REALTIME_SECRET",
    "NEXT_PUBLIC_NEON_REALTIME_URL",
  ];
  if (env.PRO_LAUNCHED === "1") required.push("DODO_PAYMENTS_API_KEY", "DODO_PAYMENTS_WEBHOOK_SECRET", "DODO_PRO_PRODUCT_ID", "GROQ_API_KEY");
  const missing = required.filter((name) => !env[name]);
  const url = env.NEXT_PUBLIC_NEON_REALTIME_URL;
  if (url && !/^(wss|https):\/\/[^/\s]+/.test(url)) missing.push("NEXT_PUBLIC_NEON_REALTIME_URL (must be a wss:// or https:// URL)");
  return missing;
}
