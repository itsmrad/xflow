"use client";

import "./globals.css";

/** Last resort when the root layout itself fails: no fonts or header, just the tokens. */
export default function GlobalError({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  return (
    <html lang="en">
      <body className="grid min-h-dvh place-items-center bg-bg p-6 font-mono text-fg">
        <main className="max-w-[440px]">
          <h1 className="t-h3">xflow hit an error.</h1>
          {error.digest && <p className="t-mono-sm mt-2 text-fg-subtle">ref {error.digest}</p>}
          <button type="button" onClick={reset} className="t-mono-sm mt-6 rounded-sm border border-line-strong px-3 py-2">
            Try again
          </button>
        </main>
      </body>
    </html>
  );
}
