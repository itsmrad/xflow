"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useRef, useSyncExternalStore } from "react";

/**
 * Signed-in hint without loading Clerk on marketing pages: Clerk keeps a readable `__client_uat`
 * cookie that is non-zero while a session exists. Wrong at worst means "Sign in" → Clerk redirects.
 */
function signedIn() {
  return /(?:^|; )__client_uat=(?!0(?:;|$))\d+/.test(document.cookie);
}

export function AuthSlot() {
  const on = useSyncExternalStore(() => () => {}, signedIn, () => false);
  return (
    <span className="inline-flex w-[96px] justify-end">
      <Link href={on ? "/dashboard" : "/sign-in"} prefetch={false} className="t-label inline-flex h-8 items-center rounded-sm px-3 text-fg-muted transition-colors duration-[120ms] hover:bg-ink-a4 hover:text-fg">
        {on ? "Dashboard" : "Sign in"}
      </Link>
    </span>
  );
}

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
              className={
                vertical
                  ? `t-label flex h-12 items-center border-b border-line px-[var(--gutter)] ${current ? "text-fg" : "text-fg-muted"}`
                  : `t-label relative inline-flex h-16 items-center px-3 transition-colors duration-[120ms] hover:text-fg ${current ? "text-fg after:absolute after:inset-x-3 after:bottom-[13px] after:h-0.5 after:bg-volt" : "text-fg-muted"}`
              }
            >
              {l.label}
            </Link>
          </li>
        );
      })}
    </ul>
  );
}

/** Closes the native popover menu after navigation (§7.1). */
export function CloseMenuOnNavigate({ id }: { id: string }) {
  const path = usePathname();
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    document.getElementById(id)?.hidePopover?.();
  }, [path, id]);
  return null;
}
