import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { mdxComponents } from "@/components/mdx";
import { PageHead } from "@/components/page-head";
import { Section } from "@/components/frame";
import { LinkMono } from "@/components/ui";
import { changelog } from "@/lib/source";

export const dynamicParams = false;
export const generateStaticParams = () => changelog.map((e) => ({ slug: e.slug }));

export async function generateMetadata({ params }: { params: Promise<{ slug: string }> }): Promise<Metadata> {
  const { slug } = await params;
  const e = changelog.find((x) => x.slug === slug);
  return e ? { title: `v${e.version}: ${e.title}`, description: e.description, alternates: { canonical: `/changelog/${e.slug}` } } : {};
}

export default async function Entry({ params }: { params: Promise<{ slug: string }> }) {
  const { slug } = await params;
  const e = changelog.find((x) => x.slug === slug);
  if (!e) notFound();
  return (
    <>
      <PageHead label={`v${e.version} · ${e.date}`} title={e.title} />
      <Section>
        <div className="prose p-[var(--cell-p)] py-12">
          <e.body components={mdxComponents} />
        </div>
        <div className="border-t border-line p-[var(--cell-p)]">
          <LinkMono href="/changelog">← All releases</LinkMono>
        </div>
      </Section>
    </>
  );
}
