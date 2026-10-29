import type { Instrumentation } from "next";

/** One structured line per server error for Vercel's logs: the route pattern, never the URL or body. */
export const onRequestError: Instrumentation.onRequestError = (error, request, context) => {
  const e = error as Error & { digest?: string };
  console.error(
    JSON.stringify({
      event: "request_error",
      digest: e.digest,
      name: e.name,
      method: request.method,
      route: context.routePath,
      type: context.routeType,
    }),
  );
};
