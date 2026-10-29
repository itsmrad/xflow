/**
 * DESIGN.md §10.2 size budgets, from the `next build` output alone (deterministic, no server, no timing):
 * first-load JS (gzip) per prerendered route, CSS (gzip), preloaded font bytes and the home page's HTML.
 * `bun scripts/budget.ts` after `bun run build`; exits 1 over budget. Timing budgets (LCP, TBT) stay
 * with Lighthouse locally: shared CI runners make them flaky.
 *
 * JS can't meet §10.2 on this stack: React 19 + the Next 16 App Router runtime alone are ~136 KB gz on
 * every page, over the 90 KB marketing budget before any of our code. So JS fails on a ceiling just above
 * today's size (a regression ratchet; lower it as JS comes off) and reports the §10.2 gap on every run.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { gzipSync } from "node:zlib";

const KB = 1024;
export const BUDGET = { marketingJs: 90 * KB, docsJs: 140 * KB, css: 30 * KB, fonts: 110 * KB, fontFiles: 2, homeHtml: 60 * KB };
export const JS_CEILING = { marketing: 143 * KB, docs: 147 * KB };

const NEXT = join(import.meta.dir, "../.next");
const gz = (bytes: Buffer) => gzipSync(bytes, { level: 9 }).length;
const walk = (dir: string): string[] =>
  readdirSync(dir).flatMap((f) => (statSync(join(dir, f)).isDirectory() ? walk(join(dir, f)) : [join(dir, f)]));
const asset = (href: string) => readFileSync(join(NEXT, href.replace(/^\/_next\//, "")));
const kb = (n: number) => `${(n / KB).toFixed(1)} KB`;

const failures: string[] = [];
const over = (what: string, size: number, budget: number) => {
  if (size > budget) failures.push(`${what}: ${kb(size)} > ${kb(budget)}`);
};

const pages = walk(join(NEXT, "server/app")).filter((f) => f.endsWith(".html"));
const rows: [string, number, number][] = [];
const jsGap: [string, number][] = [];
for (const file of pages) {
  const route = "/" + relative(join(NEXT, "server/app"), file).replace(/\.html$/, "").replace(/(^|\/)index$/, "");
  if (route.startsWith("/_")) continue; // _not-found, _global-error: not routes anyone lands on
  const html = readFileSync(file, "utf8");
  // noModule scripts are legacy polyfills modern browsers never download.
  const scripts = new Set([...html.matchAll(/<script(?![^>]*noModule)[^>]+src="(\/_next\/static\/[^"]+\.js)"/g)].map((m) => m[1]!));
  const styles = new Set([...html.matchAll(/<link[^>]+href="(\/_next\/static\/[^"]+\.css)"/g)].map((m) => m[1]!));
  const js = [...scripts].reduce((n, s) => n + gz(asset(s)), 0);
  const css = [...styles].reduce((n, s) => n + gz(asset(s)), 0);
  rows.push([route, js, css]);
  const docs = route.startsWith("/docs");
  over(`first-load JS ${route} (ceiling)`, js, docs ? JS_CEILING.docs : JS_CEILING.marketing);
  if (js > (docs ? BUDGET.docsJs : BUDGET.marketingJs)) jsGap.push([route, js - (docs ? BUDGET.docsJs : BUDGET.marketingJs)]);
  over(`CSS ${route}`, css, BUDGET.css);
  if (route === "/") over("HTML /", gz(Buffer.from(html)), BUDGET.homeHtml);
}
if (rows.length < 10) throw new Error(`only ${rows.length} prerendered pages found; run \`bun run build\` first`);

// Fonts a page preloads; the other unicode-range files (Cyrillic, Vietnamese…) load only for those glyphs.
const fonts = [...new Set(pages.flatMap((p) => [...readFileSync(p, "utf8").matchAll(/<link rel="preload" href="(\/_next\/static\/media\/[^"]+\.woff2)"/g)].map((m) => m[1]!)))];
const fontBytes = fonts.reduce((n, f) => n + asset(f).length, 0);
over("fonts", fontBytes, BUDGET.fonts);
if (fonts.length > BUDGET.fontFiles) failures.push(`fonts: ${fonts.length} files > ${BUDGET.fontFiles}`);

rows.sort((a, b) => b[1] - a[1]);
console.log(`route${" ".repeat(40)}first-load JS (gz)  CSS (gz)`);
for (const [route, js, css] of rows.slice(0, 8)) console.log(`${route.padEnd(45)}${kb(js).padStart(18)}  ${kb(css).padStart(8)}`);
console.log(`… ${rows.length} routes. Fonts: ${fonts.length} files, ${kb(fontBytes)}.`);
if (jsGap.length) {
  const worst = jsGap.reduce((a, b) => (b[1] > a[1] ? b : a));
  console.log(`\n§10.2 JS budget not met on ${jsGap.length} routes (worst ${worst[0]}, +${kb(worst[1])}); see the header comment.`);
}
console.log(failures.length ? `\nover budget:\n  ${failures.join("\n  ")}` : "\nbudget: fonts, CSS and HTML within §10.2; JS within its ceiling");
process.exit(failures.length ? 1 : 0);
