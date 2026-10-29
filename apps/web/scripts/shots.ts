/**
 * Screenshot every route at 390 and 1440, light and dark: `bun scripts/shots.ts [baseUrl] [outDir] [routes...]`.
 * Uses the system Chrome so no browser download is needed.
 */
import { chromium } from "@playwright/test";

const [base = "http://localhost:3000", out = "../../.reports/web-shots", ...only] = process.argv.slice(2);
const ROUTES = only.length
  ? only
  : ["/", "/pricing", "/download", "/compare/wispr-flow", "/docs", "/docs/getting-started/install", "/docs/reference/cli", "/changelog", "/privacy", "/sign-in", "/device", "/dashboard", "/this-page-does-not-exist"];

const browser = await chromium.launch({ channel: "chrome" });
for (const theme of ["light", "dark"] as const)
  for (const width of [390, 1440]) {
    const ctx = await browser.newContext({ viewport: { width, height: 900 }, colorScheme: theme, reducedMotion: "reduce", deviceScaleFactor: 1 });
    const page = await ctx.newPage();
    for (const route of ROUTES) {
      await page.goto(base + route, { waitUntil: "networkidle" });
      const name = (route === "/" ? "home" : route.slice(1).replace(/\//g, "_")) + `-${width}-${theme}.png`;
      await page.screenshot({ path: `${out}/${name}`, fullPage: true });
      console.log(name);
    }
    await ctx.close();
  }
await browser.close();
