import { error, fields, json, sameOrigin, seeOther, wantsHtml } from "@/lib/http";
import { decideDevice } from "@/lib/server/device";
import { withinLimit } from "@/lib/server/limits";
import { sessionUserId } from "@/lib/server/users";
import { normalizeUserCode } from "@/lib/tokens";

/** The signed-in user approves or denies a CLI sign-in. */
export async function POST(request: Request) {
  if (!sameOrigin(request)) return error(403, "cross-origin request refused");
  const userId = await sessionUserId();
  if (!userId) return error(401, "sign in first");
  if (!(await withinLimit(`device-decide:${userId}`, 20, 600))) return error(429, "too many attempts; wait a few minutes");

  const body = await fields(request);
  const code = typeof body.user_code === "string" ? normalizeUserCode(body.user_code) : null;
  const decision = body.decision;
  if (!code || (decision !== "approve" && decision !== "deny")) return error(400, "user_code and decision (approve|deny) are required");

  const ok = await decideDevice(code, userId, decision === "approve");
  if (wantsHtml(request)) return seeOther(`/device?${ok ? `done=${decision}` : "error=expired"}`);
  return ok ? json({ ok: true, decision }) : error(404, "code not found, expired or already used");
}
