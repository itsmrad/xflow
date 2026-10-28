import Link from "next/link";
import { FOOTER } from "@/content/site";
import { cliVersion } from "@/lib/version";
import { Crossings } from "./frame";
import { ThemeToggle } from "./theme-toggle";
import { DotWordmark } from "./wordmark";

export function SiteFooter() {
  return (
    <footer className="frame rule">
      <Crossings />
      <div className="grid grid-cols-2 gap-y-8 p-[var(--cell-p)] md:grid-cols-4">
        {FOOTER.map((col) => (
          <div key={col.title}>
            <h2 className="t-mono-label text-fg-subtle">{col.title}</h2>
            <ul className="mt-4 space-y-1">
              {col.links.map(([href, label]) => (
                <li key={href}>
                  {href.startsWith("http") ? (
                    <a href={href} className="t-body-sm inline-flex min-h-6 items-center text-fg-muted hover:text-fg">
                      {label}
                    </a>
                  ) : (
                    <Link href={href} className="t-body-sm inline-flex min-h-6 items-center text-fg-muted hover:text-fg">
                      {label}
                    </Link>
                  )}
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
      <div className="dash-t px-[var(--cell-p)] pt-10 pb-6">
        <DotWordmark />
      </div>
      <div className="flex flex-wrap items-center justify-between gap-4 px-[var(--cell-p)] pb-[var(--cell-p)]">
        <p className="t-caption text-fg-subtle">© 2026 xflow · MIT licensed</p>
        <ThemeToggle />
        <p className="t-mono-sm text-fg-subtle">v{cliVersion()}</p>
      </div>
    </footer>
  );
}
