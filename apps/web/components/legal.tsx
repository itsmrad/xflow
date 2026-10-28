import type { ReactNode } from "react";
import { Section } from "./frame";
import { PageHead } from "./page-head";
import { Callout } from "./ui";

/** Legal pages share the prose layout (§9.12). Text comes from the owner; these are marked placeholders. */
export function LegalPage({ title, lede, children }: { title: string; lede: string; children: ReactNode }) {
  return (
    <>
      <PageHead label="Legal" title={title} lede={lede} />
      <Section>
        <div className="p-[var(--cell-p)] py-12">
          <Callout type="warn" title="Placeholder.">
            This page is not final legal text. [COMPANY] will publish the full version before Pro launches. Questions: [SUPPORT_EMAIL].
          </Callout>
          <div className="prose mt-10">{children}</div>
        </div>
      </Section>
    </>
  );
}
