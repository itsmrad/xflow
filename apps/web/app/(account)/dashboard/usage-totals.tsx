import type { UsageRow } from "@/lib/server/realtime";

/** Today's hosted usage. Rendered live by LiveUsage, or once from the server when realtime is unavailable. */
export function UsageTotals({ rows, note }: { rows: readonly UsageRow[]; note: string }) {
  const get = (kind: UsageRow["kind"]) => rows.find((r) => r.kind === kind);
  const minutes = ((get("transcribe")?.audio_ms ?? 0) / 60000).toFixed(1);
  return (
    <div aria-live="polite" className="mt-4">
      <p className="t-caption text-fg-subtle">Today (UTC) · {note}</p>
      <dl className="mt-3 grid grid-cols-3 overflow-hidden rounded-md border border-line">
        {[
          ["Dictations", String(get("transcribe")?.requests ?? 0)],
          ["Audio min", minutes],
          ["Cleanups", String(get("cleanup")?.requests ?? 0)],
        ].map(([k, v], i) => (
          <div key={k} className={`p-4 ${i ? "border-l border-line" : ""}`}>
            <dt className="t-mono-label text-fg-subtle">{k}</dt>
            <dd className="mt-2 font-mono text-[28px] leading-8 font-medium tabular-nums text-fg">{v}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
