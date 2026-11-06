import { BadRequest } from "./groq";
import { tokenCostMicros } from "./pricing";

export const CHAT_MODEL = "openai/gpt-oss-120b";
export const CHAT_MONTH_MICROS = 1_500_000;
export const CHAT_DAY_MICROS = 250_000;
export const CHAT_BODY_BYTES = 128 * 1024;

/** A byte per token plus framing is deliberately conservative, including tool schemas/results. */
export function chatRequest(input: unknown) {
  if (!input || typeof input !== "object" || Array.isArray(input)) throw new BadRequest("body must be an object");
  const raw = input as Record<string, unknown>;
  const allowed = ["model", "stream", "stream_options", "messages", "tools", "max_tokens", "max_completion_tokens"];
  if (Object.keys(raw).some(k => !allowed.includes(k))) throw new BadRequest("unsupported chat field");
  if (raw.stream !== true) throw new BadRequest("stream must be true");
  if (!Array.isArray(raw.messages) || !raw.messages.length || raw.messages.length > 64) throw new BadRequest("messages must hold 1–64 entries");
  // Validate shape, then copy only the wire fields. Never let a caller change provider routing.
  const messages = raw.messages.map(m => {
    if (!m || typeof m !== "object" || !["system", "user", "assistant", "tool"].includes(m.role) || typeof m.content !== "string") throw new BadRequest("invalid message");
    const out: Record<string, unknown> = { role: m.role, content: m.content };
    if (m.role === "tool") {
      if (typeof m.tool_call_id !== "string") throw new BadRequest("tool result needs tool_call_id");
      out.tool_call_id = m.tool_call_id;
    }
    if (m.role === "assistant" && m.tool_calls !== undefined) {
      if (!Array.isArray(m.tool_calls) || m.tool_calls.length > 8) throw new BadRequest("invalid tool calls");
      out.tool_calls = m.tool_calls.map((c: Record<string, unknown>) => {
        const f = c?.function as Record<string, unknown> | undefined;
        if (c?.type !== "function" || typeof c.id !== "string" || typeof f?.name !== "string" || typeof f.arguments !== "string") throw new BadRequest("invalid tool call");
        return { id: c.id, type: "function", function: { name: f.name, arguments: f.arguments } };
      });
    }
    return out;
  });
  let tools;
  if (raw.tools !== undefined) {
    if (!Array.isArray(raw.tools) || raw.tools.length > 8) throw new BadRequest("invalid tools");
    tools = raw.tools.map(t => {
      const f = t?.function;
      if (t?.type !== "function" || typeof f?.name !== "string" || typeof f.description !== "string" || !f.parameters || typeof f.parameters !== "object" || Array.isArray(f.parameters)) throw new BadRequest("invalid tool");
      return { type: "function", function: { name: f.name, description: f.description, parameters: f.parameters } };
    });
  }
  const requested = raw.max_completion_tokens ?? raw.max_tokens ?? 2048;
  if (typeof requested !== "number" || !Number.isInteger(requested) || requested < 1) throw new BadRequest("invalid output limit");
  const output = Math.min(requested, 2048);
  const inputTokens = new TextEncoder().encode(JSON.stringify({ messages, tools })).length + messages.length * 32 + 256;
  if (inputTokens > 32_000) throw new BadRequest("chat context exceeds the conservative 32k token bound", 413);
  return {
    body: { model: CHAT_MODEL, stream: true, stream_options: { include_usage: true }, messages, ...(tools ? { tools } : {}), max_completion_tokens: output },
    reserveMicros: tokenCostMicros(CHAT_MODEL, inputTokens, output),
  };
}

/** Whitelist SSE fields; provider errors and metadata may contain private request details. */
export function publicChatFrame(data: string): string {
  if (data === "[DONE]") return data;
  const v = JSON.parse(data);
  if (v.error) throw new Error("provider stream error");
  const choices = (Array.isArray(v.choices) ? v.choices : []).map((c: { index?: number; delta?: { content?: string; tool_calls?: unknown }; finish_reason?: string }) => ({
    index: c.index ?? 0,
    delta: { ...(typeof c.delta?.content === "string" ? { content: c.delta.content } : {}), ...(Array.isArray(c.delta?.tool_calls) ? { tool_calls: c.delta.tool_calls.map((t: { index?: number; id?: string; type?: string; function?: { name?: string; arguments?: string } }) => ({ index: t.index, id: t.id, type: t.type, function: { name: t.function?.name, arguments: t.function?.arguments } })) } : {}) },
    finish_reason: c.finish_reason ?? null,
  }));
  const count = (n: unknown) => typeof n === "number" && Number.isSafeInteger(n) && n >= 0 ? n : 0;
  return JSON.stringify({ choices, ...(v.usage ? { usage: { prompt_tokens: count(v.usage.prompt_tokens), completion_tokens: count(v.usage.completion_tokens) } } : {}) });
}
