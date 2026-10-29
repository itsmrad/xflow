import type { Metadata } from "next";
import { CtaBand } from "@/components/cta";
import { Section } from "@/components/frame";
import { ComparisonTable } from "@/components/marketing";
import { PageHead } from "@/components/page-head";
import { Ref } from "@/components/ui";
import { WISPR } from "@/content/site";

export const dynamic = "force-static";
export const metadata: Metadata = {
  title: "xflow vs Wispr Flow",
  description: "Price, privacy and platforms, compared from public pages.",
  alternates: { canonical: "/compare/wispr-flow" },
};

function Block({ id, title, children }: { id: string; title: string; children: React.ReactNode }) {
  return (
    <Section labelledBy={id}>
      <div className="grid gap-6 p-[var(--cell-p)] py-12 lg:grid-cols-12">
        <h2 id={id} className="t-h2 text-fg lg:col-span-4">
          {title}
        </h2>
        <div className="t-body max-w-[64ch] space-y-4 text-fg-muted lg:col-span-8">{children}</div>
      </div>
    </Section>
  );
}

export default function Compare() {
  return (
    <>
      <PageHead label="Compare" title="xflow vs Wispr Flow" lede={`Wispr Flow is good. Here is how they differ, from public pages. Last checked ${WISPR.checked}.`} />
      <Section labelledBy="table-title">
        <div className="p-[var(--cell-p)]">
          <h2 id="table-title" className="sr-only">
            Comparison
          </h2>
          <ComparisonTable />
        </div>
      </Section>
      <Block id="price" title="Price">
        <p>
          xflow Pro, coming soon, will be $5/month and unlimited. Wispr Flow Pro is $15/month, or $12/month billed yearly.
          <Ref n={13} />
        </p>
        <p>
          xflow Free is unlimited when you bring your own speech provider key. You pay the provider directly. Wispr Flow&apos;s free tier allows
          2,000 words a week on desktop.
          <Ref n={13} />
        </p>
      </Block>
      <Block id="privacy" title="Privacy">
        <p>
          xflow is MIT licensed and has no telemetry.
          <Ref n={3} /> Audio stays in memory and is never saved or logged by default.
          <Ref n={5} /> With <code className="t-mono-sm text-fg">privacy.offline = true</code> and a local whisper.cpp server, nothing leaves your
          machine.
          <Ref n={6} />
        </p>
      </Block>
      <Block id="better" title="Where Wispr Flow is better">
        <p>
          Platforms. Wispr Flow runs on macOS, Windows, iOS and Android.
          <Ref n={13} /> xflow runs on macOS, with Linux (GNOME) in beta. There is no Windows or mobile version.
        </p>
      </Block>
      <CtaBand />
    </>
  );
}
