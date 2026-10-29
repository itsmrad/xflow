import { NAV, REPO } from "@/content/site";
import { getStars } from "@/lib/github";
import { NavLinks } from "./header-client";
import { IconGitHub, IconMenu } from "./icons";
import { Button } from "./ui";
import { Wordmark } from "./wordmark";

const CLOSE_MENU = `document.currentScript.parentElement.addEventListener("click",e=>{if(e.target.closest("a"))e.currentTarget.hidePopover?.()})`;

export async function SiteHeader() {
  const stars = await getStars();
  return (
    <header className="sticky top-0 z-10 border-b border-line bg-[color-mix(in_oklab,var(--bg)_85%,transparent)] backdrop-blur-md">
      <div className="frame relative flex h-16 items-center gap-2 pl-[var(--cell-p)] pr-3 lg:gap-4">
        <Wordmark className="mr-auto lg:mr-6" />
        <nav aria-label="Main" className="hidden lg:block lg:mr-auto">
          <NavLinks links={NAV} className="flex" />
        </nav>
        {stars !== null && (
          <a href={REPO} className="t-mono-sm hidden min-w-[6ch] items-center gap-1.5 text-fg-muted hover:text-fg lg:inline-flex">
            <IconGitHub /> {stars >= 1000 ? `${(stars / 1000).toFixed(1)}k` : stars}
            <span className="sr-only"> stars on GitHub</span>
          </a>
        )}
        <span className="hidden lg:inline-flex">
          <AuthSlot />
        </span>
        <Button href="/download" variant="primary" size="sm">
          Get xflow
        </Button>
        <button
          type="button"
          popoverTarget="site-menu"
          aria-controls="site-menu"
          aria-label="Menu"
          className="grid size-10 place-items-center rounded-sm text-fg-muted hover:bg-ink-a4 hover:text-fg lg:hidden"
        >
          <IconMenu />
        </button>
      </div>
      <div
        id="site-menu"
        popover="auto"
        className="inset-x-0 top-16 m-0 w-full border-b border-line bg-bg p-0 text-fg shadow-2 lg:hidden [&:popover-open]:block"
      >
        <nav aria-label="Mobile">
          <NavLinks links={[...NAV, { href: "/sign-in", label: "Sign in" }]} vertical />
        </nav>
        {/* Close the menu when a link in it is followed. The header lives in the root layout, so this runs once. */}
        <script dangerouslySetInnerHTML={{ __html: CLOSE_MENU }} />
      </div>
    </header>
  );
}

/**
 * Signed-in hint without loading Clerk on marketing pages: Clerk keeps a readable `__client_uat` cookie
 * that is non-zero while a session exists. Server markup says "Sign in"; this inline script flips it.
 * Wrong at worst means "Sign in" → Clerk redirects. A plain link: account routes load Clerk anyway.
 */
const AUTH = `if(/(?:^|; )__client_uat=(?!0(?:;|$))\\d+/.test(document.cookie)){const a=document.currentScript.previousElementSibling;a.href="/dashboard";a.textContent="Dashboard"}`;

function AuthSlot() {
  return (
    <span className="inline-flex w-[96px] justify-end">
      {/* A plain <a>: <Link> would route by its own href prop and ignore the script's swap to /dashboard. */}
      {/* eslint-disable-next-line @next/next/no-html-link-for-pages */}
      <a href="/sign-in" suppressHydrationWarning className="t-label inline-flex h-8 items-center rounded-sm px-3 text-fg-muted transition-colors duration-[120ms] hover:bg-ink-a4 hover:text-fg">
        Sign in
      </a>
      <script dangerouslySetInnerHTML={{ __html: AUTH }} />
    </span>
  );
}
