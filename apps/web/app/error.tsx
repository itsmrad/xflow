"use client";

import Link from "next/link";

/**
 * Any server or render failure below the root layout (§9.11 styling; header and footer come from the
 * root layout). Plain markup on purpose: this boundary ships with every page. Details stay in the log.
 */
export default function ErrorPage({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  return (
    <main id="content" className="frame rule -mt-px">
      <div className="mx-auto flex min-h-[calc(100dvh-64px-120px)] max-w-[480px] flex-col justify-center px-[var(--cell-p)] py-16">
        <p className="t-mono-label text-fg-subtle">[ 500 ]</p>
        <h1 className="t-h2 mt-4 text-fg">Something broke on our side.</h1>
        <p className="t-body mt-3 text-fg-muted">Try again. If it keeps happening, report it with the reference below.</p>
        {error.digest && <p className="t-mono-sm mt-4 text-fg-subtle">ref {error.digest}</p>}
        <div className="mt-6 flex gap-3">
          <button type="button" onClick={reset} className="t-label inline-flex h-9 items-center rounded-sm bg-volt px-4 text-volt-ink">
            Try again
          </button>
          <Link href="/" className="t-label inline-flex h-9 items-center rounded-sm border border-line-strong px-4 text-fg hover:bg-ink-a4">
            Go home
          </Link>
        </div>
      </div>
    </main>
  );
}
