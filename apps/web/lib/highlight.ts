import "server-only";
import { createHighlighter, type Highlighter } from "shiki";
import { xflowTheme } from "./shiki-theme";

let highlighter: Promise<Highlighter> | undefined;

/** Build-time highlighting for marketing pages; zero runtime JS. */
export async function highlight(code: string, lang: "toml" | "sh" | "json" | "text", highlightLines: number[] = []) {
  highlighter ??= createHighlighter({ themes: [xflowTheme], langs: ["toml", "sh", "json"] });
  return (await highlighter).codeToHtml(code.trim(), {
    lang: lang === "text" ? "text" : lang,
    theme: "xflow",
    transformers: [
      {
        line(node, line) {
          if (highlightLines.includes(line)) this.addClassToHast(node, "hl");
        },
      },
    ],
  });
}
