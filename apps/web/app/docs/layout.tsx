import type { ReactNode } from "react";
import { DocsSearch } from "@/components/docs-search";
import { Sidebar } from "@/components/docs-nav";
import { source } from "@/lib/source";

export default function DocsLayout({ children }: { children: ReactNode }) {
  return (
    <div className="frame lg:grid lg:grid-cols-[264px_minmax(0,1fr)]">
      <aside className="border-b border-line lg:border-b-0 lg:border-r">
        <div className="p-4 lg:sticky lg:top-16 lg:max-h-[calc(100dvh-64px)] lg:overflow-y-auto lg:py-6">
          <DocsSearch />
          <details className="group mt-3 lg:hidden">
            <summary className="t-label flex h-10 cursor-pointer list-none items-center justify-between rounded-sm px-3 text-fg-muted hover:bg-ink-a4 [&::-webkit-details-marker]:hidden">
              Browse docs <span aria-hidden className="t-mono transition-transform duration-[200ms] group-open:rotate-45">+</span>
            </summary>
            <div className="pt-3">
              <Sidebar tree={source.pageTree} />
            </div>
          </details>
          <div className="mt-6 hidden lg:block">
            <Sidebar tree={source.pageTree} />
          </div>
        </div>
      </aside>
      <main id="content" className="min-w-0">
        {children}
      </main>
    </div>
  );
}
