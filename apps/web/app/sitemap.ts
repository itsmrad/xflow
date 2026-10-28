import type { MetadataRoute } from "next";
import { SITE_URL } from "@/content/site";
import { changelog, source } from "@/lib/source";

export default function sitemap(): MetadataRoute.Sitemap {
  const pages = ["", "/pricing", "/download", "/compare/wispr-flow", "/changelog", "/privacy", "/terms", "/security"];
  return [
    ...pages.map((p) => ({ url: `${SITE_URL}${p}` })),
    ...source.getPages().map((p) => ({ url: `${SITE_URL}${p.url}` })),
    ...changelog.map((e) => ({ url: `${SITE_URL}/changelog/${e.slug}`, lastModified: e.date })),
  ];
}
