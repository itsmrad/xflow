"use client";

import { useRef, useState } from "react";
import type { InstallMethod } from "@/content/install";
import { CopyButton } from "./copy";

/** §7.7: tabbed install commands; ARIA tabs with roving tabindex; the `$ ` prompt is never copied. */
export function CommandBox({ methods, className = "" }: { methods: InstallMethod[]; className?: string }) {
  const [active, setActive] = useState(0);
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  const m = methods[active]!;
  function onKey(e: React.KeyboardEvent) {
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    const to = e.key === "Home" ? 0 : e.key === "End" ? methods.length - 1 : (active + d + methods.length) % methods.length;
    if (!d && e.key !== "Home" && e.key !== "End") return;
    e.preventDefault();
    setActive(to);
    tabs.current[to]?.focus();
  }
  return (
    <figure className={`not-prose overflow-hidden rounded-md border border-line bg-bg-raised text-left shadow-1 ${className}`}>
      <div className="flex items-center justify-between border-b border-line pr-1">
        <div role="tablist" aria-label="Install method" className="flex" onKeyDown={onKey}>
          {methods.map((x, i) => (
            <button
              key={x.id}
              ref={(el) => {
                tabs.current[i] = el;
              }}
              id={`tab-${x.id}`}
              role="tab"
              type="button"
              aria-selected={i === active}
              aria-controls={`panel-${x.id}`}
              tabIndex={i === active ? 0 : -1}
              onClick={() => setActive(i)}
              className={`t-label relative h-10 px-4 transition-colors duration-[200ms] ${
                i === active ? "text-fg after:absolute after:inset-x-4 after:bottom-0 after:h-0.5 after:bg-volt" : "text-fg-muted hover:text-fg"
              }`}
            >
              {x.label}
            </button>
          ))}
        </div>
        <CopyButton text={m.lines.map((l) => l.cmd).join("\n")} label="Copy commands" />
      </div>
      {methods.map((x, i) => (
        <div key={x.id} id={`panel-${x.id}`} role="tabpanel" aria-labelledby={`tab-${x.id}`} hidden={i !== active}>
          <pre tabIndex={0} className="t-mono overflow-x-auto px-5 py-4">
            <code className="grid">
              {x.lines.map((l) => (
                <span key={l.cmd} className="whitespace-pre">
                  <span aria-hidden className="select-none text-syn-muted">
                    ${" "}
                  </span>
                  {l.cmd}
                </span>
              ))}
            </code>
          </pre>
          {x.caption && <p className="t-caption border-t border-line px-5 py-3 text-fg-subtle">{x.caption}</p>}
        </div>
      ))}
    </figure>
  );
}
