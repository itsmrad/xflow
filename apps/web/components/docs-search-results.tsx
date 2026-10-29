"use client";

import Link from "next/link";
import { useStaticSearch } from "fumadocs-core/search/client";

export default function Results({ close }: { close: () => void }) {
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
