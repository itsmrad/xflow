import { loader } from "fumadocs-core/source";
import { changelog as entries, docs } from "fumadocs-mdx:collections/server";

export const source = loader({ baseUrl: "/docs", source: docs.toFumadocsSource() });

export type Entry = (typeof entries)[number];
/** Newest first. The slug is the file name without its date prefix. */
export const changelog = [...entries]
  .map((e) => ({ ...e, slug: `v${e.version.replace(/\./g, "-")}` }))
  .sort((a, b) => b.date.localeCompare(a.date));
