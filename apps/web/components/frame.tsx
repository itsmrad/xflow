import type { ReactNode } from "react";

/** The two `+` marks where a full-bleed rule crosses the rails (§4.1). */
export function Crossings({ spark = false }: { spark?: boolean }) {
  if (spark)
    return (
      <>
        <Spark className="spark spark-l" />
        <Spark className="spark spark-r" />
      </>
    );
  return (
    <>
      <span aria-hidden className="x x-l" />
      <span aria-hidden className="x x-r" />
    </>
  );
}

function Spark({ className }: { className: string }) {
  return (
    <svg aria-hidden className={className} viewBox="0 0 16 16" fill="currentColor">
      <path d="M8 0c.4 4.2 3.8 7.6 8 8-4.2.4-7.6 3.8-8 8-.4-4.2-3.8-7.6-8-8 4.2-.4 7.6-3.8 8-8Z" />
    </svg>
  );
}

export function SectionLabel({ n, total, title }: { n?: number; total?: number; title: string }) {
  const pad = (v: number) => String(v).padStart(2, "0");
  return (
    <p aria-hidden className="t-mono-label text-fg-muted">
      <span className="text-fg-subtle">[ </span>
      {n !== undefined ? (
        <>
          <span className="text-volt-text">{pad(n)}</span>
          {total !== undefined && <span className="text-fg-subtle"> / {pad(total)}</span>}
        </>
      ) : (
        <span className="text-fg">{title}</span>
      )}
      <span className="text-fg-subtle"> ]</span>
      {n !== undefined && <span> · {title}</span>}
    </p>
  );
}

type Label = { n?: number; total?: number; title: string };

/**
 * A section on the grid: frame-width with rails, a full-bleed top rule with crossings,
 * an optional 48px label row (itself followed by a full-bleed rule).
 */
export function Section({
  id,
  label,
  children,
  className = "",
  spark = false,
  labelledBy,
}: {
  id?: string;
  label?: Label;
  children: ReactNode;
  className?: string;
  spark?: boolean;
  labelledBy?: string;
}) {
  return (
    <section id={id} aria-labelledby={labelledBy} className="frame rule">
      <Crossings spark={spark} />
      {label ? (
        <>
          <div className="label-row">
            <SectionLabel {...label} />
          </div>
          <div className={`rule ${className}`}>
            <Crossings />
            {children}
          </div>
        </>
      ) : (
        <div className={className}>{children}</div>
      )}
    </section>
  );
}

/** Section heading block: H2 + lede, spacing per §5.3 (label→H2 16, H2→lede 16, lede→content 40). */
export function Heading({ id, title, lede, as: As = "h2" }: { id?: string; title: ReactNode; lede?: ReactNode; as?: "h1" | "h2" }) {
  return (
    <div className="mb-10 max-w-[64ch]">
      <As id={id} className={`${As === "h1" ? "t-h1 max-w-[20ch]" : "t-h2 max-w-[28ch]"} text-fg`}>
        {title}
      </As>
      {lede && <p className="t-lede mt-4 text-fg-muted">{lede}</p>}
    </div>
  );
}
