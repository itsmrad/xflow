"use client";

import { useState } from "react";
import { IconCheck, IconCopy } from "./icons";

/**
 * 32×32 icon button; ✓ for 1.5 s and an aria-live "Copied" (§7.7). Copies `text`, or the code block's
 * visible <pre>. If the clipboard refuses, it selects that <pre> so ⌘C still works.
 */
export function CopyButton({ text, label = "Copy code" }: { text?: string; label?: string }) {
  const [done, setDone] = useState(false);
  async function copy(e: React.MouseEvent<HTMLButtonElement>) {
    // Resolve the target now: React clears currentTarget once the handler awaits.
    const figure = e.currentTarget.closest("figure");
    const pre = figure?.querySelector<HTMLElement>("[role=tabpanel]:not([hidden]) pre") ?? figure?.querySelector<HTMLElement>("pre");
    try {
      await navigator.clipboard.writeText(text ?? pre?.innerText.replace(/\n$/, "") ?? "");
    } catch {
      if (pre) getSelection()?.selectAllChildren(pre);
      return;
    }
    setDone(true);
    setTimeout(() => setDone(false), 1500);
  }
  return (
    <button
      type="button"
      onClick={copy}
      aria-label={label}
      className="grid size-8 shrink-0 place-items-center rounded-sm text-fg-subtle transition-colors duration-[120ms] hover:bg-ink-a4 hover:text-fg"
    >
      {done ? <IconCheck className="text-ok" /> : <IconCopy />}
      <span role="status" aria-live="polite" className="sr-only">
        {done ? "Copied" : ""}
      </span>
    </button>
  );
}
