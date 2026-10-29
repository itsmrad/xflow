/**
 * Browser regression checks for the small interactive pieces, against `next start`; exits 1 on failure.
 * `bun scripts/ui-check.ts [baseUrl]`
 * - Copy buttons with the clipboard refused: no error, the visible commands end up selected.
 * - Theme toggle with localStorage blocked: the choice still applies; the OS only matters on System.
 * - Docs TOC: the heading in view is marked, after scrolling, after a TOC click, after client navigation.
 * - Mobile menu: closes after a link in it is followed.
 * - CLS ≤ 0.01 on /sign-in and /sign-up at 390 and 1440 (Clerk mounts client-side). Skipped, and said so,
 *   when the server has no Clerk keys (CI).
 */
import { chromium, type Page } from "@playwright/test";

const base = process.argv[2] ?? "http://localhost:3000";
const browser = await chromium.launch({ channel: "chrome" });
let failed = 0;
function check(name: string, ok: boolean, detail = "") {
  if (!ok) failed++;
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
}
async function fresh(init?: () => void, viewport = { width: 1280, height: 800 }) {
  const page = await browser.newPage({ viewport, colorScheme: "light" });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  if (init) await page.addInitScript(init);
  return { page, errors };
}

// Copy fallback
{
  const { page, errors } = await fresh(() => {
    Object.defineProperty(navigator, "clipboard", { value: { writeText: () => Promise.reject(new DOMException("denied", "NotAllowedError")) } });
  });
  const selected = () => page.evaluate(() => getSelection()?.toString() ?? "");
  await page.goto(`${base}/download`, { waitUntil: "networkidle" });
  await page.getByRole("tab", { name: "Linux (beta)" }).click();
  await page.getByRole("button", { name: "Copy commands" }).click();
  check("copy: /download Linux tab selected", (await selected()).includes("sudo apt install") && !errors.length, errors.join(" | "));
  await page.goto(`${base}/docs/providers/groq`, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "Copy code" }).first().click();
  check("copy: docs code block selected", (await selected()).includes("xflow key set groq") && !errors.length, errors.join(" | "));
  await page.close();
}

// Theme with storage blocked
{
  const { page, errors } = await fresh(() => {
    for (const k of ["getItem", "setItem", "removeItem"] as const) {
      Storage.prototype[k] = () => {
        throw new DOMException("blocked", "SecurityError");
      };
    }
  });
  const state = () =>
    page.evaluate(() => ({
      theme: document.documentElement.dataset.theme,
      checked: [...document.querySelectorAll("[data-mode]")].find((b) => b.getAttribute("aria-checked") === "true")?.getAttribute("data-mode"),
    }));
  await page.goto(`${base}/pricing`, { waitUntil: "networkidle" });
  await page.click("[data-mode=dark]");
  let s = await state();
  check("theme: Dark applies with storage blocked", s.theme === "dark" && s.checked === "dark", JSON.stringify(s));
  await page.emulateMedia({ colorScheme: "dark" });
  await page.emulateMedia({ colorScheme: "light" });
  s = await state();
  check("theme: OS change ignored while Dark", s.theme === "dark" && s.checked === "dark", JSON.stringify(s));
  await page.click("[data-mode=system]");
  await page.emulateMedia({ colorScheme: "dark" });
  // The OS change arrives as a media-query change event; give it a moment instead of reading at once.
  await page.waitForFunction(() => document.documentElement.dataset.theme === "dark", undefined, { timeout: 2000 }).catch(() => {});
  s = await state();
  check("theme: System follows the OS", s.theme === "dark" && s.checked === "system" && !errors.length, JSON.stringify(s) + errors.join(" | "));
  await page.close();
}

// Docs TOC tracking (xl: the TOC is visible)
{
  const { page, errors } = await fresh(undefined, { width: 1440, height: 900 });
  const current = (p: Page) => p.evaluate(() => document.querySelector('nav[aria-label="On this page"] a[aria-current="location"]')?.getAttribute("href") ?? null);
  const settle = () => page.waitForTimeout(400);
  await page.goto(`${base}/docs/reference/config`, { waitUntil: "networkidle" });
  const hrefs = await page.$$eval('nav[aria-label="On this page"] a', (as) => as.map((a) => a.getAttribute("href")!));
  check("toc: first heading marked on load", (await current(page)) === hrefs[0], `${await current(page)}`);
  const target = hrefs[Math.floor(hrefs.length / 2)]!;
  await page.evaluate((id) => document.getElementById(decodeURIComponent(id.slice(1)))!.scrollIntoView(), target);
  await settle();
  check("toc: scrolled heading marked", (await current(page)) === target, `${await current(page)}, expected ${target}`);
  const last = hrefs.at(-2)!;
  await page.click(`nav[aria-label="On this page"] a[href="${last}"]`);
  await settle();
  check("toc: clicked heading marked", (await current(page)) === last, `${await current(page)}, expected ${last}`);
  await page.click('nav[aria-label=Docs] a[href="/docs/providers/openai"] >> visible=true');
  await page.waitForURL("**/docs/providers/openai");
  await page.waitForLoadState("networkidle");
  const next = await page.$$eval('nav[aria-label="On this page"] a', (as) => as.map((a) => a.getAttribute("href")!));
  await page.evaluate((id) => document.getElementById(decodeURIComponent(id.slice(1)))!.scrollIntoView(), next.at(-1)!);
  await settle();
  await page.evaluate(() => scrollTo(0, 1e6));
  await settle();
  check("toc: a jump to the bottom marks the last heading", (await current(page)) === next.at(-1), `${await current(page)}`);
  check("toc: tracking works after client navigation", (await current(page)) === next.at(-1) && !errors.length, `${await current(page)}, expected ${next.at(-1)} ${errors.join(" | ")}`);
  await page.close();
}

// Mobile menu closes after following a link (client navigation keeps the header mounted)
{
  const { page, errors } = await fresh(undefined, { width: 390, height: 800 });
  await page.goto(`${base}/`, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "Menu" }).click();
  const open = () => page.evaluate(() => document.getElementById("site-menu")!.matches(":popover-open"));
  const opened = await open();
  await page.locator('#site-menu a[href="/pricing"]').click();
  await page.waitForURL("**/pricing");
  check("menu: opens, then closes after navigating", opened && !(await open()) && !errors.length, `opened=${opened} ${errors.join(" | ")}`);
  await page.close();
}

// CLS on the Clerk pages: its card must land in the space reserved for it
for (const route of ["/sign-in", "/sign-up"]) {
  const status = (await fetch(base + route)).status;
  if (status !== 200) {
    console.log(`skip cls ${route}: ${status} (no Clerk keys on this server)`);
    continue;
  }
  for (const width of [390, 1440]) {
    const { page } = await fresh(() => {
      (window as unknown as { cls: number }).cls = 0;
      new PerformanceObserver((list) => {
        for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean }[]) {
          if (!e.hadRecentInput) (window as unknown as { cls: number }).cls += e.value;
        }
      }).observe({ type: "layout-shift", buffered: true });
    }, { width, height: 900 });
    await page.goto(base + route, { waitUntil: "networkidle" });
    await page.waitForSelector(".cl-rootBox", { timeout: 15000 });
    await page.waitForTimeout(1000);
    const cls = await page.evaluate(() => (window as unknown as { cls: number }).cls);
    check(`cls: ${route} at ${width}`, cls <= 0.01, cls.toFixed(3));
    await page.close();
  }
}

await browser.close();
process.exit(failed ? 1 : 0);
