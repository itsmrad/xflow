import "server-only";
import { after } from "next/server";
import { db } from "@/db";
import { usageEvents } from "@/db/schema";
import { chatUsage, GROQ_BASE_URL } from "@/lib/groq";
import { error } from "@/lib/http";
import { admit, releaseSlot } from "@/lib/server/limits";
import type { ApiCaller } from "@/lib/server/api-auth";

type Kind = "transcribe" | "cleanup";

// Pro is unlimited for a person dictating; these only stop scripts and leaked tokens.
// Daily: 2,000 transcriptions is ~8 hours of back-to-back 15 s dictations.
const LIMITS: Record<Kind, { perMinute: number; perDay: number; concurrent: number }> = {
  transcribe: { perMinute: 40, perDay: 2000, concurrent: 2 },
  cleanup: { perMinute: 60, perDay: 4000, concurrent: 2 },
};
const UPSTREAM_TIMEOUT_MS = 55_000;
const SLOT_LEASE_SECONDS = 90;

export function groqKey(): string | null {
  return process.env.GROQ_API_KEY || null;
}

// Server env only: lets a test run point at a stub, or ops at another Groq endpoint.
const groqBase = () => (process.env.GROQ_BASE_URL || GROQ_BASE_URL).replace(/\/+$/, "");

/** Gate shared by both proxy routes. Returns an error response, or null to proceed. */
export function gate(caller: ApiCaller): Response | null {
  if (!caller.pro) return error(402, "hosted transcription needs xflow Pro; subscribe at /dashboard");
  if (!groqKey()) return error(503, "hosted transcription is not configured");
  return null;
}

export type Forward = {
  kind: Kind;
  caller: ApiCaller;
  model: string;
  path: "/audio/transcriptions" | "/chat/completions";
  body: BodyInit;
  contentType?: string;
  audioBytes?: number;
  audioMs?: number;
  signal: AbortSignal;
};

/** Forwards one validated request to Groq with the central key, streams the reply, records usage. */
export async function forward(f: Forward): Promise<Response> {
  const limits = LIMITS[f.kind];
  const prefix = `${f.kind}:${f.caller.userId}`;
  const slotKey = `${prefix}:inflight`;
  const { minuteOk, dayOk, slot } = await admit(prefix, limits, SLOT_LEASE_SECONDS);
  if (!minuteOk || !dayOk || !slot) {
    if (slot) after(() => releaseSlot(slotKey));
    if (!slot) return error(429, `at most ${limits.concurrent} ${f.kind} requests at a time`, { "retry-after": "1" });
    return error(429, minuteOk ? "daily request ceiling reached" : "too many requests; slow down", {
      "retry-after": minuteOk ? "3600" : "10",
    });
  }

  const started = performance.now();
  let status = 502;
  let usageText: Promise<string> = Promise.resolve("");
  const finish = () =>
    after(async () => {
      const text = await usageText.catch(() => "");
      const usage = f.kind === "cleanup" ? chatUsage(text) : { inputTokens: 0, outputTokens: 0 };
      await Promise.allSettled([
        releaseSlot(slotKey),
        db.insert(usageEvents).values({
          userId: f.caller.userId,
          tokenId: f.caller.tokenId,
          kind: f.kind,
          model: f.model,
          status,
          audioBytes: f.audioBytes ?? 0,
          audioMs: f.audioMs ?? 0,
          ...usage,
          latencyMs: Math.round(performance.now() - started),
        }),
      ]);
    });

  let upstream: Response;
  try {
    upstream = await fetch(`${groqBase()}${f.path}`, {
      method: "POST",
      headers: { authorization: `Bearer ${groqKey()}`, ...(f.contentType ? { "content-type": f.contentType } : {}) },
      body: f.body,
      signal: AbortSignal.any([f.signal, AbortSignal.timeout(UPSTREAM_TIMEOUT_MS)]),
    });
  } catch (e) {
    status = f.signal.aborted ? 499 : 504;
    finish();
    return error(504, e instanceof Error && e.name === "TimeoutError" ? "provider timed out" : "provider unreachable");
  }

  const upstreamMs = Math.round(performance.now() - started);
  status = upstream.status;
  const timing = { "server-timing": `upstream;dur=${upstreamMs}` };
  if (!upstream.ok || !upstream.body) {
    const detail = (await upstream.text().catch(() => "")).slice(0, 2000);
    finish();
    if (upstream.status === 401 || upstream.status === 403) {
      console.error(`groq rejected the central key: ${upstream.status}`);
      return error(502, "provider rejected the request", timing);
    }
    if (upstream.status === 429) {
      return error(503, "provider is busy; retry shortly", { ...timing, "retry-after": upstream.headers.get("retry-after") ?? "2" });
    }
    if (upstream.status >= 500) return error(502, "provider error", timing);
    // 400/413/422: the provider's own message is the most useful thing to show.
    return new Response(detail, {
      status: upstream.status,
      headers: { "content-type": upstream.headers.get("content-type") ?? "application/json", "cache-control": "no-store", ...timing },
    });
  }

  // Stream the reply straight through; the second branch feeds usage accounting after it's sent.
  const [client, meter] = upstream.body.tee();
  usageText = f.kind === "cleanup" ? new Response(meter).text() : meter.cancel().then(() => "");
  finish();
  return new Response(client, {
    status: upstream.status,
    headers: {
      "content-type": upstream.headers.get("content-type") ?? "application/json",
      "cache-control": "no-store",
      ...timing,
    },
  });
}
