import type { ReactNode } from "react";
import { SUPPORT_EMAIL } from "@/content/site";
import { Section } from "./frame";
import { PageHead } from "./page-head";

/** Legal pages share the prose layout (§9.12). */
export function LegalPage({ title, lede, updated, children }: { title: string; lede: string; updated: string; children: ReactNode }) {
  return (
    <>
      <PageHead label="Legal" title={title} lede={lede} />
      <Section>
        <div className="p-[var(--cell-p)] py-12">
          <p className="t-mono-sm text-fg-subtle">
            Last updated <time dateTime={updated}>{updated}</time> · <Mail />
          </p>
          <div className="prose mt-10">{children}</div>
        </div>
      </Section>
    </>
  );
}

export function Mail() {
  return (
    <a href={`mailto:${SUPPORT_EMAIL}`} className="underline underline-offset-2 hover:text-fg">
      {SUPPORT_EMAIL}
    </a>
  );
}
