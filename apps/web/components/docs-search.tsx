"use client";

import { lazy, Suspense, useEffect, useRef, useState } from "react";

// The search client (and its index fetch) loads on first open, not with every docs page.
const Results = lazy(() => import("./docs-search-results"));

/** ⌘K or / opens a native <dialog> (top layer, no layout shift) with static local search (§7.21). */
export function DocsSearch() {
  const ref = useRef<HTMLDialogElement>(null);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const typing = e.target instanceof HTMLElement && e.target.closest("input, textarea, [contenteditable]");
      if ((e.key === "k" && (e.metaKey || e.ctrlKey)) || (e.key === "/" && !typing)) {
        e.preventDefault();
        ref.current?.showModal();
        setOpen(true);
      }
    }
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, []);
  return (
    <>
      <button
        type="button"
        onClick={() => {
          ref.current?.showModal();
          setOpen(true);
        }}
        className="t-body-sm flex h-9 w-full items-center gap-2 rounded-sm border border-line bg-bg-raised px-3 text-fg-subtle hover:border-line-strong"
      >
        <span className="flex-1 text-left">Search docs</span>
        <kbd className="t-mono-sm hidden rounded-xs border border-line px-1.5 text-[12px] pointer-fine:inline">⌘K</kbd>
      </button>
      <dialog
        ref={ref}
        onClose={() => setOpen(false)}
        onClick={(e) => e.target === ref.current && ref.current?.close()}
        aria-label="Search docs"
        className="mx-auto mt-[12vh] w-[min(640px,calc(100%-24px))] rounded-md border border-line bg-bg-raised p-0 text-fg shadow-2 backdrop:bg-[color-mix(in_oklab,var(--bg)_70%,transparent)] backdrop:backdrop-blur-sm"
      >
        {open && (
          <Suspense fallback={<p className="t-body-sm h-12 px-4 py-3 text-fg-subtle">Loading search…</p>}>
            <Results close={() => ref.current?.close()} />
          </Suspense>
        )}
      </dialog>
    </>
  );
}
