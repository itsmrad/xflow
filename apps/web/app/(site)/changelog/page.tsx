import type { Metadata } from "next";
import Link from "next/link";
import { Section } from "@/components/frame";
import { mdxComponents } from "@/components/mdx";
import { PageHead } from "@/components/page-head";
import { Badge } from "@/components/ui";
import { changelog } from "@/lib/source";

export const dynamic = "force-static";
export const metadata: Metadata = {
  title: "Changelog",
  description: "What shipped, when.",
  alternates: { canonical: "/changelog", types: { "application/rss+xml": "/changelog/rss.xml" } },
};

export default function Changelog() {
  return (
    <>
      <PageHead label="Changelog" title="Changelog" lede="What shipped, when.">
        <a href="/changelog/rss.xml" className="t-mono-sm mt-6 inline-block text-volt-text underline-offset-4 hover:underline">
          RSS →
        </a>
      </PageHead>
      {changelog.map((e) => (
        <Section key={e.slug} id={e.slug} labelledBy={`${e.slug}-title`}>
          <div className="grid lg:grid-cols-12">
            <div className="border-b border-line p-[var(--cell-p)] lg:col-span-3 lg:border-r lg:border-b-0">
              <div className="t-mono-sm lg:sticky lg:top-24">
                <Link href={`/changelog/${e.slug}`} className="text-fg hover:underline">
                  v{e.version}
                </Link>
                <p className="text-fg-subtle">
                  <time dateTime={e.date}>{e.date}</time>
                </p>
              </div>
            </div>
            <article className="p-[var(--cell-p)] lg:col-span-9">
              <div className="flex flex-wrap items-center gap-3">
                <h2 id={`${e.slug}-title`} className="t-h3 text-fg">
                  {e.title}
                </h2>
                {e.tags.map((t) => (
                  <Badge key={t}>{t}</Badge>
                ))}
              </div>
              <div className="prose mt-6">
                <e.body components={mdxComponents} />
              </div>
            </article>
          </div>
        </Section>
      ))}
    </>
  );
}
