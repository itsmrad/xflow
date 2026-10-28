import type { ReactNode } from "react";

export type Line = { t?: string; kind?: "cmd" | "idle" | "listening" | "processing" | "success" | "error" | "muted" | "out"; text: ReactNode; note?: string; className?: string };

const KIND: Record<NonNullable<Line["kind"]>, string> = {
  cmd: "text-syn-fg",
  out: "text-syn-fg",
  idle: "text-fg-muted",
  muted: "text-syn-muted",
  listening: "text-rec",
  processing: "text-syn-number",
  success: "text-ok",
  error: "text-rec",
};

/** Window chrome with outline circles, never traffic lights (§7.8). */
export function WindowChrome({ title, right, children }: { title: ReactNode; right?: ReactNode; children?: ReactNode }) {
  return (
    <div className="flex h-9 items-center gap-3 border-b border-line px-4">
      <span aria-hidden className="flex gap-1.5">
        {[0, 1, 2].map((i) => (
          <span key={i} className="size-2.5 rounded-full border border-line-strong" />
        ))}
      </span>
      <span className="t-mono-sm min-w-0 flex-1 truncate text-center text-fg-subtle">{title}</span>
      {right ?? <span aria-hidden className="w-[42px]" />}
      {children}
    </div>
  );
}

export function Terminal({
  title,
  lines,
  live = false,
  className = "",
  bodyClass = "",
  lineClass,
}: {
  title: string;
  lines: Line[];
  live?: boolean;
  className?: string;
  bodyClass?: string;
  lineClass?: (i: number) => string;
}) {
  return (
    <figure className={`not-prose overflow-hidden rounded-md border border-line bg-bg-raised shadow-1 ${className}`}>
      <WindowChrome
        title={title}
        right={
          live ? (
            <span className="live t-mono-label flex items-center gap-1.5 text-fg-subtle">
              [ live <span aria-hidden className="live-dot" /> ]
            </span>
          ) : undefined
        }
      />
      <pre tabIndex={0} className={`t-mono-sm overflow-x-auto p-5 ${bodyClass}`}>
        <code className="grid">
          {lines.map((l, i) => (
            <span key={i} className={`flex gap-4 whitespace-pre ${lineClass?.(i) ?? ""} ${l.className ?? ""}`}>
              {l.t && <span className="text-syn-muted">{l.t}</span>}
              <span className={KIND[l.kind ?? "out"]}>
                {l.kind === "cmd" && (
                  <span aria-hidden className="select-none text-syn-muted">
                    ${" "}
                  </span>
                )}
                {l.text}
              </span>
              {l.note && <span className="text-syn-muted"># {l.note}</span>}
            </span>
          ))}
        </code>
      </pre>
    </figure>
  );
}
