import { SITE_URL } from "@/content/site";
import { changelog } from "@/lib/source";

export const dynamic = "force-static";

const esc = (s: string) => s.replace(/[<>&"']/g, (c) => `&#${c.charCodeAt(0)};`);

export function GET() {
  const items = changelog
    .map(
      (e) =>
        `<item><title>${esc(`v${e.version}: ${e.title}`)}</title><link>${SITE_URL}/changelog/${e.slug}</link><guid>${SITE_URL}/changelog/${e.slug}</guid><pubDate>${new Date(`${e.date}T00:00:00Z`).toUTCString()}</pubDate>${e.description ? `<description>${esc(e.description)}</description>` : ""}</item>`,
    )
    .join("");
  return new Response(
    `<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel><title>xflow changelog</title><link>${SITE_URL}/changelog</link><description>What shipped, when.</description>${items}</channel></rss>`,
    { headers: { "content-type": "application/rss+xml; charset=utf-8" } },
  );
}
