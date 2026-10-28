import Link from "next/link";

export function Wordmark({ className = "" }: { className?: string }) {
  return (
    <Link href="/" className={`inline-flex items-center font-mono text-[18px] font-medium leading-none tracking-tight text-fg ${className}`} aria-label="xflow home">
      xflow
      <span aria-hidden className="cursor ml-0.5 !w-[0.45em]" />
    </Link>
  );
}

/** 5×7 dot-matrix glyphs for the footer wordmark (decorative SVG, §7.19). */
const GLYPHS: Record<string, string[]> = {
  x: ["00000", "00000", "10001", "01010", "00100", "01010", "10001"],
  f: ["00110", "01001", "01000", "11100", "01000", "01000", "01000"],
  l: ["01100", "00100", "00100", "00100", "00100", "00100", "01110"],
  o: ["00000", "00000", "01110", "10001", "10001", "10001", "01110"],
  w: ["00000", "00000", "10001", "10001", "10101", "10101", "01010"],
};

export function DotWordmark() {
  const word = "xflow";
  const cols = word.length * 6 - 1;
  const cells: string[] = [];
  [...word].forEach((ch, gi) =>
    GLYPHS[ch]!.forEach((row, y) =>
      [...row].forEach((bit, x) => {
        if (bit === "1") cells.push(`M${gi * 6 + x} ${y}h.86v.86h-.86z`);
      }),
    ),
  );
  return (
    <svg aria-hidden viewBox={`-0.07 -0.07 ${cols + 0.14} 7.14`} className="block h-auto w-full text-line-strong" style={{ aspectRatio: `${cols + 0.14} / 7.14` }}>
      <path d={cells.join("")} fill="currentColor" />
    </svg>
  );
}

/** Static dot-matrix waveform drawn behind a heading (§4.4 ascii-wave). Deterministic, no DOM text. */
export function Wave({ className = "" }: { className?: string }) {
  const cols = 96;
  const rects: string[] = [];
  for (let x = 0; x < cols; x++) {
    const a = Math.abs(Math.sin(x * 0.21) * 0.6 + Math.sin(x * 0.047 + 1) * 0.4);
    const h = Math.max(1, Math.round(a * 9));
    for (let y = 0; y < h; y++) rects.push(`M${x * 2} ${10 - y}h1.2v.7h-1.2z`, `M${x * 2} ${10 + y}h1.2v.7h-1.2z`);
  }
  return (
    <svg aria-hidden viewBox="0 0 192 21" preserveAspectRatio="none" className={`pointer-events-none text-line-strong ${className}`}>
      <path d={rects.join("")} fill="currentColor" />
    </svg>
  );
}
