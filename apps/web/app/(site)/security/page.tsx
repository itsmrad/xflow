import type { Metadata } from "next";
import Link from "next/link";
import { LegalPage } from "@/components/legal";
import { REPO } from "@/content/site";

export const metadata: Metadata = { title: "Security", alternates: { canonical: "/security" } };

export default function Security() {
  return (
    <LegalPage title="Security" lede="How to report a vulnerability, and what xflow can touch on your machine.">
      <h2>Report a vulnerability</h2>
      <p>
        Email [SUPPORT_EMAIL], or use <a href={`${REPO}/security/advisories/new`}>GitHub private vulnerability reporting</a>. Please don&apos;t open a
        public issue for security bugs.
      </p>
      <h2>Permissions</h2>
      <p>
        On macOS, xflowd needs Microphone and Accessibility. Input Monitoring is asked for only if the shortcut still isn&apos;t heard. Password
        fields refuse dictation, and multi-line text is never auto-pasted into a terminal. See <Link href="/docs/troubleshooting/permissions">Permissions</Link>.
      </p>
    </LegalPage>
  );
}
