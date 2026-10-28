"use client";

import { useSyncExternalStore } from "react";

type Mode = "system" | "light" | "dark";
const MODES: Mode[] = ["system", "light", "dark"];
const listeners = new Set<() => void>();

function read(): Mode {
  try {
    const t = localStorage.getItem("theme");
    return t === "light" || t === "dark" ? t : "system";
  } catch {
    return "system";
  }
}

function apply(mode: Mode) {
  const dark = mode === "system" ? matchMedia("(prefers-color-scheme: dark)").matches : mode === "dark";
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}

function subscribe(cb: () => void) {
  listeners.add(cb);
  const mq = matchMedia("(prefers-color-scheme: dark)");
  const onSystem = () => read() === "system" && apply("system");
  mq.addEventListener("change", onSystem);
  return () => {
    listeners.delete(cb);
    mq.removeEventListener("change", onSystem);
  };
}

/** System / Light / Dark segmented control (§5.9). The inline head script applies the theme before paint. */
export function ThemeToggle() {
  const mode = useSyncExternalStore(subscribe, read, () => "system" as Mode);
  function set(m: Mode) {
    try {
      if (m === "system") localStorage.removeItem("theme");
      else localStorage.setItem("theme", m);
    } catch {}
    apply(m);
    listeners.forEach((l) => l());
  }
  return (
    <div role="radiogroup" aria-label="Theme" className="inline-flex h-8 rounded-sm border border-line p-0.5">
      {MODES.map((m) => (
        <button
          key={m}
          type="button"
          role="radio"
          aria-checked={mode === m}
          onClick={() => set(m)}
          className={`t-caption rounded-xs px-2.5 capitalize transition-colors duration-[200ms] ${mode === m ? "bg-ink-a8 text-fg" : "text-fg-muted hover:text-fg"}`}
        >
          {m}
        </button>
      ))}
    </div>
  );
}
