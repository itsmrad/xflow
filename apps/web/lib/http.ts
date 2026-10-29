import { ipAddress } from "@vercel/functions/headers";
import { PRODUCTION_URL } from "@/content/site";

const NO_STORE = { "cache-control": "no-store" };

export function json(body: unknown, status = 200, headers: HeadersInit = {}): Response {
  return Response.json(body, { status, headers: { ...NO_STORE, ...headers } });
}

export function error(status: number, message: string, headers: HeadersInit = {}): Response {
  return json({ error: { message } }, status, headers);
}

/** Rate-limit key for the caller: Vercel's x-real-ip (set by its edge, not the client), IPv6 by /64. */
export function clientIp(request: Request): string {
  return ipKey(ipAddress(request) ?? "unknown");
}

/** One key per IPv4 address or IPv6 /64 (one host usually owns a whole /64). */
export function ipKey(ip: string): string {
  if (!ip.includes(":")) return ip;
  const last = ip.split(":").at(-1)!;
  if (last.includes(".")) return last; // IPv4-mapped (::ffff:1.2.3.4)
  const [head = "", tail = ""] = ip.toLowerCase().split("::");
  const h = head ? head.split(":") : [];
  const t = tail ? tail.split(":") : [];
  const groups = [...h, ...Array(Math.max(0, 8 - h.length - t.length)).fill("0"), ...t];
  return `${groups.slice(0, 4).map((g) => g.replace(/^0+(?=.)/, "")).join(":")}::/64`;
}

/**
 * Public origin for links the server hands out (device verification, checkout return):
 * NEXT_PUBLIC_APP_URL, else production's URL, else a preview's own URL, else local dev.
 */
export function appUrl(): string {
  const url =
    process.env.NEXT_PUBLIC_APP_URL ||
    (process.env.VERCEL_ENV === "production"
      ? PRODUCTION_URL
      : process.env.VERCEL_URL
        ? `https://${process.env.VERCEL_URL}`
        : "http://localhost:3000");
  return url.replace(/\/+$/, "");
}

/**
 * Browser form posts must come from our own origin. Requests without an Origin header
 * (curl, the CLI) can't carry the Lax session cookie cross-site, so they're fine.
 */
export function sameOrigin(request: Request): boolean {
  const origin = request.headers.get("origin");
  return !origin || origin === new URL(request.url).origin || origin === appUrl();
}

/** Body as JSON or form fields, whichever the client sent; {} when empty or malformed. */
export async function fields(request: Request): Promise<Record<string, unknown>> {
  const type = request.headers.get("content-type") ?? "";
  try {
    if (type.includes("application/json")) {
      const body: unknown = await request.json();
      return typeof body === "object" && body !== null && !Array.isArray(body) ? (body as Record<string, unknown>) : {};
    }
    if (type.includes("application/x-www-form-urlencoded") || type.includes("multipart/form-data")) {
      return Object.fromEntries((await request.formData()).entries());
    }
  } catch {
    // fall through
  }
  return {};
}

/** Form posts from a page get a 303 back to it; API clients get JSON. */
export function wantsHtml(request: Request): boolean {
  return (request.headers.get("accept") ?? "").includes("text/html");
}

export function seeOther(path: string): Response {
  return new Response(null, { status: 303, headers: { location: path, ...NO_STORE } });
}
