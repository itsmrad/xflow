import type { Metadata } from "next";
import Link from "next/link";
import { LegalPage, Mail } from "@/components/legal";
import { REPO } from "@/content/site";

export const metadata: Metadata = { title: "Security", alternates: { canonical: "/security" } };

export default function Security() {
  return (
    <LegalPage title="Security" lede="How to report a vulnerability, and what xflow can touch on your machine." updated="2026-10-29">
      <h2>Report a vulnerability</h2>
      <p>
        Email <Mail />, or use <a href={`${REPO}/security/advisories/new`}>GitHub private vulnerability reporting</a>. Please don&apos;t open a public
        issue for security bugs. Include the version, the steps to reproduce and what an attacker gains. There is no bug bounty.
      </p>
      <h2>Permissions</h2>
      <p>
        On macOS, xflowd needs Microphone and Accessibility. Input Monitoring is asked for only if the shortcut still isn&apos;t heard. Password
        fields refuse dictation, and multi-line text is never auto-pasted into a terminal. See <Link href="/docs/troubleshooting/permissions">Permissions</Link>.
      </p>
      <h2>Keys and tokens</h2>
      <p>
        Provider keys are encrypted at rest in xflow&apos;s own keystore, readable only by you. That stops casual disclosure, not malware running as
        you. Our server stores CLI tokens only as one-way hashes. See <Link href="/docs/privacy-and-security">Privacy and security</Link>.
      </p>
    </LegalPage>
  );
}
