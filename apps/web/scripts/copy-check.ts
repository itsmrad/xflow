/**
 * Copy buttons with the clipboard refused: no error, and the visible commands end up selected so ⌘C works.
 * Covers the tabbed install box (after switching tabs) and an MDX code block.
 * `bun scripts/copy-check.ts [baseUrl]` against `next start`; exits 1 on failure.
 */
import { chromium } from "@playwright/test";

const base = process.argv[2] ?? "http://localhost:3000";
const browser = await chromium.launch({ channel: "chrome" });
const page = await browser.newPage();
await page.addInitScript(() => {
  Object.defineProperty(navigator, "clipboard", { value: { writeText: () => Promise.reject(new DOMException("denied", "NotAllowedError")) } });
});
const errors: string[] = [];
page.on("pageerror", (e) => errors.push(e.message));

let failed = 0;
async function check(name: string, expected: string) {
  const selected = await page.evaluate(() => getSelection()?.toString() ?? "");
  const ok = selected.includes(expected) && errors.length === 0;
  if (!ok) failed++;
  console.log(`${ok ? "ok  " : "FAIL"} ${name}: selected ${selected.length} chars${errors.length ? `; errors: ${errors.join(" | ")}` : ""}`);
}

await page.goto(`${base}/download`, { waitUntil: "networkidle" });
await page.getByRole("tab", { name: "Linux (beta)" }).click();
await page.getByRole("button", { name: "Copy commands" }).click();
await check("/download Linux tab", "sudo apt install");

await page.goto(`${base}/docs/providers/groq`, { waitUntil: "networkidle" });
await page.getByRole("button", { name: "Copy code" }).first().click();
await check("/docs code block", "xflow key set groq");

await browser.close();
process.exit(failed ? 1 : 0);
