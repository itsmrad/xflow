"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

/** Nav links with aria-current; client only because the active route is known after navigation. */
export function NavLinks({ links, className = "", vertical = false }: { links: readonly { href: string; label: string }[]; className?: string; vertical?: boolean }) {
  const path = usePathname();
  return (
    <ul className={className}>
      {links.map((l) => {
        const current = l.href !== "/#how-it-works" && (path === l.href || path.startsWith(`${l.href}/`));
        return (
          <li key={l.href}>
            <Link
              href={l.href}
              aria-current={current ? "page" : undefined}
              className={`t-label text-fg-muted aria-[current=page]:text-fg ${
                vertical
                  ? "flex h-12 items-center border-b border-line px-[var(--gutter)]"
                  : "relative inline-flex h-16 items-center px-3 transition-colors duration-[120ms] hover:text-fg aria-[current=page]:after:absolute aria-[current=page]:after:inset-x-3 aria-[current=page]:after:bottom-[13px] aria-[current=page]:after:h-0.5 aria-[current=page]:after:bg-volt"
              }`}
            >
              {l.label}
            </Link>
          </li>
        );
      })}
    </ul>
  );
}
