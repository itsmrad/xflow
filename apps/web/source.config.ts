import { defineCollections, defineConfig, defineDocs, frontmatterSchema } from "fumadocs-mdx/config";
import { z } from "zod";
import { xflowTheme } from "./lib/shiki-theme";

export const docs = defineDocs({ dir: "content/docs" });

export const changelog = defineCollections({
  type: "doc",
  dir: "content/changelog",
  schema: frontmatterSchema.extend({
    version: z.string(),
    date: z.string(),
    tags: z.array(z.enum(["macos", "linux", "cli", "tui", "pro", "web"])).default([]),
  }),
});

export default defineConfig({
  mdxOptions: {
    // One css-variables theme: syntax colours follow the light/dark tokens (§5.1).
    rehypeCodeOptions: { themes: { light: xflowTheme, dark: xflowTheme }, defaultColor: "light", langs: ["sh", "bash", "toml", "json"] },
  },
});
