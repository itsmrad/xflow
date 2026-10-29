import Link from "next/link";
import { Children, isValidElement, type ReactNode } from "react";
import { CLAIMS, type ClaimId } from "@/content/claims";

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md" | "lg";

const VARIANT: Record<Variant, string> = {
  primary: "bg-volt text-volt-ink hover:brightness-95",
  secondary: "bg-bg-raised text-fg border border-line-strong hover:bg-[color-mix(in_oklab,var(--bg-raised),var(--fg)_4%)]",
  ghost: "text-fg-muted hover:bg-ink-a4 hover:text-fg",
  danger: "text-rec border border-[color-mix(in_oklab,var(--rec)_40%,transparent)] hover:bg-[color-mix(in_oklab,var(--rec)_8%,transparent)]",
};
const SIZE: Record<Size, string> = { sm: "h-8 px-3", md: "h-10 px-4", lg: "h-12 px-5" };

export function buttonClass(variant: Variant = "secondary", size: Size = "md", extra = "") {
  return `t-label inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-sm transition-[filter,background-color,color,transform] duration-[120ms] active:translate-y-px disabled:opacity-40 aria-disabled:opacity-40 ${VARIANT[variant]} ${SIZE[size]} ${extra}`;
}

/** §7.2: `<a>` when there's an href, otherwise `<button type="button">`. */
export function Button({
  href,
  variant,
  size,
  className,
  children,
  ...rest
}: { href?: string; variant?: Variant; size?: Size; className?: string; children: ReactNode } & Record<string, unknown>) {
  const cls = buttonClass(variant, size, className);
  if (href) {
    if (href.startsWith("http"))
      return (
        <a href={href} className={cls} {...rest}>
          {children}
        </a>
      );
    return (
      <Link href={href} className={cls} {...rest}>
        {children}
      </Link>
    );
  }
  return (
    <button type="button" className={cls} {...rest}>
      {children}
    </button>
  );
}

export function LinkMono({ href, children, className = "" }: { href: string; children: ReactNode; className?: string }) {
  return (
    <Link href={href} className={`t-mono-sm text-volt-text underline-offset-4 hover:underline ${className}`}>
      {children}
    </Link>
  );
}

export function Kbd({ children, active = false }: { children: ReactNode; active?: boolean }) {
  return (
    <kbd
      className={`t-mono-sm inline-flex h-6 min-w-7 items-center justify-center rounded-xs border border-b-2 px-1.5 font-medium ${
        active ? "border-[#9DBF2A] bg-volt text-volt-ink" : "border-line-strong bg-bg-raised text-fg"
      }`}
    >
      {children}
    </kbd>
  );
}

const BADGE = {
  NEW: "bg-volt-soft text-volt-text",
  BETA: "text-warn bg-[color-mix(in_oklab,var(--warn)_12%,transparent)]",
  PRO: "bg-volt text-volt-ink",
  NEUTRAL: "bg-ink-a8 text-fg-muted",
} as const;

export function Badge({ children, tone = "NEUTRAL" }: { children: ReactNode; tone?: keyof typeof BADGE }) {
  return <span className={`t-mono-label inline-flex h-5 items-center rounded-xs px-1.5 ${BADGE[tone]}`}>{children}</span>;
}

const CALLOUT = {
  note: { bar: "var(--line-strong)", mark: "[i]" },
  privacy: { bar: "var(--volt)", mark: "[*]" },
  warn: { bar: "var(--warn)", mark: "[!]" },
  danger: { bar: "var(--rec)", mark: "[!]" },
} as const;

export function Callout({ type = "note", title, children }: { type?: keyof typeof CALLOUT; title?: ReactNode; children: ReactNode }) {
  const c = CALLOUT[type];
  return (
    <div className="not-prose t-body-sm flex gap-3 py-1 pl-4 text-fg-muted" style={{ boxShadow: `inset 2px 0 0 ${c.bar}` }}>
      <span aria-hidden className="t-mono-sm shrink-0 text-fg-subtle">
        {c.mark}
      </span>
      <div className="min-w-0 [&>p+p]:mt-2 [&_code]:t-mono-sm [&_code]:text-fg [&_a]:text-fg [&_a]:underline [&_a]:underline-offset-2">
        {title && <strong className="font-medium text-fg">{title} </strong>}
        {children}
      </div>
    </div>
  );
}

/** Footnote marker that resolves to the claims ledger. */
export function Ref({ n }: { n: ClaimId }) {
  return (
    <a
      href={CLAIMS[n].href}
      title={CLAIMS[n].text}
      className="ml-0.5 align-super font-mono text-[0.65em] font-normal text-fg-subtle no-underline hover:text-fg"
    >
      <span className="sr-only">Source: </span>
      {n}
    </a>
  );
}

export function Abbr() {
  return <abbr title="bring your own key">BYOK</abbr>;
}

const textOf = (node: ReactNode): string =>
  typeof node === "string" || typeof node === "number"
    ? String(node)
    : Array.isArray(node)
      ? node.map(textOf).join("")
      : isValidElement<{ children?: ReactNode }>(node)
        ? textOf(node.props.children)
        : "";

/** The header cells of a table's first row, for its region label: "Model, Name, Note". */
function headerLabel(table: ReactNode): string {
  const kids = (el: ReactNode) => Children.toArray(isValidElement<{ children?: ReactNode }>(el) ? el.props.children : null);
  const thead = Children.toArray(table).find((c) => isValidElement(c) && c.type === "thead");
  return kids(kids(thead)[0])
    .map(textOf)
    .filter(Boolean)
    .join(", ");
}

/**
 * Wide tables scroll inside this region on phones. It takes focus (with the global focus ring) so
 * keyboard users can scroll it, and its label names the columns (WCAG scrollable-region-focusable).
 */
export function TableScroll({ children }: { children: ReactNode }) {
  const columns = headerLabel(children);
  return (
    <div role="region" tabIndex={0} aria-label={columns ? `Table: ${columns}` : "Table"} className="table-scroll">
      <table>{children}</table>
    </div>
  );
}
