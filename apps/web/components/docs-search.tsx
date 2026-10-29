"use client";

import { lazy, Suspense, useEffect, useRef, useState } from "react";

// The search client (and its index fetch) loads on first open, not with every docs page.
const Results = lazy(() => import("./docs-search-results"));

/** ⌘K or / opens a native <dialog> (top layer, no layout shift) with static local search (§7.21). */
export function DocsSearch() {
  const ref = useRef<HTMLDialogElement>(null);
  const [open, setOpen] = useState(false);
  const show = () => {
    ref.current?.showModal();
    setOpen(true);
  };
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const typing = e.target instanceof HTMLElement && e.target.closest("input, textarea, [contenteditable]");
      if ((e.key === "k" && (e.metaKey || e.ctrlKey)) || (e.key === "/" && !typing)) {
        e.preventDefault();
        show();
      }
    }
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, []);
  return (
    <>
      <button
        type="button"
        onClick={show}
        className="search-button"
      >
        <span>Search docs</span>
        <kbd>⌘K</kbd>
      </button>
      <dialog
        ref={ref}
        onClose={() => setOpen(false)}
        onClick={(e) => e.target === ref.current && ref.current?.close()}
        aria-label="Search docs"
        className="search-dialog"
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
