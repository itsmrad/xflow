"use client";

import { useState } from "react";
import { IconCheck, IconCopy } from "./icons";

/** Copy button for MDX code blocks: copies the sibling <pre>'s text. */
export function CopyText() {
  const [done, setDone] = useState(false);
  async function copy(e: React.MouseEvent<HTMLButtonElement>) {
    const pre = e.currentTarget.closest("figure")?.querySelector("pre");
    if (!pre) return;
    try {
      await navigator.clipboard.writeText(pre.innerText.replace(/\n$/, ""));
      setDone(true);
      setTimeout(() => setDone(false), 1500);
    } catch {
      getSelection()?.selectAllChildren(pre);
    }
  }
  return (
    <button type="button" onClick={copy} aria-label="Copy code" className="grid size-8 place-items-center rounded-sm text-fg-subtle transition-colors duration-[120ms] hover:bg-ink-a4 hover:text-fg">
      {done ? <IconCheck className="text-ok" /> : <IconCopy />}
      <span role="status" aria-live="polite" className="sr-only">
        {done ? "Copied" : ""}
      </span>
    </button>
  );
}
