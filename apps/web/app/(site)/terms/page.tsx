import type { Metadata } from "next";
import { LegalPage } from "@/components/legal";

export const metadata: Metadata = { title: "Terms", alternates: { canonical: "/terms" } };

export default function Terms() {
  return (
    <LegalPage title="Terms" lede="The terms for using xflow Pro and this website.">
      <h2>Software</h2>
      <p>The xflow app is open source under the MIT licence. The licence in the repository governs the code.</p>
      <h2>Pro</h2>
      <p>Pro is $5/month and renews monthly until you cancel from the dashboard. Refunds: ask within 14 days of a charge. Fair-use limits apply to automated use.</p>
    </LegalPage>
  );
}
