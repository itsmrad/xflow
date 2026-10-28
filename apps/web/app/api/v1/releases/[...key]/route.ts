import { error } from "@/lib/http";
import { isReleaseKey, releaseUrl } from "@/lib/server/releases";

/** Public download: redirects to a 5-minute presigned URL for a CLI release artifact. */
export async function GET(_request: Request, ctx: RouteContext<"/api/v1/releases/[...key]">) {
  const key = (await ctx.params).key.join("/");
  if (!isReleaseKey(key)) return error(404, "not found");
  const url = await releaseUrl(key);
  if (!url) return error(503, "downloads are not configured");
  return new Response(null, { status: 302, headers: { location: url, "cache-control": "public, max-age=60" } });
}
