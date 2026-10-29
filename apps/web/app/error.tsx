"use client";

import { CenterPanel } from "@/components/account";
import { Button, buttonClass } from "@/components/ui";

/** Any server or render failure below the root layout (§9.11 styling). Details stay in the server log. */
export default function ErrorPage({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  return (
    <main id="content">
      <CenterPanel label="500" wide>
        <h1 className="t-h2 text-fg">Something broke on our side.</h1>
        <p className="t-body mt-3 text-fg-muted">Try again. If it keeps happening, report it with the reference below.</p>
        {error.digest && <p className="t-mono-sm mt-4 text-fg-subtle">ref {error.digest}</p>}
        <div className="mt-6 flex gap-3">
          <button type="button" onClick={reset} className={buttonClass("primary", "md")}>
            Try again
          </button>
          <Button href="/" variant="secondary">
            Go home
          </Button>
        </div>
      </CenterPanel>
    </main>
  );
}
