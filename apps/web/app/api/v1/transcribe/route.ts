import { BadRequest, MAX_AUDIO_BYTES, transcribeRequest } from "@/lib/groq";
import { error } from "@/lib/http";
import { audioCostMicros } from "@/lib/pricing";
import { apiCaller, unauthorized } from "@/lib/server/api-auth";
import { forward, gate } from "@/lib/server/upstream";

export const maxDuration = 60;

/** Same multipart body as Groq `audio/transcriptions`; answered with Groq's response. */
export async function POST(request: Request) {
  const length = Number(request.headers.get("content-length") ?? 0);
  if (length > MAX_AUDIO_BYTES + 64 * 1024) return error(413, `audio exceeds ${MAX_AUDIO_BYTES} bytes`);
  const caller = await apiCaller(request);
  if (!caller) return unauthorized();
  const denied = gate(caller);
  if (denied) return denied;

  let parsed;
  try {
    parsed = await transcribeRequest(await request.formData());
  } catch (e) {
    if (e instanceof BadRequest) return error(e.status, e.message);
    return error(400, "body must be multipart/form-data");
  }
  return forward({
    kind: "transcribe",
    caller,
    model: parsed.model,
    path: "/audio/transcriptions",
    body: parsed.form,
    audioBytes: parsed.audioBytes,
    audioMs: parsed.audioMs,
    // Duration is exact from the header, so the reservation is the final charge.
    reserveMicros: audioCostMicros(parsed.model, parsed.audioMs),
    signal: request.signal,
  });
}
