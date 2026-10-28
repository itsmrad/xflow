import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import { findNeighbour } from "fumadocs-core/page-tree";
import { CommandBox } from "@/components/command-box";
import { Toc } from "@/components/docs-nav";
import { FeatureGrid } from "@/components/marketing";
import { mdxComponents } from "@/components/mdx";
import { IconArrow, IconBook, IconBox, IconKey, IconKeyboard, IconTerminal, IconWrench } from "@/components/icons";
import { Callout } from "@/components/ui";
import { INSTALL } from "@/content/install";
import { REPO } from "@/content/site";
import { source } from "@/lib/source";

export const dynamic = "force-static";
export const dynamicParams = false;

export function generateStaticParams() {
  return source.generateParams();
}

export async function generateMetadata({ params }: { params: Promise<{ slug?: string[] }> }): Promise<Metadata> {
  const { slug } = await params;
  const page = source.getPage(slug);
  if (!page) notFound();
  const title = slug ? page.data.title : "Docs";
  return {
    title,
    description: page.data.description,
    alternates: { canonical: page.url },
  };
}

const ENTRIES = [
  { href: "/docs/getting-started/install", title: "Install", body: "Build from source on macOS or Linux.", Icon: IconBox },
  { href: "/docs/getting-started/setup", title: "Setup", body: "Provider, key, microphone, shortcut, permissions.", Icon: IconWrench },
  { href: "/docs/using/hotkeys", title: "Hotkeys", body: "Hold Fn, double-tap for hands-free, rebind.", Icon: IconKeyboard },
  { href: "/docs/using/dictionary", title: "Dictionary", body: "Vocabulary hints, replacements, learning.", Icon: IconBook },
  { href: "/docs/providers", title: "Providers", body: "Ten cloud services, or a local server.", Icon: IconKey },
  { href: "/docs/reference/cli", title: "CLI reference", body: "Every command, generated from --help.", Icon: IconTerminal },
];

export default async function DocsPage({ params }: { params: Promise<{ slug?: string[] }> }) {
  const { slug } = await params;
  const page = source.getPage(slug);
  if (!page) notFound();
  const MDX = page.data.body;
  const { previous, next } = findNeighbour(source.pageTree, page.url);
  const file = `apps/web/content/docs/${page.path}`;
  const home = !slug;

  return (
    <div className="xl:grid xl:grid-cols-[minmax(0,1fr)_220px]">
      <article className="min-w-0 px-[var(--cell-p)] py-10 lg:py-12">
        <p aria-hidden className="t-mono-label text-fg-subtle">
          [ <span className="text-fg-muted">{home ? "Docs" : page.url.split("/").slice(2, -1).join(" / ").replace(/-/g, " ") || "Docs"}</span> ]
        </p>
        <h1 className="t-h1 mt-4 text-fg">{home ? "Docs" : page.data.title}</h1>
        {page.data.description && <p className="t-lede mt-4 max-w-[64ch] text-fg-muted">{page.data.description}</p>}
        {home && (
          <div className="mt-10 space-y-10">
            <CommandBox methods={INSTALL} />
            <div className="-mx-[var(--cell-p)] border-y border-line">
              <FeatureGrid cols={3}>
                {ENTRIES.map(({ href, title, body, Icon }) => (
                  <div key={href} className="group relative cell transition-colors duration-[120ms] hover:bg-ink-a4">
                    <Icon className="text-fg-subtle transition-colors group-hover:text-volt-text" />
                    <h2 className="t-h3 mt-4 text-fg">
                      <Link href={href} className="stretch">
                        {title}
                      </Link>
                    </h2>
                    <p className="t-body-sm mt-1 text-fg-muted">{body}</p>
                  </div>
                ))}
              </FeatureGrid>
            </div>
            <Callout type="note" title="Something not working?">
              Run <code>xflow doctor</code>, then see <Link href="/docs/troubleshooting/permissions">Troubleshooting</Link>.
            </Callout>
          </div>
        )}
        <div className="prose mt-10">
          <MDX components={mdxComponents} />
        </div>
        <footer className="mt-16 border-t border-line pt-6">
          <a href={`${REPO}/edit/main/${file}`} className="t-mono-sm text-fg-subtle hover:text-fg">
            Edit on GitHub ↗
          </a>
          <nav aria-label="Previous and next" className="mt-6 grid gap-px overflow-hidden rounded-md border border-line bg-line sm:grid-cols-2">
            {[previous, next].map((n, i) =>
              n ? (
                <Link key={n.url} href={n.url} className={`bg-bg p-4 transition-colors hover:bg-bg-sunken ${i ? "sm:text-right" : ""}`}>
                  <span className="t-mono-label text-fg-subtle">{i ? "Next" : "Previous"}</span>
                  <span className="t-body-sm mt-1 flex items-center gap-2 text-fg sm:[justify-content:var(--j)]" style={{ ["--j" as string]: i ? "flex-end" : "flex-start" }}>
                    {!i && <IconArrow className="rotate-180" />}
                    {n.name}
                    {i ? <IconArrow /> : null}
                  </span>
                </Link>
              ) : (
                <span key={i} className="hidden bg-bg sm:block" />
              ),
            )}
          </nav>
        </footer>
      </article>
      <aside className="hidden border-l border-line xl:block">
        <div className="sticky top-16 max-h-[calc(100dvh-64px)] overflow-y-auto p-6">
          <Toc toc={page.data.toc} />
        </div>
      </aside>
    </div>
  );
}
