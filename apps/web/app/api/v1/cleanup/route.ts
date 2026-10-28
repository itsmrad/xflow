import { BadRequest, cleanupRequest, MAX_CLEANUP_BODY_BYTES } from "@/lib/groq";
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
    return error(400, e instanceof BadRequest ? e.message : "body must be JSON");
  }
  return forward({
    kind: "cleanup",
    caller,
    model: parsed.model,
    path: "/chat/completions",
    body: JSON.stringify(parsed.body),
    contentType: "application/json",
    signal: request.signal,
  });
}
