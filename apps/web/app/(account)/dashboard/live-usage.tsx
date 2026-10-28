"use client";

import { createRealtimeClient, type SealedLiveQuery } from "@neon/realtime/client";
import { RealtimeProvider, useLiveQuery } from "@neon/realtime-react";
import { useCallback, useState } from "react";
import type { UsageRow } from "@/lib/server/realtime";

type Props = { url: string; sealed: SealedLiveQuery<UsageRow>; initialData: UsageRow[]; userId: string; since: string };

// A full page load resets this, so a signed-out page stops renewing.
let signedOut = false;

export function LiveUsage({ url, ...props }: Props) {
  const [client] = useState(() => createRealtimeClient({ url }));
  return (
    <RealtimeProvider client={client}>
      <Totals {...props} />
    </RealtimeProvider>
  );
}

function Totals({ sealed, initialData, userId, since }: Omit<Props, "url">) {
  const refreshQuery = useCallback(async () => {
    if (signedOut) throw new Error("Signed out");
    const res = await fetch("/api/v1/usage/live", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ userId, since }),
    });
    if (res.status === 401) {
      signedOut = true;
      window.location.reload();
      throw new Error("Signed out");
    }
    if (!res.ok) throw new Error(`renew failed: ${res.status}`);
    return (await res.json()) as SealedLiveQuery<UsageRow>;
  }, [userId, since]);

  const { data, status } = useLiveQuery(sealed, { refreshQuery, initialData });
  const rows = data ?? initialData;
  const get = (kind: UsageRow["kind"]) => rows.find((r) => r.kind === kind);
  const minutes = ((get("transcribe")?.audio_ms ?? 0) / 60000).toFixed(1);
  return (
    <div aria-live="polite" className="mt-4">
      <p className="t-caption text-fg-subtle">Today (UTC){status === "error" ? " · live updates stopped" : " · live"}</p>
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
