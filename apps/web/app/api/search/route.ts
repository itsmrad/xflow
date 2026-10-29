import { createFromSource } from "fumadocs-core/search/server";
import { source } from "@/lib/source";

// Exported once at build time; the browser downloads it on first search and searches locally.
export const revalidate = false;
const { staticGET } = createFromSource(source);

export async function GET() {
  const res = await staticGET();
  // An hour in the browser, so moving between docs pages doesn't fetch the index again.
  return new Response(res.body, {
    headers: { "content-type": "application/json", "cache-control": "public, max-age=3600, s-maxage=31536000" },
  });
}
