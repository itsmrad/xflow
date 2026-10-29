import type { ReactNode } from "react";

/** The docs grid. Each page renders its own sidebar (it knows its URL) and <main>. */
export default function DocsLayout({ children }: { children: ReactNode }) {
  return <div className="frame lg:grid lg:grid-cols-[264px_minmax(0,1fr)]">{children}</div>;
}
