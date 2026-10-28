// End-to-end check of a running app against the real Clerk, Neon (Postgres, Realtime, Object
// Storage) and a Groq stub (or real Groq). Creates a throwaway Clerk user and deletes it after.
//
//   bun --conditions react-server --env-file=.env.local scripts/e2e.ts
//
// The app must run with DODO_PAYMENTS_WEBHOOK_SECRET=$E2E_WEBHOOK_SECRET, DODO_PRO_PRODUCT_ID=pdt_test_pro
// (the fixture's product) and, for the stub,
// GROQ_BASE_URL=http://127.0.0.1:3901 GROQ_API_KEY=$E2E_STUB_KEY.
import { createRealtimeClient, type SealedLiveQuery } from "@neon/realtime/client";
import { AwsClient } from "aws4fetch";
import { Pool } from "pg";
import { Webhook } from "standardwebhooks";
import fixture from "../test/fixtures/subscription.active.json";

const BASE = process.env.E2E_BASE_URL ?? "http://localhost:3000";
const CLERK = "https://api.clerk.com/v1";
const clerkKey = process.env.CLERK_SECRET_KEY!;
const webhookSecret = process.env.E2E_WEBHOOK_SECRET!;
const stubKey = process.env.E2E_STUB_KEY;
const audioPath = process.env.E2E_WAV!;
const pool = new Pool({ connectionString: process.env.DATABASE_URL_UNPOOLED!.replace("sslmode=require", "sslmode=verify-full") });

let failures = 0;
const check = (name: string, ok: boolean, detail = "") => {
  if (!ok) failures++;
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
};
const ms = (t: number) => `${Math.round(performance.now() - t)} ms`;

// --- Groq stub: records exactly what the proxy forwards ---------------------------------------
const seen: { path: string; auth: string | null; fields?: string[]; format?: string; json?: unknown }[] = [];
const stub = stubKey
  ? Bun.serve({
      port: 3901,
      hostname: "127.0.0.1",
      async fetch(req) {
        const path = new URL(req.url).pathname;
        const auth = req.headers.get("authorization");
        if (auth !== `Bearer ${stubKey}`) return Response.json({ error: { message: "bad key" } }, { status: 401 });
        if (path.endsWith("/audio/transcriptions")) {
          const form = await req.formData();
          const file = form.get("file") as File;
          const format = String(form.get("response_format"));
          seen.push({ path, auth, fields: [...form.keys()].sort(), format });
          // Like the provider: the billed duration is the decoded length; "STUB_LONG" plays a
          // provider that decodes far more audio than the upload's size suggests.
          const duration = String(form.get("prompt")).includes("STUB_LONG") ? 600 : (file.size - 44) / 32_000;
          const text = `stub transcript of ${file.size} bytes (${file.name}, ${file.type})`;
          return Response.json(format === "verbose_json" ? { task: "transcribe", language: "english", duration, text, segments: [], x_groq: { id: "req_stub" } } : { text, x_groq: { id: "req_stub" } });
        }
        const json = (await req.json()) as { messages: { content: string }[] };
        seen.push({ path, auth, json });
        const said = json.messages.map((m) => m.content).join(" ");
        if (said.includes("STUB_SLOW")) await Bun.sleep(1500); // the client hangs up before this answers
        if (said.includes("STUB_DRIP")) {
          // Headers now, body later: the client hangs up while the reply is arriving.
          const body = new ReadableStream({
            async start(c) {
              c.enqueue(new TextEncoder().encode('{"choices":[{"index":0,"message":{"role":"assistant","content":"'));
              await Bun.sleep(1500);
              c.enqueue(new TextEncoder().encode('late"}}],"usage":{"prompt_tokens":42,"completion_tokens":7}}'));
              c.close();
            },
          });
          return new Response(body, { headers: { "content-type": "application/json" } });
        }
        await Bun.sleep(400); // long enough for concurrent requests to overlap
        return Response.json({
          choices: [{ index: 0, message: { role: "assistant", content: "So, deploy the thing." }, finish_reason: "stop" }],
          usage: { prompt_tokens: 42, completion_tokens: 7, total_tokens: 49 },
        });
      },
    })
  : null;

async function clerk(path: string, init: RequestInit = {}) {
  const res = await fetch(`${CLERK}${path}`, {
    ...init,
    headers: { authorization: `Bearer ${clerkKey}`, "content-type": "application/json", ...init.headers },
  });
  if (!res.ok) throw new Error(`clerk ${path}: ${res.status} ${await res.text()}`);
  return res.json() as Promise<Record<string, unknown>>;
}

let clerkUserId = "";
let sessionId = "";
// Clerk session JWTs live 60 s; mint one per signed-in request.
const sessionJwt = async () => (await clerk(`/sessions/${sessionId}/tokens`, { method: "POST" })).jwt as string;
const asUser = async (path: string, init: RequestInit = {}) =>
  fetch(`${BASE}${path}`, { redirect: "manual", ...init, headers: { authorization: `Bearer ${await sessionJwt()}`, ...init.headers } });

try {
  // --- Clerk ----------------------------------------------------------------------------------
  const email = `xflow-e2e-${Date.now()}+clerk_test@example.com`;
  clerkUserId = (await clerk("/users", { method: "POST", body: JSON.stringify({ email_address: [email], skip_password_requirement: true }) })).id as string;
  sessionId = (await clerk("/sessions", { method: "POST", body: JSON.stringify({ user_id: clerkUserId }) })).id as string;
  check("clerk: test user + session created", !!clerkUserId && !!sessionId);

  const anonDash = await fetch(`${BASE}/dashboard`, { redirect: "manual", headers: { accept: "text/html" } });
  check("clerk: /dashboard redirects signed-out visitors", anonDash.status === 307 || anonDash.status === 302 || anonDash.status === 404, `${anonDash.status}`);
  const anonApi = await fetch(`${BASE}/api/v1/usage/live`, { method: "POST" });
  check("clerk: session API refuses signed-out callers", anonApi.status === 401, `${anonApi.status}`);
  const dash = await asUser("/dashboard");
  check("clerk: /dashboard renders for a signed-in user", dash.status === 200 && (await dash.text()).includes(email), `${dash.status}`);
  const { rows: [user] } = await pool.query<{ id: string }>("SELECT id FROM users WHERE clerk_user_id = $1", [clerkUserId]);
  check("neon: users row created on first visit", !!user);
  const userId = user!.id;

  // --- Device flow ----------------------------------------------------------------------------
  let t = performance.now();
  const code = (await (await fetch(`${BASE}/api/v1/device/code`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_name: "e2e.local" }),
  })).json()) as Record<string, string | number>;
  check("device: code issued", /^[A-Z]{4}-[A-Z]{4}$/.test(String(code.user_code)) && code.expires_in === 600 && code.interval === 5, `${code.user_code}, ${ms(t)}`);
  const poll = async () => {
    const r = await fetch(`${BASE}/api/v1/device/token`, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({ grant_type: "urn:ietf:params:oauth:grant-type:device_code", device_code: String(code.device_code) }),
    });
    return { status: r.status, body: (await r.json()) as Record<string, string | number> };
  };
  const p1 = await poll();
  check("device: authorization_pending before approval", p1.body.error === "authorization_pending", JSON.stringify(p1.body));
  const p2 = await poll();
  check("device: fast poll gets slow_down and a longer interval", p2.body.error === "slow_down" && p2.body.interval === 10, JSON.stringify(p2.body));
  const page = await asUser(`/device?code=${String(code.user_code).toLowerCase().replace("-", "")}`);
  check("device: /device shows the pending request", page.status === 200 && (await page.text()).includes("e2e.local"), `${page.status}`);
  const approve = await asUser("/api/v1/device/decide", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ user_code: code.user_code, decision: "approve" }),
  });
  check("device: user approves", approve.status === 200, `${approve.status}`);
  const crossSite = await asUser("/api/v1/device/decide", {
    method: "POST",
    headers: { origin: "https://evil.example", "content-type": "application/json" },
    body: JSON.stringify({ user_code: code.user_code, decision: "approve" }),
  });
  check("device: cross-origin approval refused", crossSite.status === 403, `${crossSite.status}`);
  await Bun.sleep(10_500);
  const p3 = await poll();
  const token = String(p3.body.access_token ?? "");
  check("device: token issued after approval", p3.status === 200 && /^xf_[A-Za-z0-9_-]{43}$/.test(token) && p3.body.token_type === "Bearer");
  const p4 = await poll();
  check("device: code can't be redeemed twice", p4.body.error === "invalid_grant" || p4.body.error === "slow_down", JSON.stringify(p4.body));
  const { rows: tok } = await pool.query("SELECT token_hash, prefix FROM api_tokens WHERE user_id = $1", [userId]);
  check("neon: only the SHA-256 hash is stored", tok.length === 1 && tok[0].token_hash === new Bun.CryptoHasher("sha256").update(token).digest("hex") && !JSON.stringify(tok).includes(token));
  const bearer = { authorization: `Bearer ${token}` };

  // --- Entitlement before payment -------------------------------------------------------------
  const me0 = (await (await fetch(`${BASE}/api/v1/me`, { headers: bearer })).json()) as { pro: boolean };
  check("me: free user is not Pro", me0.pro === false);
  const wavBytes = await Bun.file(audioPath).bytes();
  const audioForm = () => {
    // Exactly the fields apps/cli/crates/xflow-providers/src/stt.rs sends.
    const f = new FormData();
    f.set("model", "whisper-large-v3-turbo");
    f.set("file", new File([wavBytes], "dictation.wav", { type: "audio/wav" }));
    f.set("response_format", "json");
    f.set("language", "en");
    f.set("prompt", "xflow, xflowd, kubectl");
    return f;
  };
  const freeTx = await fetch(`${BASE}/api/v1/transcribe`, { method: "POST", headers: bearer, body: audioForm() });
  check("proxy: free user gets 402", freeTx.status === 402, `${freeTx.status}`);
  const noTok = await fetch(`${BASE}/api/v1/transcribe`, { method: "POST", body: audioForm() });
  check("proxy: no token gets 401", noTok.status === 401, `${noTok.status}`);

  // --- Dodo webhook ---------------------------------------------------------------------------
  const deliver = async (event: unknown, secret = webhookSecret) => {
    const body = JSON.stringify(event);
    const id = `msg_${crypto.randomUUID()}`;
    const now = new Date();
    return fetch(`${BASE}/api/v1/webhooks/dodo`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "webhook-id": id,
        "webhook-timestamp": String(Math.floor(now.getTime() / 1000)),
        "webhook-signature": new Webhook(secret).sign(id, now, body),
      },
      body,
    });
  };
  const subId = `sub_e2e_${Date.now()}`;
  const event = (type: string, status: string, timestamp: string) => ({
    ...fixture,
    type,
    timestamp,
    data: {
      ...fixture.data,
      status,
      subscription_id: subId,
      customer: { ...fixture.data.customer, customer_id: `cus_e2e_${userId.slice(0, 8)}` },
      next_billing_date: new Date(Date.now() + 30 * 86_400_000).toISOString(),
      metadata: { xflow_user_id: userId },
    },
  });
  // Review M1: the Dodo customer is still linked to an older account with the same email.
  const { rows: [oldAccount] } = await pool.query<{ id: string }>(
    "INSERT INTO users (clerk_user_id, dodo_customer_id) VALUES ($1, $2) RETURNING id",
    [`${clerkUserId}-old`, `cus_e2e_${userId.slice(0, 8)}`],
  );
  // Review S1: another product's active subscription must not grant Pro.
  const other = await deliver({ ...event("subscription.active", "active", new Date().toISOString()), data: { ...event("subscription.active", "active", new Date().toISOString()).data, subscription_id: `${subId}_other`, product_id: "pdt_not_pro" } });
  const meOther = (await (await fetch(`${BASE}/api/v1/me`, { headers: bearer })).json()) as { pro: boolean };
  check("webhook: another product's subscription doesn't grant Pro", other.status === 200 && meOther.pro === false);
  const { rows: [moved] } = await pool.query("SELECT (SELECT dodo_customer_id FROM users WHERE id = $1) AS payer, (SELECT dodo_customer_id FROM users WHERE id = $2) AS old", [userId, oldAccount!.id]);
  check("webhook: customer held by an older account moves to the payer (M1)", moved.payer === `cus_e2e_${userId.slice(0, 8)}` && moved.old === null);
  await pool.query("DELETE FROM users WHERE id = $1", [oldAccount!.id]);
  const forged = await deliver(event("subscription.active", "active", new Date().toISOString()), `whsec_${btoa("not-the-secret-000000000")}`);
  check("webhook: bad signature rejected", forged.status === 401, `${forged.status}`);
  const active = await deliver(event("subscription.active", "active", new Date().toISOString()));
  check("webhook: subscription.active applied", active.status === 200 && ((await active.json()) as { result: string }).result === "applied", `${active.status}`);
  const stale = await deliver(event("subscription.expired", "expired", new Date(Date.now() - 3_600_000).toISOString()));
  check("webhook: older out-of-order event ignored", ((await stale.json()) as { result: string }).result === "stale");
  const me1 = (await (await fetch(`${BASE}/api/v1/me`, { headers: bearer })).json()) as { pro: boolean };
  check("me: Pro after webhook", me1.pro === true);

  // --- Realtime: subscribe through the browser path before generating usage -------------------
  const since = new Date(Date.UTC(new Date().getUTCFullYear(), new Date().getUTCMonth(), new Date().getUTCDate())).toISOString();
  const sealedRes = await asUser("/api/v1/usage/live", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ userId, since }) });
  const sealed = (await sealedRes.json()) as SealedLiveQuery<{ kind: string; requests: number }>;
  check("realtime: sealed usage query issued", sealedRes.status === 200 && typeof sealed.capability === "string");
  const rt = createRealtimeClient({ url: process.env.NEON_REALTIME_URL! });
  const sub = await rt.subscribe(sealed);
  await sub.awaitRows(() => true, 15_000);
  check("realtime: initial snapshot received (no usage yet)", (sub.getSnapshot().data ?? []).length === 0);

  // --- Proxy: transcribe + cleanup ------------------------------------------------------------
  t = performance.now();
  const tx = await fetch(`${BASE}/api/v1/transcribe`, { method: "POST", headers: bearer, body: audioForm() });
  const txBody = await tx.text();
  check("proxy: transcribe forwarded", tx.status === 200 && txBody.includes('"text"'), `${ms(t)}, ${tx.headers.get("server-timing")}: ${txBody.slice(0, 120)}`);
  check("proxy: reply doesn't name the provider", !txBody.includes("x_groq") && !/groq/i.test(txBody));
  if (stub) {
    const last = seen.findLast((s) => s.path.endsWith("/audio/transcriptions"));
    check("proxy: Groq receives the Rust provider's exact fields + central key", JSON.stringify(last?.fields) === JSON.stringify(["file", "language", "model", "prompt", "response_format"]) && last?.auth === `Bearer ${stubKey}`, JSON.stringify(last?.fields));
    check("proxy: upstream asked for verbose_json, client got {text} only (N2)", last?.format === "verbose_json" && JSON.stringify(Object.keys(JSON.parse(txBody))) === '["text"]', txBody.slice(0, 80));
  }
  // Warm proxy overhead: total time minus the upstream's own time (server-timing).
  const over: number[] = [];
  for (let i = 0; i < 5; i++) {
    const t0 = performance.now();
    const r = await fetch(`${BASE}/api/v1/transcribe`, { method: "POST", headers: bearer, body: audioForm() });
    await r.text();
    const upstreamMs = Number(r.headers.get("server-timing")?.match(/dur=(\d+)/)?.[1] ?? 0);
    over.push(performance.now() - t0 - upstreamMs);
  }
  over.sort((a, b) => a - b);
  console.log(`INFO  warm proxy overhead per transcribe (incl. 2 DB round trips to Neon): p50 ${Math.round(over[2]!)} ms, min ${Math.round(over[0]!)} ms`);
  const cleanupBody = {
    model: "qwen/qwen3.8-27b",
    stream: false,
    messages: [
      { role: "system", content: "Correct punctuation and capitalization, remove filler words." },
      { role: "user", content: "um so like deploy the thing" },
    ],
    reasoning_effort: "none",
  };
  t = performance.now();
  const cu = await fetch(`${BASE}/api/v1/cleanup`, { method: "POST", headers: { ...bearer, "content-type": "application/json" }, body: JSON.stringify(cleanupBody) });
  const cuBody = (await cu.json()) as { choices?: { message: { content: string } }[] };
  check("proxy: cleanup forwarded", cu.status === 200 && !!cuBody.choices?.[0]?.message.content, `${cu.status}, ${ms(t)}: ${cuBody.choices?.[0]?.message.content ?? JSON.stringify(cuBody).slice(0, 200)}`);
  if (stub) {
    check("proxy: Groq receives the Rust chat body plus the output cap", JSON.stringify(seen.findLast((s) => s.json)?.json) === JSON.stringify({ ...cleanupBody, max_completion_tokens: 512 }));
  }

  await sub.awaitRows((rows) => rows.some((r) => r.kind === "cleanup" && r.requests >= 1) && rows.some((r) => r.kind === "transcribe" && r.requests >= 6), 20_000).then(
    () => check("realtime: live rows updated after the requests", true, JSON.stringify(sub.getSnapshot().data)),
    () => check("realtime: live rows updated after the requests", false, JSON.stringify(sub.getSnapshot().data)),
  );
  sub.unsubscribe();
  rt.close();

  const { rows: usage } = await pool.query("SELECT kind, model, status, audio_bytes, audio_ms, input_tokens, output_tokens, latency_ms FROM usage_events WHERE user_id = $1 ORDER BY created_at", [userId]);
  check("neon: usage_events recorded", usage.length >= 2 && usage.some((u) => u.kind === "transcribe" && u.audio_ms > 0) && usage.some((u) => u.kind === "cleanup"), JSON.stringify(usage));

  // --- Limits ---------------------------------------------------------------------------------
  const bad = await fetch(`${BASE}/api/v1/cleanup`, { method: "POST", headers: { ...bearer, "content-type": "application/json" }, body: JSON.stringify({ ...cleanupBody, model: "gpt-5" }) });
  check("proxy: unknown model rejected", bad.status === 400, `${bad.status}`);
  const greedy = await fetch(`${BASE}/api/v1/cleanup`, { method: "POST", headers: { ...bearer, "content-type": "application/json" }, body: JSON.stringify({ ...cleanupBody, max_completion_tokens: 32768 }) });
  check("proxy: output token cap enforced (M2)", greedy.status === 400, `${greedy.status}`);
  const big = await fetch(`${BASE}/api/v1/cleanup`, { method: "POST", headers: { ...bearer, "content-type": "application/json" }, body: JSON.stringify({ ...cleanupBody, messages: [{ role: "user", content: "x".repeat(400_000) }] }) });
  check("proxy: oversized body rejected", big.status === 413, `${big.status}`);
  if (stub) {
    const burst = await Promise.all(
      [0, 1, 2].map(() => fetch(`${BASE}/api/v1/cleanup`, { method: "POST", headers: { ...bearer, "content-type": "application/json" }, body: JSON.stringify(cleanupBody) }).then((r) => r.status)),
    );
    check("proxy: per-user concurrency capped at 2", burst.filter((s) => s === 429).length === 1 && burst.filter((s) => s === 200).length === 2, JSON.stringify(burst));
  }

  if (stub) {
    const spendNow = async () => Number((await pool.query("SELECT count FROM rate_limits WHERE key = $1", [`spend:${userId}:day`])).rows[0]?.count ?? 0);
    const lastRow = async (kind: string) =>
      (await pool.query("SELECT status, cost_micros, audio_ms FROM usage_events WHERE user_id = $1 AND kind = $2 ORDER BY created_at DESC LIMIT 1", [userId, kind])).rows[0];
    const cleanupWith = (marker: string, signal?: AbortSignal) =>
      fetch(`${BASE}/api/v1/cleanup`, {
        method: "POST",
        headers: { ...bearer, "content-type": "application/json" },
        body: JSON.stringify({ ...cleanupBody, messages: [cleanupBody.messages[0], { role: "user", content: `um ${marker} deploy` }] }),
        signal,
      });

    // Review N1: hanging up after the upload must not refund what the provider still bills.
    await Bun.sleep(1500); // let earlier requests' reservations settle first
    let before = await spendNow();
    await cleanupWith("STUB_SLOW", AbortSignal.timeout(400)).catch(() => null);
    await Bun.sleep(2500);
    const hung = await lastRow("cleanup");
    check("N1: client hang-up keeps the reservation", hung?.status === 499 && hung.cost_micros > 0 && (await spendNow()) - before === hung.cost_micros, JSON.stringify(hung));

    // Review N3: hanging up while the provider's reply is arriving still records usage and frees the slot.
    before = await spendNow();
    await cleanupWith("STUB_DRIP", AbortSignal.timeout(500)).catch(() => null);
    await Bun.sleep(2500);
    const dripRow = await lastRow("cleanup");
    const both = await Promise.all([cleanupWith("ok"), cleanupWith("ok")]).then((rs) => rs.map((r) => r.status));
    check("N3: hang-up mid-reply records usage, keeps the charge, frees the slot", dripRow?.status === 499 && dripRow.cost_micros > 0 && (await spendNow()) - before >= dripRow.cost_micros && JSON.stringify(both) === "[200,200]", `${JSON.stringify(dripRow)} then ${JSON.stringify(both)}`);
    await Bun.sleep(1500);

    // Review N2: the provider's billed duration settles the charge when it exceeds the upload's.
    const longForm = audioForm();
    longForm.set("prompt", "STUB_LONG");
    const long = await fetch(`${BASE}/api/v1/transcribe`, { method: "POST", headers: bearer, body: longForm });
    await long.text();
    await Bun.sleep(1500);
    const longRow = await lastRow("transcribe");
    check("N2: settles to the provider's billed duration", long.status === 200 && longRow?.audio_ms === 600_000 && longRow.cost_micros === Math.ceil((600_000 / 3_600_000) * 40_000), JSON.stringify(longRow));

    const forged = new Uint8Array(wavBytes);
    new DataView(forged.buffer).setUint32(28, 32_000 * 20, true); // byteRate x20
    const forgedForm = audioForm();
    forgedForm.set("file", new File([forged], "dictation.wav", { type: "audio/wav" }));
    const forgedRes = await fetch(`${BASE}/api/v1/transcribe`, { method: "POST", headers: bearer, body: forgedForm });
    check("N2: a forged WAV header is refused", forgedRes.status === 400, `${forgedRes.status} ${await forgedRes.text()}`);
    const flacForm = audioForm();
    flacForm.set("file", new File([new Uint8Array(4096)], "dictation.flac", { type: "audio/flac" }));
    const flacRes = await fetch(`${BASE}/api/v1/transcribe`, { method: "POST", headers: bearer, body: flacForm });
    check("N2: FLAC is refused on the hosted path", flacRes.status === 400, `${flacRes.status}`);
  }

  // Review M2: once the day's spend budget is used up, Pro requests stop with a clear reason.
  await Bun.sleep(1500); // reservations settle after each response is sent
  const { rows: [spent] } = await pool.query("SELECT count FROM rate_limits WHERE key = $1", [`spend:${userId}:day`]);
  const { rows: costs } = await pool.query("SELECT kind, count(*)::int AS n, sum(cost_micros)::int AS micros FROM usage_events WHERE user_id = $1 GROUP BY kind ORDER BY kind", [userId]);
  const recorded = costs.reduce((n, c) => n + c.micros, 0);
  check("fair use: reservations settle to the recorded cost exactly", spent?.count === recorded && recorded > 0, `day spend ${spent?.count} µ$ = Σ ${JSON.stringify(costs)}`);
  await pool.query("UPDATE rate_limits SET count = 400000 WHERE key = $1", [`spend:${userId}:day`]);
  const overBudget = await fetch(`${BASE}/api/v1/transcribe`, { method: "POST", headers: bearer, body: audioForm() });
  const overBody = (await overBudget.json()) as { error: { code: string; message: string } };
  check("fair use: daily budget exhausted → 429 fair_use_daily with retry-after", overBudget.status === 429 && overBody.error.code === "fair_use_daily" && Number(overBudget.headers.get("retry-after")) > 0, overBody.error.message);
  await pool.query("UPDATE rate_limits SET count = 0 WHERE key = $1", [`spend:${userId}:day`]);

  // --- Object storage -------------------------------------------------------------------------
  const aws = new AwsClient({ accessKeyId: process.env.AWS_ACCESS_KEY_ID!, secretAccessKey: process.env.AWS_SECRET_ACCESS_KEY!, region: process.env.AWS_REGION!, service: "s3" });
  const key = `e2e/${Date.now()}.txt`;
  const objectUrl = `${process.env.AWS_ENDPOINT_URL_S3!.replace(/\/+$/, "")}/releases/${key}`;
  const put = await aws.fetch(objectUrl, { method: "PUT", body: "xflow release e2e", headers: { "content-type": "text/plain" } });
  check("storage: test object uploaded to releases bucket", put.ok, `${put.status}`);
  const redirect = await fetch(`${BASE}/api/v1/releases/${key}`, { redirect: "manual" });
  const location = redirect.headers.get("location") ?? "";
  const download = location ? await fetch(location) : null;
  check("storage: download redirects to a presigned URL that works", redirect.status === 302 && location.includes("X-Amz-Signature") && (await download?.text()) === "xflow release e2e", `${redirect.status}`);
  const traversal = await fetch(`${BASE}/api/v1/releases/..%2F..%2Fetc`, { redirect: "manual" });
  check("storage: path traversal refused", traversal.status === 404, `${traversal.status}`);
  await aws.fetch(objectUrl, { method: "DELETE" });

  // --- Revocation -----------------------------------------------------------------------------
  const revoke = await asUser(`/api/v1/tokens/${(await pool.query("SELECT id FROM api_tokens WHERE user_id = $1", [userId])).rows[0].id}/revoke`, { method: "POST" });
  const meAfter = await fetch(`${BASE}/api/v1/me`, { headers: bearer });
  check("tokens: revoked token stops working", revoke.status === 200 && meAfter.status === 401, `${revoke.status}/${meAfter.status}`);

  // --- Billing endpoints without Dodo keys ----------------------------------------------------
  const checkout = await asUser("/api/v1/billing/checkout", { method: "POST" });
  check("billing: checkout reports 'not configured' or 'already Pro' without Dodo keys", checkout.status === 503 || checkout.status === 409, `${checkout.status}`);
} catch (e) {
  failures++;
  console.log("FAIL  unexpected error:", e instanceof Error ? e.message : e);
} finally {
  if (clerkUserId) {
    const { rows } = await pool.query<{ id: string }>("DELETE FROM users WHERE clerk_user_id = $1 RETURNING id", [clerkUserId]);
    for (const r of rows) await pool.query("DELETE FROM rate_limits WHERE key LIKE $1", [`%${r.id}%`]);
    await clerk(`/users/${clerkUserId}`, { method: "DELETE" }).catch(() => {});
    console.log("cleanup: test user removed from Clerk and Neon");
  }
  await pool.end();
  stub?.stop(true);
  console.log(failures ? `\n${failures} check(s) failed` : "\nall checks passed");
  process.exit(failures ? 1 : 0);
}
