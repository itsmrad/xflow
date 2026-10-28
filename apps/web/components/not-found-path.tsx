"use client";

import { usePathname } from "next/navigation";

/** Prints the missing path (§9.11). The page stays static; only the path is client-side. */
export function NotFoundPath() {
  const path = usePathname() || "/";
  return (
    <code className="grid">
      <span>
        <span aria-hidden className="select-none text-syn-muted">$ </span>open {path}
      </span>
      <span className="text-rec">zsh: no such file or directory: {path}</span>
    </code>
  );
}
