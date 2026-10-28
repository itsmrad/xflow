"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import type { Root, Node } from "fumadocs-core/page-tree";
import { AnchorProvider, TOCItem, type TOCItemType } from "fumadocs-core/toc";

/** Sidebar from the page tree (§7.21): groups as mono labels, active item with a 2px Volt bar. */
export function Sidebar({ tree }: { tree: Root }) {
  const path = usePathname();
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
      return <Item key={i} url={n.url} name={n.name} active={path === n.url} group={depth === 0} />;
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

export function Toc({ toc }: { toc: TOCItemType[] }) {
  if (toc.length === 0) return null;
  return (
    <AnchorProvider toc={toc} single>
      <nav aria-label="On this page">
        <p className="t-mono-label text-fg-subtle">On this page</p>
        <ul className="mt-3 border-l border-line">
          {toc.map((t) => (
            <li key={t.url}>
              <TOCItem
                href={t.url}
                className="t-caption -ml-px block border-l border-transparent py-1 text-fg-muted transition-colors duration-[120ms] hover:text-fg data-[active=true]:border-volt data-[active=true]:text-fg"
                style={{ paddingLeft: 12 + (t.depth - 2) * 12 }}
              >
                {t.title}
              </TOCItem>
            </li>
          ))}
        </ul>
      </nav>
    </AnchorProvider>
  );
}
