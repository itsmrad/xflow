import type { Metadata } from "next";
import Link from "next/link";
import { LegalPage, Mail } from "@/components/legal";
import { REPO } from "@/content/site";

export const metadata: Metadata = { title: "Terms", alternates: { canonical: "/terms" } };

export default function Terms() {
  return (
    <LegalPage title="Terms" lede="The terms for using xflow, its hosted plans and this website." updated="2026-10-29">
      <h2>Who we are</h2>
      <p>
        xflow and this website are run by an individual developer based in India (&ldquo;we&rdquo;, &ldquo;us&rdquo;). Contact: <Mail />. By using
        xflow, an xflow account or this website, you agree to these terms.
      </p>

      <h2>The software</h2>
      <p>
        The xflow app is open source under the <a href={`${REPO}/blob/main/LICENSE`}>MIT licence</a>, which governs the code. These terms cover the
        services we run: accounts, hosted plans and this website.
      </p>

      <h2>Check your text</h2>
      <p>
        Speech recognition and AI cleanup make mistakes. Read what xflow produces before you use, send or run it. We are not responsible for
        transcription or cleanup errors, or for what is pasted where, including into a terminal, a message or a document.
      </p>

      <h2>Your account</h2>
      <p>
        Keep your account and CLI tokens to yourself. You are responsible for what happens under them. If a token leaks, revoke it from the dashboard.
        Accounts are for people 18 and over.
      </p>

      <h2>Acceptable use</h2>
      <p>Don&apos;t:</p>
      <ul>
        <li>break the law, or use xflow to harm or harass anyone;</li>
        <li>try to break, overload or get around the limits or security of the service;</li>
        <li>share, resell or script access to a hosted plan, or use it as a general speech API for other apps.</li>
      </ul>

      <h2>Fair use on hosted plans</h2>
      <p>
        Hosted plans are unlimited for one person dictating. To keep them that way, we cap concurrent requests, request rates and daily and monthly
        usage per account. Hitting a cap pauses hosted transcription until it resets. Your own provider key keeps working.
      </p>

      <h2>Pro, payments and refunds</h2>
      <p>Pro is $5/month and is coming soon. When it launches:</p>
      <ul>
        <li>
          <a href="https://dodopayments.com">Dodo Payments</a> is the Merchant of Record. You buy Pro from Dodo, and your purchase contract is with
          Dodo. Dodo handles payment, tax, invoices and chargebacks under its own terms.
        </li>
        <li>Pro renews automatically each month until you cancel. You can cancel any time from the dashboard. Pro stays on until the end of the period you paid for.</li>
        <li>Ask within 14 days of a charge and we refund it in full. Email <Mail />.</li>
        <li>If we end your Pro for any reason other than a breach of these terms, we refund the unused part of the current period.</li>
      </ul>

      <h2>Changes to the service and prices</h2>
      <p>
        We may change, suspend or discontinue features at any time. We give at least 30 days&apos; notice of a price change, by email or on this
        site. A new price applies from your first renewal after the notice, and you can cancel before then.
      </p>

      <h2>Your own provider (BYOK)</h2>
      <p>
        With your own key, audio and text go from your device to the provider you chose, under that provider&apos;s terms, privacy policy and prices.
        We don&apos;t control third-party providers, and you are responsible for your account and charges with them.
      </p>

      <h2>Suspension and termination</h2>
      <p>
        We may suspend or close your account or hosted access if you break these terms, put the service or other users at risk, or if the law
        requires it. You can stop at any time and ask us to delete your account. See <Link href="/privacy">Privacy</Link>.
      </p>

      <h2>No warranty</h2>
      <p>
        xflow and its services are provided &ldquo;as is&rdquo; and &ldquo;as available&rdquo;, without warranties of any kind, express or implied,
        including merchantability, fitness for a particular purpose, accuracy, non-infringement, and uninterrupted or error-free operation.
      </p>

      <h2>Limitation of liability</h2>
      <p>
        To the extent the law allows, our total liability for all claims about xflow or these terms is limited to the fees you paid for xflow in the 3
        months before the claim. We are not liable for indirect, incidental, special, consequential or punitive damages, or for lost profits, data,
        revenue or goodwill.
      </p>

      <h2>Indemnity</h2>
      <p>
        You will indemnify us against third-party claims, losses and costs (including reasonable legal fees) that arise from your use of xflow, the
        text you produce with it, or your breach of these terms.
      </p>

      <h2>Law and courts</h2>
      <p>These terms are governed by the laws of India. The courts of India have exclusive jurisdiction over any dispute about them.</p>

      <h2>Rights the law protects</h2>
      <p>
        Nothing in these terms limits rights that cannot be limited by law, including consumer rights where you live. If a part of these terms is
        unenforceable, the rest still applies.
      </p>

      <h2>Changes to these terms</h2>
      <p>
        We post changes here and update the date above. If a change materially affects paying users, we email them at least 30 days before it
        applies.
      </p>
    </LegalPage>
  );
}
