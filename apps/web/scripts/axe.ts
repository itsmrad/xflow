/**
 * Accessibility check: axe-core on every sitemap route plus a 404, light and dark, at 375 px and desktop (§10.1).
 * `bun scripts/axe.ts [baseUrl] [extra routes...]` against `next start`; exits 1 on any violation.
 */
import AxeBuilder from "@axe-core/playwright";
import { chromium } from "@playwright/test";

const [base = "http://localhost:3000", ...extra] = process.argv.slice(2);
const sitemap = await (await fetch(`${base}/sitemap.xml`)).text();
const ROUTES = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => new URL(m[1]!).pathname);
if (ROUTES.length < 10) throw new Error(`sitemap lists only ${ROUTES.length} routes`);
ROUTES.push("/a-missing-page", ...extra);

const browser = await chromium.launch({ channel: "chrome" });
let failed = 0;
for (const theme of ["light", "dark"] as const) {
  for (const width of [375, 1280]) {
    const ctx = await browser.newContext({ colorScheme: theme, reducedMotion: "reduce", viewport: { width, height: 800 } });
    const page = await ctx.newPage();
    for (const route of ROUTES) {
      await page.goto(base + route, { waitUntil: "networkidle" });
      const { violations } = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
      for (const v of violations) {
        failed++;
        console.log(`${theme} ${width} ${route}: ${v.id} (${v.impact}) ${v.nodes.slice(0, 3).map((n) => n.target.join(" ")).join(" | ")}`);
      }
    }
    await ctx.close();
  }
}
await browser.close();
console.log(failed ? `${failed} violations` : `axe: 0 violations on ${ROUTES.length} routes x 2 themes x 375/1280`);
process.exit(failed ? 1 : 0);
