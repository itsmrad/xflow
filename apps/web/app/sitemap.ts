import type { MetadataRoute } from "next";
import { LEGAL_UPDATED, SITE_URL } from "@/content/site";
import { changelog, source } from "@/lib/source";

export default function sitemap(): MetadataRoute.Sitemap {
  const pages = ["", "/pricing", "/download", "/compare/wispr-flow", "/changelog"];
  // Legal pages carry a real "Last updated" date; the rest change with every deploy, so no lastmod.
  const legal = ["/privacy", "/terms", "/security"];
  return [
    ...pages.map((p) => ({ url: `${SITE_URL}${p}` })),
    ...legal.map((p) => ({ url: `${SITE_URL}${p}`, lastModified: LEGAL_UPDATED })),
    ...source.getPages().map((p) => ({ url: `${SITE_URL}${p.url}` })),
    ...changelog.map((e) => ({ url: `${SITE_URL}/changelog/${e.slug}`, lastModified: e.date })),
  ];
}
