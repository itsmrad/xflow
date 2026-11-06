import { expect, mock, test } from "bun:test";
const work: Promise<unknown>[] = [];
let result = "ok", released = 0, records = 0;
const calls: unknown[] = [];
mock.module("server-only", () => ({}));
mock.module("next/server", () => ({ after: (f: () => Promise<unknown>) => { work.push(Promise.resolve().then(f)); } }));
mock.module("@/db", () => ({ db: { execute: async () => ({rows:[{result}]}), insert: () => ({values: async () => { records++; }}) } }));
mock.module("@/lib/server/limits", () => ({releaseSlot: async () => { released++; }}));
const { streamChat } = await import("../../lib/server/chat");
const caller = {userId:"synthetic-user", tokenId:"synthetic-token", pro:true};
test("stub stream forwards split text/tool frames, strips metadata and releases one slot", async () => {
  const original = globalThis.fetch;
  globalThis.fetch = (async (_url: string | URL | Request, init?: RequestInit) => {
    calls.push(JSON.parse(String(init?.body)));
    return new Response(new ReadableStream({start(c) {
      for (const part of ['data: {"choices":[{"delta":{"content":"hel', 'lo"}}],"x_groq":{"secret":"private"}}\r\n\r\ndata: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","type":"function","function":{"name":"search","arguments":"{}"}}]}}]}\n\ndata: [DONE]\n\n']) c.enqueue(new TextEncoder().encode(part));
      c.close();
    }}));
  }) as unknown as typeof fetch;
  try {
    const response = await streamChat(caller, {messages:[]}, 500, new AbortController().signal);
    const text = await response.text(); await Promise.all(work);
    expect(text).toContain("hello"); expect(text).toContain("search"); expect(text).not.toContain("private");
    expect(released).toBe(1); expect(records).toBe(1);
    result = "ai_budget_month";
    const refused = await streamChat(caller, {}, 500, new AbortController().signal);
    expect(refused.status).toBe(429); expect(await refused.text()).toContain("spend_cap"); expect(calls.length).toBe(1);
    result = "ok";
    globalThis.fetch = (async () => new Response('private provider error', {status:401})) as unknown as typeof fetch;
    const bad = await streamChat(caller, {}, 500, new AbortController().signal);
    expect(bad.status).toBe(502); expect(await bad.text()).not.toContain("private"); await Promise.all(work);
    expect(released).toBe(2);
    globalThis.fetch = (async () => new Response('data: {"error":{"message":"private"}}\n\n')) as unknown as typeof fetch;
    const broken = await streamChat(caller, {}, 500, new AbortController().signal);
    await expect(broken.text()).rejects.toThrow("interrupted"); await Promise.all(work);
    expect(released).toBe(3);
    globalThis.fetch = (async () => new Response(new ReadableStream({start(c) {c.enqueue(new TextEncoder().encode('data: {"choices":[{"delta":{"content":"partial"}}]}\n\n'));}}))) as unknown as typeof fetch;
    const cancelled = await streamChat(caller, {}, 500, new AbortController().signal);
    const reader = cancelled.body!.getReader(); await reader.read(); await reader.cancel(); await Promise.all(work);
    expect(released).toBe(4);
  } finally { globalThis.fetch = original; }
});
