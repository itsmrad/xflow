/**
 * Writes the decorative dot-matrix SVGs to public/art (§4.4: textures are files, not DOM).
 * Pages draw them with CSS mask-image so they take the theme's --line-strong colour.
 * Run: bun scripts/gen-art.ts
 */
import { writeFileSync } from "node:fs";

const GLYPHS: Record<string, string[]> = {
  x: ["00000", "00000", "10001", "01010", "00100", "01010", "10001"],
  f: ["00110", "01001", "01000", "11100", "01000", "01000", "01000"],
  l: ["01100", "00100", "00100", "00100", "00100", "00100", "01110"],
  o: ["00000", "00000", "01110", "10001", "10001", "10001", "01110"],
  w: ["00000", "00000", "10001", "10001", "10101", "10101", "01010"],
};

const word = "xflow";
const cells: string[] = [];
[...word].forEach((ch, gi) =>
  GLYPHS[ch]!.forEach((row, y) => [...row].forEach((bit, x) => bit === "1" && cells.push(`M${gi * 6 + x} ${y}h.86v.86h-.86z`))),
);
writeFileSync("public/art/wordmark.svg", `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 29 7"><path d="${cells.join("")}"/></svg>\n`);

const rects: string[] = [];
for (let x = 0; x < 96; x++) {
  const a = Math.abs(Math.sin(x * 0.21) * 0.6 + Math.sin(x * 0.047 + 1) * 0.4);
  const h = Math.max(1, Math.round(a * 9));
  for (let y = 0; y < h; y++) rects.push(`M${x * 2} ${10 - y}h1.2v.7h-1.2z`, `M${x * 2} ${10 + y}h1.2v.7h-1.2z`);
}
writeFileSync("public/art/wave.svg", `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 192 21" preserveAspectRatio="none"><path d="${rects.join("")}"/></svg>\n`);
