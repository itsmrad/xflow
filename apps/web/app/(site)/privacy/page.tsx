import type { Metadata } from "next";
import Link from "next/link";
import { LegalPage } from "@/components/legal";

export const metadata: Metadata = { title: "Privacy", alternates: { canonical: "/privacy" } };

export default function Privacy() {
  return (
    <LegalPage title="Privacy" lede="What xflow and this website collect, in plain words.">
      <h2>The app</h2>
      <p>
        xflow has no telemetry. Audio stays in memory and goes only to the speech provider you configure, or nowhere in offline mode. History is a
        local SQLite file you can turn off. Details: <Link href="/docs/privacy-and-security">Privacy and security</Link>.
      </p>
      <h2>Pro</h2>
      <p>On Pro, audio goes through xflow&apos;s server to our speech provider. We keep a usage record (time, duration, size) for billing and abuse limits.</p>
      <h2>This website</h2>
      <p>No analytics, no tracking cookies. Signing in uses Clerk, which sets the cookies a session needs. Payments go through Dodo Payments.</p>
    </LegalPage>
  );
}
