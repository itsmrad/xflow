import { highlight } from "@/lib/highlight";
import { CopyButton } from "./copy";

/** §7.10: build-time Shiki, filename header as a mono label, copy button, keyboard-scrollable. */
export async function CodeBlock({
  code,
  lang,
  filename,
  numbered = false,
  hl,
  className = "",
}: {
  code: string;
  lang: "toml" | "sh" | "json" | "text";
  filename?: string;
  numbered?: boolean;
  hl?: number[];
  className?: string;
}) {
  const html = await highlight(code, lang, hl);
  return (
    <figure className={`code not-prose relative min-w-0 overflow-hidden rounded-md border border-line bg-bg-sunken ${numbered ? "numbered" : ""} ${className}`}>
      <div className="flex h-9 items-center justify-between border-b border-line pl-4 pr-1">
        <figcaption className="t-mono-label text-fg-subtle">[ {filename ?? lang} ]</figcaption>
        <CopyButton text={code.trim()} label="Copy code" />
      </div>
      <div
        // Shiki output is generated at build time from strings in this repo.
        dangerouslySetInnerHTML={{ __html: html.replace("<pre ", '<pre tabindex="0" ') }}
      />
    </figure>
  );
}
