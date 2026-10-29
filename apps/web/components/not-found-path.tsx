"use client";

import { useSyncExternalStore } from "react";

const noop = () => () => {};

/**
 * Prints the missing path (§9.11). The 404 is prerendered once for every path, so the server
 * snapshot is a placeholder and the real path appears after hydration (no #418 mismatch).
 */
export function NotFoundPath() {
  const path = useSyncExternalStore(noop, () => location.pathname, () => "…");
  return (
    <code className="grid whitespace-pre-wrap break-all">
      <span>
        <span aria-hidden className="select-none text-syn-muted">$ </span>open {path}
      </span>
      <span className="text-rec">zsh: no such file or directory: {path}</span>
    </code>
  );
}
