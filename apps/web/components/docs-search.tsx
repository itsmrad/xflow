"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { useStaticSearch } from "fumadocs-core/search/client";

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
        <kbd className="t-mono-sm rounded-xs border border-line px-1.5 text-[12px]">⌘K</kbd>
      </button>
      <dialog
        ref={ref}
        onClose={() => setOpen(false)}
        onClick={(e) => e.target === ref.current && ref.current?.close()}
        aria-label="Search docs"
        className="mx-auto mt-[12vh] w-[min(640px,calc(100%-24px))] rounded-md border border-line bg-bg-raised p-0 text-fg shadow-2 backdrop:bg-[color-mix(in_oklab,var(--bg)_70%,transparent)] backdrop:backdrop-blur-sm"
      >
        {open && <Results close={() => ref.current?.close()} />}
      </dialog>
    </>
  );
}

function Results({ close }: { close: () => void }) {
  const { search, onSearchChange, isLoading, data } = useStaticSearch({ from: "/api/search" });
  const items = (data?.items ?? []).flatMap((i) => (i.type === "table" ? i.rows : [i])).slice(0, 12);
  return (
    <div>
      <label className="flex h-12 items-center gap-3 border-b border-line px-4">
        <span className="sr-only">Search</span>
        <span aria-hidden className="t-mono text-fg-subtle">/</span>
        <input
          autoFocus
          value={search}
          onChange={(e) => onSearchChange(e.target.value)}
          placeholder="Search the docs"
          className="t-body h-full flex-1 bg-transparent outline-none placeholder:text-fg-subtle"
        />
        <kbd className="t-mono-sm rounded-xs border border-line px-1.5 text-[12px] text-fg-subtle">Esc</kbd>
      </label>
      <ul className="max-h-[60vh] overflow-y-auto p-2" role="list">
        {search && !isLoading && items.length === 0 && <li className="t-body-sm p-3 text-fg-subtle">No results for “{search}”.</li>}
        {items.map((r) => (
          <li key={r.id}>
            <Link href={r.url} onClick={close} className="t-body-sm block rounded-sm px-3 py-2 text-fg-muted hover:bg-ink-a4 hover:text-fg focus-visible:bg-ink-a4">
              <span className={r.type === "page" ? "font-medium text-fg" : r.type === "heading" ? "text-fg" : "line-clamp-1"}>
                {r.type !== "page" && <span className="text-fg-subtle"># </span>}
                {r.content}
              </span>
            </Link>
          </li>
        ))}
      </ul>
    </div>
  );
}
