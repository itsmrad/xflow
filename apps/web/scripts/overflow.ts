/**
 * No page may scroll sideways: every sitemap route plus a 404 (and any extra routes), at 390/768/1440.
 * `bun scripts/overflow.ts [baseUrl] [extra routes...]` against `next start`; exits 1 on any overflow.
 */
import { chromium } from "@playwright/test";

const [base = "http://localhost:3000", ...extra] = process.argv.slice(2);
const sitemap = await (await fetch(`${base}/sitemap.xml`)).text();
const routes = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => new URL(m[1]!).pathname);
routes.push("/a-missing-page-with-a-rather-long-path/that-keeps-going/and-going", ...extra);

const browser = await chromium.launch({ channel: "chrome" });
let failed = 0;
for (const width of [390, 768, 1440]) {
  const page = await browser.newPage({ viewport: { width, height: 900 }, reducedMotion: "reduce" });
  for (const route of routes) {
    await page.goto(base + route, { waitUntil: "networkidle" });
    const scroll = await page.evaluate(() => document.documentElement.scrollWidth);
    if (scroll > width) {
      failed++;
      console.log(`${width} ${route}: ${scroll}px wide`);
    }
  }
  await page.close();
}
await browser.close();
console.log(failed ? `${failed} overflowing pages` : `overflow: 0 of ${routes.length} routes x 3 widths`);
process.exit(failed ? 1 : 0);
