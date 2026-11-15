import "server-only";
import { after } from "next/server";
import { sql } from "drizzle-orm";
import { db } from "@/db";
import { usageEvents } from "@/db/schema";
import { CHAT_DAY_MICROS, CHAT_MONTH_MICROS, CHAT_MODEL, publicChatFrame } from "@/lib/chat";
import { GROQ_BASE_URL } from "@/lib/groq";
import { json } from "@/lib/http";
import { FAIR_USE } from "@/lib/pricing";
import { releaseSlot } from "./limits";
import type { ApiCaller } from "./api-auth";

export async function streamChat(caller: ApiCaller, body: unknown, reserve: number, signal: AbortSignal): Promise<Response> {
  const { rows } = await db.execute<{ result: string }>(sql`SELECT xflow_admit_chat(${caller.userId}, ${reserve}::int,
    ${FAIR_USE.dailyMicros}::int, ${FAIR_USE.monthlyMicros}::int, ${CHAT_DAY_MICROS}::int, ${CHAT_MONTH_MICROS}::int) AS result`);
  const result = rows[0]?.result;
  if (result !== "ok") return json({ error: { code: result?.includes("budget") ? "spend_cap" : "rate_limited", message: "Hosted chat limit reached. Daily caps reset at 00:00 UTC; monthly caps on the 1st. Your own API key still works." } }, 429);
  const started = performance.now();
  const abort = new AbortController();
  const combined = AbortSignal.any([signal, abort.signal, AbortSignal.timeout(55_000)]);
  let finished = false;
  const finish = async (status: number) => {
    if (finished) return;
    finished = true;
    // Keep the conservative reservation, even on disconnect/missing usage. No race at UTC rollover.
    await Promise.allSettled([
      releaseSlot("chat", caller.userId),
      db.insert(usageEvents).values({ userId: caller.userId, tokenId: caller.tokenId, kind: "chat", model: CHAT_MODEL, status, costMicros: reserve, latencyMs: Math.round(performance.now() - started) }),
    ]);
  };
  let upstream;
  try {
    upstream = await fetch(`${(process.env.GROQ_BASE_URL || GROQ_BASE_URL).replace(/\/+$/, "")}/chat/completions`, {
      method: "POST", headers: { authorization: `Bearer ${process.env.GROQ_API_KEY}`, "content-type": "application/json" },
      body: JSON.stringify(body), signal: combined, redirect: "error",
    });
  } catch {
    after(() => finish(504));
    return json({ error: { message: "Hosted chat provider unavailable" } }, 504);
  }
  if (!upstream.ok || !upstream.body) {
    await upstream.body?.cancel();
    after(() => finish(502));
    return json({ error: { message: "Hosted chat provider unavailable" } }, 502);
  }
  const reader = upstream.body.getReader();
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  let buffer = "", bytes = 0, done = false;
  let complete!: () => void;
  const completion = new Promise<void>(resolve => { complete = resolve; });
  after(() => completion);
  const stream = new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        while (!done) {
          const chunk = await reader.read();
          if (chunk.done) throw new Error("interrupted");
          bytes += chunk.value.length;
          if (bytes > 4 * 1024 * 1024) throw new Error("oversized stream");
          buffer += decoder.decode(chunk.value, { stream: true }).replace(/\r/g, "");
          if (buffer.length > 256 * 1024) throw new Error("oversized frame");
          let at;
          let emitted = false;
          while ((at = buffer.indexOf("\n\n")) !== -1) {
            const frame = buffer.slice(0, at); buffer = buffer.slice(at + 2);
            const data = frame.split("\n").filter(l => l.startsWith("data:")).map(l => l.slice(5).trimStart()).join("\n");
            if (!data) continue;
            controller.enqueue(encoder.encode(`data: ${publicChatFrame(data)}\n\n`));
            emitted = true;
            if (data === "[DONE]") { done = true; break; }
          }
          if (done) {
            controller.close(); await reader.cancel(); await finish(200); complete();
          }
          if (emitted || done) return;
        }
      } catch {
        abort.abort();
        controller.error(new Error("Hosted chat interrupted; keep the partial answer and retry"));
        await finish(502); complete();
      }
    },
    async cancel() { done = true; abort.abort(); await reader.cancel().catch(() => {}); await finish(499); complete(); },
  });
  return new Response(stream, { headers: { "content-type": "text/event-stream", "cache-control": "no-store", "x-accel-buffering": "no" } });
}
