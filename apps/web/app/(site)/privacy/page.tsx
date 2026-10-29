import type { Metadata } from "next";
import Link from "next/link";
import { LegalPage, Mail } from "@/components/legal";
import { LEGAL_UPDATED } from "@/content/site";

export const metadata: Metadata = {
  title: "Privacy",
  description: "What xflow and its website collect, why, and for how long. Free audio never touches our servers.",
  alternates: { canonical: "/privacy" },
};

export default function Privacy() {
  return (
    <LegalPage title="Privacy" lede="What xflow and this website collect, and why, in plain words." updated={LEGAL_UPDATED}>
      <h2>Who we are</h2>
      <p>
        xflow is run by an individual developer based in India, the data fiduciary under India&apos;s Digital Personal Data Protection Act, 2023.
        Contact: <Mail />.
      </p>

      <h2>The app, with your own key (Free)</h2>
      <p>
        Audio goes from your device straight to the speech provider you configure, or to a local server, and never touches our servers. The app has
        no telemetry. History is a local file on your machine that you can turn off, and your provider keys are stored encrypted on your machine.
        Details: <Link href="/docs/privacy-and-security">Privacy and security</Link>.
      </p>

      <h2>Pro (coming soon)</h2>
      <p>
        On Pro, the app sends your audio, and the text to clean up if cleanup is on, to our server. The server relays it to a third-party speech and AI
        provider and returns the result. We don&apos;t store the audio, the transcript or the cleaned-up text. The provider processes them under its
        own terms.
      </p>
      <p>For each request we keep a usage record, used for billing, fair-use limits and the usage on your dashboard:</p>
      <ul>
        <li>time, request type (transcription or cleanup), model and result status;</li>
        <li>audio size and duration, text token counts, provider cost and response time;</li>
        <li>which of your CLI tokens made the request.</li>
      </ul>
      <p>Usage records are kept while your account exists and deleted with it.</p>

      <h2>Your account</h2>
      <ul>
        <li>Your email address and account id, from Clerk, which runs sign-in.</li>
        <li>CLI tokens: a name, the first few characters, and a one-way hash. The token itself is never stored. Plus when it was created, last used or revoked.</li>
        <li>Subscription status and billing period from Dodo Payments, and your Dodo customer id. We never see your card details.</li>
        <li>CLI sign-in codes with the device name the CLI sends, and the IP addresses used to rate-limit sign-in. Both are deleted at the first sign-in after they are a day old.</li>
      </ul>

      <h2>This website</h2>
      <p>
        No analytics, no ad trackers, no tracking cookies. Your theme choice stays in your browser&apos;s local storage. Sign-in pages use the cookies
        Clerk needs for a session. Our host keeps standard request logs, including IP addresses, under its own retention.
      </p>

      <h2>Who processes data for us</h2>
      <ul>
        <li>Clerk: accounts and sign-in.</li>
        <li>Dodo Payments: payments, as Merchant of Record.</li>
        <li>Vercel: hosting. Neon: database.</li>
        <li>A third-party speech and AI provider: Pro requests only.</li>
      </ul>
      <p>These services may process data outside India. We don&apos;t sell your data and we don&apos;t show ads.</p>

      <h2>Your rights</h2>
      <p>
        Under the DPDP Act you can ask for a summary of your data, and for it to be corrected or erased. Email <Mail /> from your account&apos;s
        address. To delete your account, email us: we delete your account, tokens and usage records. Dodo keeps payment records it must keep by law.
        If we don&apos;t resolve a complaint, you can take it to the Data Protection Board of India.
      </p>

      <h2>Changes</h2>
      <p>We post changes here and update the date above.</p>
    </LegalPage>
  );
}
