import type { Metadata } from "next";
import { CtaBand } from "@/components/cta";
import { Section } from "@/components/frame";
import { Faq, faqJsonLd, JsonLd, PricingCards } from "@/components/marketing";
import { PageHead } from "@/components/page-head";
import { Callout, Ref } from "@/components/ui";
import { BILLING_FAQ } from "@/content/marketing";

export const dynamic = "force-static";
export const metadata: Metadata = {
  title: "Pricing",
  description: "Free with your own provider key, unlimited. Pro is $5/month, unlimited, with nothing to set up.",
  alternates: { canonical: "/pricing" },
};

const product = {
  "@context": "https://schema.org",
  "@type": "Product",
  name: "xflow",
  description: "Voice dictation for developers.",
  offers: [
    { "@type": "Offer", name: "Free", price: "0", priceCurrency: "USD" },
    { "@type": "Offer", name: "Pro", price: "5", priceCurrency: "USD" },
  ],
};

export default function Pricing() {
  return (
    <>
      <JsonLd data={product} />
      <JsonLd data={faqJsonLd(BILLING_FAQ)} />
      <PageHead
        label="Pricing"
        title="Free with your key."
        hl="$5 when you'd rather not."
        lede="Both plans are unlimited. The only difference is whose provider key does the work."
      />
      <Section labelledBy="plans-title">
        <h2 id="plans-title" className="sr-only">
          Plans
        </h2>
        <PricingCards headingLevel={3} />
        <p className="t-caption border-t border-line px-[var(--cell-p)] py-4 text-fg-subtle">
          Prices in USD. Taxes calculated at checkout. Payments handled by Dodo Payments, our merchant of record.
        </p>
      </Section>
      <Section label={{ title: "What you'd pay elsewhere" }} labelledBy="elsewhere-title">
        <div className="grid gap-10 p-[var(--cell-p)] lg:grid-cols-12">
          <h2 id="elsewhere-title" className="t-h2 text-fg lg:col-span-4">
            A third of Wispr Flow Pro.
          </h2>
          <div className="lg:col-span-8">
            <table className="w-full border-collapse text-left">
              <caption className="sr-only">Monthly prices</caption>
              <thead>
                <tr className="t-mono-label border-b border-line text-fg-subtle">
                  <th scope="col" className="py-3 pr-4 font-medium">Plan</th>
                  <th scope="col" className="py-3 pr-4 font-medium">Price</th>
                  <th scope="col" className="py-3 font-medium">Limits</th>
                </tr>
              </thead>
              <tbody className="t-body-sm text-fg-muted">
                <tr className="border-b border-line bg-volt-soft">
                  <th scope="row" className="py-4 pr-4 pl-2 font-medium text-fg">xflow Pro</th>
                  <td className="t-mono-sm py-4 pr-4 text-fg">$5/mo</td>
                  <td className="py-4">Unlimited</td>
                </tr>
                <tr className="border-b border-line">
                  <th scope="row" className="py-4 pr-4 pl-2 font-normal">xflow Free</th>
                  <td className="t-mono-sm py-4 pr-4">$0</td>
                  <td className="py-4">Unlimited, your own key</td>
                </tr>
                <tr className="border-b border-line">
                  <th scope="row" className="py-4 pr-4 pl-2 font-normal">Wispr Flow Pro</th>
                  <td className="t-mono-sm py-4 pr-4">
                    $15/mo or $12/mo yearly
                    <Ref n={13} />
                  </td>
                  <td className="py-4">Free tier: 2,000 words/week</td>
                </tr>
              </tbody>
            </table>
            <div className="mt-8">
              <Callout type="note" title="Fair use.">
                Pro is unlimited for people. To keep it that way, we cap concurrent requests and per-minute bursts, so scripts can&apos;t
                drain the shared key. You&apos;ll never hit the limit by talking.
              </Callout>
            </div>
          </div>
        </div>
      </Section>
      <Section labelledBy="billing-faq">
        <div className="section-y grid gap-10 px-[var(--cell-p)] lg:grid-cols-12">
          <h2 id="billing-faq" className="t-h2 text-fg lg:col-span-4">
            Billing questions
          </h2>
          <div className="lg:col-span-8">
            <Faq items={BILLING_FAQ} />
          </div>
        </div>
      </Section>
      <CtaBand />
    </>
  );
}
