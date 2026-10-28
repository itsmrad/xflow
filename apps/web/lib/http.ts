const NO_STORE = { "cache-control": "no-store" };

export function json(body: unknown, status = 200, headers: HeadersInit = {}): Response {
  return Response.json(body, { status, headers: { ...NO_STORE, ...headers } });
}

export function error(status: number, message: string, headers: HeadersInit = {}): Response {
  return json({ error: { message } }, status, headers);
}

/** First hop of x-forwarded-for (set by Vercel), for per-IP limits. */
export function clientIp(request: Request): string {
  return request.headers.get("x-forwarded-for")?.split(",")[0]?.trim() || "unknown";
}

export function appUrl(): string {
  return (process.env.NEXT_PUBLIC_APP_URL ?? "http://localhost:3000").replace(/\/+$/, "");
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
