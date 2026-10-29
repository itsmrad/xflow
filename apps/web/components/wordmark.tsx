import Link from "next/link";

export function Wordmark({ className = "" }: { className?: string }) {
  return (
    <Link href="/" className={`inline-flex items-center font-mono text-[18px] font-medium leading-none tracking-tight text-fg ${className}`} aria-label="xflow home">
      xflow
      <span aria-hidden className="cursor ml-0.5 !w-[0.45em]" />
    </Link>
  );
}

/** Dot-matrix wordmark (decorative, §7.19), a static file drawn through a mask in --line-strong. */
export function DotWordmark() {
  return <div aria-hidden className="art w-full bg-line-strong" style={{ aspectRatio: "29 / 7", maskImage: "url(/art/wordmark.svg)" }} />;
}

/** Dot-matrix waveform behind a heading (§4.4 ascii-wave). */
export function Wave({ className = "" }: { className?: string }) {
  return <div aria-hidden className={`art pointer-events-none bg-line-strong ${className}`} style={{ maskImage: "url(/art/wave.svg)" }} />;
}
