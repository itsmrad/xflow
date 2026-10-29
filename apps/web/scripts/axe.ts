/**
 * Accessibility check: axe-core on every public route, light and dark (§10.1).
 * `bun scripts/axe.ts [baseUrl]` against `next start`; exits 1 on any violation.
 */
import AxeBuilder from "@axe-core/playwright";
import { chromium } from "@playwright/test";

const base = process.argv[2] ?? "http://localhost:3000";
const ROUTES = ["/", "/pricing", "/download", "/compare/wispr-flow", "/docs", "/docs/getting-started/install", "/docs/reference/cli", "/changelog", "/privacy", "/sign-in", "/missing-page"];

const browser = await chromium.launch({ channel: "chrome" });
let failed = 0;
for (const theme of ["light", "dark"] as const) {
  const ctx = await browser.newContext({ colorScheme: theme, reducedMotion: "reduce" });
  const page = await ctx.newPage();
  for (const route of ROUTES) {
    await page.goto(base + route, { waitUntil: "networkidle" });
    const { violations } = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
    for (const v of violations) {
      failed++;
      console.log(`${theme} ${route}: ${v.id} (${v.impact}) ${v.nodes.slice(0, 3).map((n) => n.target.join(" ")).join(" | ")}`);
    }
  }
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} violations` : "axe: 0 violations");
process.exit(failed ? 1 : 0);
