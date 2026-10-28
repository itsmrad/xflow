"use client";

import { useState } from "react";
import { IconCheck, IconCopy } from "./icons";

/** 32×32 icon button; ✓ for 1.5 s and an aria-live "Copied" (§7.7). Falls back to selecting text. */
export function CopyButton({ text, label = "Copy command" }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  async function copy(e: React.MouseEvent<HTMLButtonElement>) {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const pre = e.currentTarget.closest("figure")?.querySelector("pre");
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
