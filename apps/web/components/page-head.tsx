import type { ReactNode } from "react";
import { Crossings, SectionLabel } from "./frame";

/** Page header cell: mono label, H1 (optionally with a highlighted second line) and lede. */
export function PageHead({ label, title, hl, lede, children }: { label: string; title: ReactNode; hl?: ReactNode; lede?: ReactNode; children?: ReactNode }) {
  return (
    <section className="frame rule relative -mt-px overflow-hidden">
      <Crossings />
      <div aria-hidden className="bg-cells pointer-events-none absolute inset-0 opacity-70" />
      <div className="relative px-[var(--cell-p)] pb-14 pt-16 md:pt-20">
        <SectionLabel title={label} />
        <h1 className="t-h1 mt-6 max-w-[22ch] text-fg">
          {title}
          {hl && (
            <>
              <br />
              <span className="hl">{hl}</span>
            </>
          )}
        </h1>
        {lede && <p className="t-lede mt-5 max-w-[60ch] text-fg-muted">{lede}</p>}
        {children}
      </div>
    </section>
  );
}
