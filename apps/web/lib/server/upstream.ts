import "server-only";
import { after } from "next/server";
import { db } from "@/db";
import { usageEvents } from "@/db/schema";
import { GROQ_BASE_URL, publicBody, publicMessage } from "@/lib/groq";
import { json } from "@/lib/http";
import { FAIR_USE } from "@/lib/pricing";
import { admit, releaseSlot, settle, type Admission } from "@/lib/server/limits";
import type { ApiCaller } from "@/lib/server/api-auth";

type Kind = "transcribe" | "cleanup";

// Request ceilings stop scripts; the spend budget in FAIR_USE bounds what any account costs.
const LIMITS: Record<Kind, { perMinute: number; perDay: number; concurrent: number }> = {
  transcribe: { perMinute: 40, perDay: 2000, concurrent: 2 },
  cleanup: { perMinute: 60, perDay: 4000, concurrent: 2 },
};
const UPSTREAM_TIMEOUT_MS = 55_000;
const SLOT_LEASE_SECONDS = 90;
const MAX_REPLY_BYTES = 1024 * 1024;

export function groqKey(): string | null {
  return process.env.GROQ_API_KEY || null;
}

// Server env only: lets a test run point at a stub, or ops at another endpoint.
const groqBase = () => (process.env.GROQ_BASE_URL || GROQ_BASE_URL).replace(/\/+$/, "");

function fail(status: number, message: string, code?: string, headers: HeadersInit = {}): Response {
  return json({ error: { message, ...(code ? { code } : {}) } }, status, headers);
}

/** Gate shared by both proxy routes. Returns an error response, or null to proceed. */
export function gate(caller: ApiCaller): Response | null {
  if (!caller.pro) return fail(402, "hosted transcription needs xflow Pro; subscribe at /dashboard", "pro_required");
  if (!groqKey()) return fail(503, "hosted transcription is not configured");
  return null;
}

function secondsUntil(next: Date): string {
  return String(Math.max(1, Math.ceil((next.getTime() - Date.now()) / 1000)));
}

function refused(a: Exclude<Admission, "ok">, kind: Kind): Response {
  const now = new Date();
  const tomorrow = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() + 1));
  const nextMonth = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1));
  switch (a) {
    case "concurrency":
      return fail(429, `at most ${LIMITS[kind].concurrent} ${kind} requests at a time`, "concurrency", { "retry-after": "1" });
    case "minute":
      return fail(429, "too many requests; slow down", "rate_limited", { "retry-after": "10" });
    case "day":
      return fail(429, "daily request ceiling reached; resets at 00:00 UTC", "daily_requests", { "retry-after": secondsUntil(tomorrow) });
    case "budget_day":
      return fail(429, "Pro fair-use limit for today reached; resets at 00:00 UTC. Your own API key still works.", "fair_use_daily", {
        "retry-after": secondsUntil(tomorrow),
      });
    case "budget_month":
      return fail(429, "Pro fair-use limit for this month reached; resets on the 1st (UTC). Your own API key still works.", "fair_use_monthly", {
        "retry-after": secondsUntil(nextMonth),
      });
  }
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
  /** Upper bound of this request's provider cost, reserved before forwarding. */
  reserveMicros: number;
  /** Actual cost and token counts from a successful reply; null keeps the reservation. */
  actual?: (reply: string) => { micros: number; inputTokens: number; outputTokens: number } | null;
  signal: AbortSignal;
};

/** Forwards one validated request with the central key, after admission and a spend reservation. */
export async function forward(f: Forward): Promise<Response> {
  const userId = f.caller.userId;
  const admission = await admit(f.kind, userId, f.reserveMicros, {
    ...LIMITS[f.kind],
    leaseSeconds: SLOT_LEASE_SECONDS,
    dayBudgetMicros: FAIR_USE.dailyMicros,
    monthBudgetMicros: FAIR_USE.monthlyMicros,
  });
  if (admission !== "ok") return refused(admission, f.kind);

  const started = performance.now();
  let status = 502;
  let charged = f.reserveMicros;
  let tokens = { inputTokens: 0, outputTokens: 0 };
  const finish = (reply: string | null) =>
    after(async () => {
      if (reply === null) {
        charged = 0; // the provider billed nothing: refund the reservation
      } else {
        const real = f.actual?.(reply);
        if (real) {
          charged = real.micros;
          tokens = { inputTokens: real.inputTokens, outputTokens: real.outputTokens };
        }
      }
      await Promise.allSettled([
        releaseSlot(f.kind, userId),
        settle(userId, charged - f.reserveMicros),
        db.insert(usageEvents).values({
          userId,
          tokenId: f.caller.tokenId,
          kind: f.kind,
          model: f.model,
          status,
          audioBytes: f.audioBytes ?? 0,
          audioMs: f.audioMs ?? 0,
          ...tokens,
          costMicros: charged,
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
    finish(null);
    return fail(504, e instanceof Error && e.name === "TimeoutError" ? "provider timed out" : "provider unreachable");
  }

  status = upstream.status;
  const reply = await readBounded(upstream);
  const timing = { "server-timing": `upstream;dur=${Math.round(performance.now() - started)}` };
  if (!upstream.ok || reply === null) {
    // An oversized success was still billed, so it keeps its reservation.
    finish(upstream.ok ? "" : null);
    if (upstream.status === 401 || upstream.status === 403) {
      console.error(`provider rejected the central key: ${upstream.status}`);
      return fail(502, "provider rejected the request", undefined, timing);
    }
    if (upstream.status === 429) {
      return fail(503, "provider is busy; retry shortly", "provider_busy", { ...timing, "retry-after": upstream.headers.get("retry-after") ?? "2" });
    }
    if (upstream.status >= 500 || reply === null) return fail(502, "provider error", undefined, timing);
    // 400/413/422: the provider's own reason is the useful part, without its name.
    return fail(upstream.status, publicMessage(reply), "invalid_request", timing);
  }

  finish(reply);
  const type = upstream.headers.get("content-type") ?? "application/json";
  return new Response(type.includes("json") ? publicBody(reply) : reply, {
    status: upstream.status,
    headers: { "content-type": type, "cache-control": "no-store", ...timing },
  });
}

/** The reply as text, or null if it exceeds MAX_REPLY_BYTES (replies here are a few KB). */
async function readBounded(res: Response): Promise<string | null> {
  if (!res.body) return "";
  const reader = res.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > MAX_REPLY_BYTES) {
      await reader.cancel();
      return null;
    }
    chunks.push(value);
  }
  return new TextDecoder().decode(Buffer.concat(chunks));
}
