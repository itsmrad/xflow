import { BadRequest, chatUsage, cleanupRequest, MAX_CLEANUP_BODY_BYTES } from "@/lib/groq";
import { estimateInputTokens, tokenCostMicros } from "@/lib/pricing";
import { error } from "@/lib/http";
import { apiCaller, unauthorized } from "@/lib/server/api-auth";
import { forward, gate } from "@/lib/server/upstream";

export const maxDuration = 60;

/** Same JSON body as Groq `chat/completions` (non-streaming); answered with Groq's response. */
export async function POST(request: Request) {
  if (Number(request.headers.get("content-length") ?? 0) > MAX_CLEANUP_BODY_BYTES) return error(413, "body too large");
  const caller = await apiCaller(request);
  if (!caller) return unauthorized();
  const denied = gate(caller);
  if (denied) return denied;

  const raw = await request.text();
  if (new TextEncoder().encode(raw).length > MAX_CLEANUP_BODY_BYTES) return error(413, "body too large");
  let parsed;
  try {
    parsed = cleanupRequest(JSON.parse(raw));
  } catch (e) {
    if (e instanceof BadRequest) return error(e.status, e.message);
    return error(400, "body must be JSON");
  }
  return forward({
    kind: "cleanup",
    caller,
    model: parsed.model,
    path: "/chat/completions",
    body: JSON.stringify(parsed.body),
    contentType: "application/json",
    // Worst case up front (estimated input + the full output cap), settled to the reported usage.
    reserveMicros: tokenCostMicros(parsed.model, estimateInputTokens(parsed.messages), parsed.maxCompletionTokens),
    actual: (reply) => {
      const usage = chatUsage(reply);
      return usage && { micros: tokenCostMicros(parsed.model, usage.inputTokens, usage.outputTokens), ...usage };
    },
    signal: request.signal,
  });
}
