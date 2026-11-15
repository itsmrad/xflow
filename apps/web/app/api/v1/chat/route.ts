import { CHAT_BODY_BYTES, chatRequest } from "@/lib/chat";
import { BadRequest } from "@/lib/groq";
import { error, refuse } from "@/lib/http";
import { apiCaller, unauthorized } from "@/lib/server/api-auth";
import { gate } from "@/lib/server/upstream";
import { streamChat } from "@/lib/server/chat";

export const maxDuration = 60;

export async function POST(request: Request) {
  const caller = await apiCaller(request);
  if (!caller) return refuse(request, unauthorized(), CHAT_BODY_BYTES);
  const denied = gate(caller);
  if (denied) return refuse(request, denied, CHAT_BODY_BYTES);
  const reader = request.body?.getReader();
  if (!reader) return error(400, "body required");
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.length;
    if (size > CHAT_BODY_BYTES) { await reader.cancel(); return error(413, "body too large"); }
    chunks.push(value);
  }
  try {
    const parsed = chatRequest(JSON.parse(new TextDecoder().decode(Buffer.concat(chunks))));
    return streamChat(caller, parsed.body, parsed.reserveMicros, request.signal);
  } catch (e) {
    return error(e instanceof BadRequest ? e.status : 400, e instanceof BadRequest ? e.message : "body must be JSON");
  }
}
