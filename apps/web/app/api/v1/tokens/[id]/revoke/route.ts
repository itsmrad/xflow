import { error, json, sameOrigin, seeOther, wantsHtml } from "@/lib/http";
import { revokeToken } from "@/lib/server/account";
import { sessionUserId } from "@/lib/server/users";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Revokes one of the signed-in user's CLI tokens; it stops working on its next request. */
export async function POST(request: Request, ctx: RouteContext<"/api/v1/tokens/[id]/revoke">) {
  if (!sameOrigin(request)) return error(403, "cross-origin request refused");
  const userId = await sessionUserId();
  if (!userId) return error(401, "sign in first");
  const { id } = await ctx.params;
  if (!UUID_RE.test(id)) return error(404, "token not found");
  const ok = await revokeToken(userId, id);
  if (wantsHtml(request)) return seeOther("/dashboard");
  return ok ? json({ ok: true }) : error(404, "token not found");
}
