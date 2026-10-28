// DB regression tests run only against TEST_DATABASE_URL (a throwaway Neon branch), never the app DB.
import { readFileSync } from "node:fs";

export const testDbUrl = process.env.TEST_DATABASE_URL;

// Captured before any test repoints DATABASE_URL.
const appDbUrl = (() => {
  if (process.env.DATABASE_URL && process.env.DATABASE_URL !== testDbUrl) return process.env.DATABASE_URL;
  try {
    return readFileSync(new URL("../../.env.local", import.meta.url), "utf8").match(/^DATABASE_URL="?([^"\n]*)"?$/m)?.[1] ?? "";
  } catch {
    return "";
  }
})();

/** Points @/db at the test branch; refuses the host .env.local uses for the app. */
export function useTestDatabase(): void {
  if (!testDbUrl) throw new Error("TEST_DATABASE_URL is not set");
  const host = (url: string) => new URL(url).hostname.replace("-pooler", "");
  if (appDbUrl && host(appDbUrl) === host(testDbUrl)) throw new Error("TEST_DATABASE_URL points at the app database");
  process.env.DATABASE_URL = testDbUrl;
}
