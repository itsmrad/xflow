import { expect, test } from "bun:test";
import { missingProductionEnv } from "./env";

const core = {
  VERCEL_ENV: "production",
  NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY: "pk_live_x",
  CLERK_SECRET_KEY: "sk_live_x",
  DATABASE_URL: "postgres://x",
  NEON_REALTIME_SECRET: "s",
  NEXT_PUBLIC_NEON_REALTIME_URL: "wss://ep-x.rt.neon.tech/v1",
};

test("previews and dev need nothing", () => {
  expect(missingProductionEnv({ VERCEL_ENV: "preview" })).toEqual([]);
  expect(missingProductionEnv({})).toEqual([]);
});

test("production needs auth, the database and realtime", () => {
  expect(missingProductionEnv(core)).toEqual([]);
  expect(missingProductionEnv({ ...core, NEON_REALTIME_SECRET: undefined, NEXT_PUBLIC_NEON_REALTIME_URL: "" })).toEqual(["NEON_REALTIME_SECRET", "NEXT_PUBLIC_NEON_REALTIME_URL"]);
  expect(missingProductionEnv({ ...core, NEXT_PUBLIC_NEON_REALTIME_URL: "localhost:5432" })).toEqual([
    "NEXT_PUBLIC_NEON_REALTIME_URL (must be a wss:// or https:// URL)",
  ]);
});

test("a launched Pro also needs billing and the provider key", () => {
  expect(missingProductionEnv({ ...core, PRO_LAUNCHED: "1" })).toEqual([
    "DODO_PAYMENTS_API_KEY",
    "DODO_PAYMENTS_WEBHOOK_SECRET",
    "DODO_PRO_PRODUCT_ID",
    "GROQ_API_KEY",
  ]);
});
