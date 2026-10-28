import { json } from "@/lib/http";
import { apiCaller, unauthorized } from "@/lib/server/api-auth";

/** Lets the CLI check its token and whether hosted transcription is available. */
export async function GET(request: Request) {
  const caller = await apiCaller(request);
  if (!caller) return unauthorized();
  return json({ pro: caller.pro });
}
