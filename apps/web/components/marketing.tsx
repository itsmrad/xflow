import type { ReactNode } from "react";
import { COMPARE, COMPARE_CHECKED, FREE_FEATURES, PRO_FEATURES } from "@/content/marketing";
import { CLAIMS, type ClaimId } from "@/content/claims";
import { REPO } from "@/content/site";
import { IconCheck } from "./icons";
import { Abbr, Badge, Button, Ref } from "./ui";

export function FeatureGrid({ children, cols = 2 }: { children: ReactNode; cols?: 2 | 3 }) {
  return (
    <div className={`cells ${cols === 3 ? "cells-3" : "cells-2"}`}>
      {children}
    </div>
  );
}

export function NumberedRows({ rows, active }: { rows: { title: ReactNode; body: ReactNode }[]; active?: number }) {
  return (
    <ol className="border-y border-line">
      {rows.map((r, i) => (
        <li
          key={i}
          className={`flex gap-5 px-[var(--cell-p)] py-5 ${i ? "border-t border-line" : ""} ${i === active ? "bg-bg-sunken shadow-[inset_2px_0_0_var(--volt)]" : ""}`}
        >
          <span className="t-mono-sm pt-1 text-fg-subtle">{String(i + 1).padStart(2, "0")}</span>
          <div>
            <h3 className="t-h3 text-fg">{r.title}</h3>
            <p className="t-body-sm mt-1 text-fg-muted">{r.body}</p>
          </div>
        </li>
      ))}
    </ol>
  );
}

/** Fig N. stat (§7.13). The caption links to its ledger source. */
export function Fig({ n, value, caption, claim, visual }: { n: number; value: string; caption: string; claim: ClaimId; visual?: ReactNode }) {
  return (
    <figure className="cell flex flex-col">
      {visual && <div aria-hidden className="mb-6 hidden h-12 sm:block">{visual}</div>}
      <p className="t-stat text-fg">{value}</p>
      <figcaption className="t-mono-sm mt-4 border-t border-line pt-3 text-fg-subtle">
        <a href={CLAIMS[claim].href} title={CLAIMS[claim].text} className="hover:text-fg">
          <span className="text-fg-muted">Fig {n}.</span> {caption}
        </a>
      </figcaption>
    </figure>
  );
}

/** A deterministic mini bar-code visual for Fig cells (decorative). */
export function Bars({ seed, filled }: { seed: number; filled: number }) {
  const n = 32;
  return (
    <svg viewBox={`0 0 ${n * 4} 48`} className="h-12 w-full max-w-[160px]" preserveAspectRatio="none">
      {Array.from({ length: n }, (_, i) => {
        const h = 8 + Math.round(Math.abs(Math.sin(i * 0.9 + seed) * Math.cos(i * 0.31 + seed)) * 40);
        return <rect key={i} x={i * 4} y={48 - h} width={2} height={h} fill={i < filled ? "var(--fg-muted)" : "var(--line-strong)"} />;
      })}
    </svg>
  );
}

export function ComparisonTable() {
  return (
    <div>
      <table className="w-full border-collapse text-left">
        <caption className="sr-only">xflow compared with Wispr Flow</caption>
        <thead>
          <tr className="border-b border-line">
            <th scope="col" className="w-[34%] py-3 pr-4">
              <span className="sr-only">Feature</span>
            </th>
            <th scope="col" className="bg-volt-soft px-4 py-3">
              <Badge tone="PRO">xflow</Badge>
            </th>
            <th scope="col" className="t-mono-label hidden px-4 py-3 text-fg-subtle md:table-cell">
              Wispr Flow
            </th>
          </tr>
        </thead>
        <tbody>
          {COMPARE.map((r) => (
            <tr key={r.label} className="border-b border-line align-top">
              <th scope="row" className="t-body-sm py-4 pr-4 font-normal text-fg-muted">
                {r.label}
              </th>
              <td className="t-body-sm bg-volt-soft px-4 py-4 font-medium text-fg">
                {r.xflow}
                <span className="t-caption mt-1 block font-normal text-fg-muted md:hidden">
                  Wispr Flow: {r.wispr}
                  {r.note && <Ref n={r.note} />}
                </span>
              </td>
              <td className="t-body-sm hidden px-4 py-4 text-fg-muted md:table-cell">
                {r.wispr}
                {r.note && <Ref n={r.note} />}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="t-caption mt-4 text-fg-subtle">
        Competitor data from public pages, checked {COMPARE_CHECKED}.{" "}
        <a className="underline underline-offset-2 hover:text-fg" href="https://github.com/itsmrad/xflow/issues/new">
          Tell us if it changed.
        </a>
      </p>
    </div>
  );
}

function Check({ children }: { children: ReactNode }) {
  return (
    <li className="t-body-sm flex gap-2.5 text-fg-muted">
      <IconCheck className="mt-[3px] shrink-0 text-ok" />
      <span>{children}</span>
    </li>
  );
}

/** Two grid cells, not floating cards (§7.15). */
export function PricingCards({ compact = false, headingLevel = 3 }: { compact?: boolean; headingLevel?: 2 | 3 }) {
  const H = headingLevel === 2 ? "h2" : "h3";
  const free = compact ? FREE_FEATURES.slice(0, 3) : FREE_FEATURES;
  const pro = compact ? PRO_FEATURES.slice(0, 3) : PRO_FEATURES;
  return (
    <div className="cells cells-2">
      <div className="cell flex flex-col">
        <H className="t-mono-label text-fg-muted">
          Free · <Abbr />
        </H>
        <p className="mt-6 flex items-baseline gap-2">
          <span className="t-stat text-fg">$0</span>
        </p>
        <p className="t-body-sm mt-3 text-fg-muted">Your provider key. Unlimited.</p>
        <div className="mt-6">
          <Button href="/download" variant="primary" size="lg" className="w-full sm:w-[168px]">
            Install
          </Button>
        </div>
        <ul className="dash-t mt-8 space-y-3 pt-6">
          {free.map((f) => (
            <Check key={f}>{f}</Check>
          ))}
        </ul>
      </div>
      <div className="relative flex flex-col shadow-[inset_0_2px_0_var(--volt)]">
        <div aria-hidden className="bg-dots pointer-events-none absolute inset-0 opacity-30" />
        <div className="cell relative flex flex-1 flex-col">
          <div className="flex items-center justify-between">
            <H className="t-mono-label text-fg-muted">Pro</H>
            <Badge tone="PRO">Coming soon</Badge>
          </div>
          <p className="mt-6 flex items-baseline gap-2">
            <span className="t-stat text-fg">$5</span>
            <span className="t-body-sm text-fg-muted">/month</span>
          </p>
          <p className="t-body-sm mt-3 text-fg-muted">Our key. Nothing to set up. Unlimited.</p>
          <div className="mt-6">
            <Button href={`${REPO}/releases`} variant="secondary" size="lg" className="w-full sm:w-[168px]">
              Watch releases
            </Button>
          </div>
          <ul className="dash-t mt-8 space-y-3 pt-6">
            {pro.map((f) => (
              <Check key={f}>{f}</Check>
            ))}
          </ul>
        </div>
      </div>
    </div>
  );
}

export function Faq({ items }: { items: { q: string; a: ReactNode }[] }) {
  return (
    <div className="border-t border-line">
      {items.map((f) => (
        <details key={f.q} className="faq border-b border-line">
          <summary className="flex min-h-14 items-center gap-4 py-4 hover:text-fg">
            <span aria-hidden className="plus t-mono text-fg-subtle">
              +
            </span>
            <span className="t-h4 text-fg">{f.q}</span>
          </summary>
          <p className="t-body max-w-[64ch] pb-6 pl-[34px] text-fg-muted">{f.a}</p>
        </details>
      ))}
    </div>
  );
}

export function faqJsonLd(items: { q: string; text: string }[]) {
  return {
    "@context": "https://schema.org",
    "@type": "FAQPage",
    mainEntity: items.map((f) => ({ "@type": "Question", name: f.q, acceptedAnswer: { "@type": "Answer", text: f.text } })),
  };
}

export function JsonLd({ data }: { data: object }) {
  return <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: JSON.stringify(data).replace(/</g, "\\u003c") }} />;
}
