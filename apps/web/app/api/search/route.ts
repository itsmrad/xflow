import { createFromSource } from "fumadocs-core/search/server";
import { source } from "@/lib/source";

// Exported once at build time; the browser downloads it and searches locally (no external service).
export const revalidate = false;
export const { staticGET: GET } = createFromSource(source);
