import type { ReactNode } from "react";
import { Crossings, SectionLabel } from "./frame";

/** Centred panel on bg-dots inside the frame: sign-in, sign-up, /device (§9.6, §9.7). */
export function CenterPanel({ label, children, wide = false }: { label: string; children: ReactNode; wide?: boolean }) {
  return (
    <section className="frame rule relative -mt-px overflow-hidden">
      <Crossings />
      <div aria-hidden className="bg-dots pointer-events-none absolute inset-0" />
      <div className="relative flex min-h-[calc(100dvh-64px-120px)] flex-col items-center justify-center px-[var(--cell-p)] py-16">
        <div className={`w-full ${wide ? "max-w-[480px]" : "max-w-[440px]"}`}>
          <SectionLabel title={label} />
          <div className="mt-4">{children}</div>
        </div>
      </div>
    </section>
  );
}

export function Panel({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <div className={`rounded-md border border-line bg-bg-raised p-6 shadow-1 sm:p-8 ${className}`}>{children}</div>;
}
