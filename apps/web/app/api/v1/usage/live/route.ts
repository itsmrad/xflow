import { error, fields, json } from "@/lib/http";
import { sealUsage } from "@/lib/server/realtime";
import { sessionUserId } from "@/lib/server/users";

/** Renews the dashboard's sealed live-usage query. Same user and window, or 401. */
export async function POST(request: Request) {
  const userId = await sessionUserId();
  const body = await fields(request);
  if (!userId || body.userId !== userId) return error(401, "signed out");
  const since = typeof body.since === "string" ? new Date(body.since) : null;
  if (!since || Number.isNaN(since.getTime())) return error(400, "since is required");
  const sealed = await sealUsage(userId, since);
  return sealed ? json(sealed) : error(503, "realtime is not configured");
}
