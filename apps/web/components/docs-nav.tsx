import Link from "next/link";
import type { Root, Node } from "fumadocs-core/page-tree";
import type { TOCItemType } from "fumadocs-core/toc";
import { TocTracker } from "./toc-tracker";

/**
 * Sidebar from the page tree (§7.21): groups as mono labels, active item with a 2px Volt bar.
 * A server component: each docs page renders it with its own URL, so no client JS is needed.
 */
export function Sidebar({ tree, path }: { tree: Root; path: string }) {
  function render(nodes: Node[], depth = 0) {
    return nodes.map((n, i) => {
      if (n.type === "separator") return null;
      if (n.type === "folder")
        return (
          <li key={i} className="mt-6 first:mt-0">
            {n.index ? (
              <Item url={n.index.url} name={n.name} active={path === n.index.url} group />
            ) : (
              <p className="t-mono-label px-3 text-fg-subtle">{n.name}</p>
            )}
            <ul className="mt-2">{render(n.children, depth + 1)}</ul>
          </li>
        );
      return (
        <li key={i}>
          <Item url={n.url} name={n.name} active={path === n.url} group={depth === 0} />
        </li>
      );
    });
  }
  return (
    <nav aria-label="Docs">
      <ul>{render(tree.children)}</ul>
    </nav>
  );
}

function Item({ url, name, active, group = false }: { url: string; name: React.ReactNode; active: boolean; group?: boolean }) {
  if (group && !active)
    return (
      <Link href={url} className="t-mono-label block px-3 py-1.5 text-fg-subtle hover:text-fg">
        {name}
      </Link>
    );
  return (
    <Link
      href={url}
      aria-current={active ? "page" : undefined}
      className={`t-body-sm block py-1.5 pl-3 pr-2 transition-colors duration-[120ms] ${
        active ? "bg-ink-a4 text-fg shadow-[inset_2px_0_0_var(--volt)]" : "text-fg-muted hover:text-fg"
      } ${group ? "t-mono-label" : ""}`}
    >
      {name}
    </Link>
  );
}

/** "On this page": server-rendered anchors; TocTracker (a few hundred bytes) marks the heading in view. */
export function Toc({ toc, path }: { toc: TOCItemType[]; path: string }) {
  if (toc.length === 0) return null;
  return (
    <nav aria-label="On this page">
      <p className="t-mono-label text-fg-subtle">On this page</p>
      <ul className="mt-3 border-l border-line">
        {toc.map((t) => (
          <li key={t.url}>
            <a
              href={t.url}
              className="t-caption -ml-px block border-l border-transparent py-1 text-fg-muted transition-colors duration-[120ms] hover:border-line-strong hover:text-fg aria-[current=location]:border-volt aria-[current=location]:text-fg"
              style={{ paddingLeft: 12 + (t.depth - 2) * 12 }}
            >
              {t.title}
            </a>
          </li>
        ))}
      </ul>
      {/* Keyed by page: provider pages share heading ids, and the tracker must re-run per page. */}
      <TocTracker key={path} />
    </nav>
  );
}
