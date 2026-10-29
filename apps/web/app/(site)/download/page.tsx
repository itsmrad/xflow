import type { Metadata } from "next";
import Link from "next/link";
import { CommandBox } from "@/components/command-box";
import { Crossings, Heading, Section } from "@/components/frame";
import { NumberedRows } from "@/components/marketing";
import { Pill } from "@/components/pill";
import { Terminal } from "@/components/terminal";
import { Button, LinkMono, Ref } from "@/components/ui";
import { INSTALL } from "@/content/install";
import { REPO } from "@/content/site";
import { latestRelease } from "@/lib/release";
import { cliVersion } from "@/lib/version";

export const revalidate = 3600;
export const metadata: Metadata = {
  title: "Download",
  description: "Install xflow on macOS or Linux. Open source, MIT, no telemetry.",
  alternates: { canonical: "/download" },
};

const mb = (bytes: number) => `${(bytes / 1_048_576).toFixed(1)} MB`;

export default async function Download() {
  const release = await latestRelease();
  const pkg = release?.assets.find((a) => a.name.endsWith(".pkg")) ?? null;
  const version = release?.tag.replace(/^v/, "") ?? cliVersion();
  const chips: [string, React.ReactNode][] = [
    ["Version", version],
    ["Size", pkg ? mb(pkg.size) : "source"],
    [
      "Idle RAM",
      <>
        11 MB<Ref n={1} />
      </>,
    ],
    [
      "Telemetry",
      <>
        none<Ref n={3} />
      </>,
    ],
    ["License", "MIT"],
    ["Platform", "macOS · Linux"],
  ];

  return (
    <>
      <section className="frame rule -mt-px" aria-labelledby="listing-title">
        <Crossings />
        <div className="flex flex-col gap-6 p-[var(--cell-p)] md:flex-row md:items-center">
          <div aria-hidden className="grid size-20 shrink-0 place-items-center rounded-[18px] bg-[#141414] font-mono text-[26px] font-medium text-[#EDEDEC] shadow-1">
            <span>
              xf<span className="cursor ml-0.5 !w-[0.4em]" />
            </span>
          </div>
          <div className="flex-1">
            <h1 id="listing-title" className="t-h1 text-fg">
              xflow
            </h1>
            <p className="t-body mt-1 text-fg-muted">Voice dictation for developers</p>
            <p className="t-caption mt-1 text-fg-subtle">by the xflow project · MIT · open source</p>
          </div>
          <div className="flex flex-col items-start gap-2 md:items-end">
            {pkg ? (
              <Button href={pkg.url} variant="primary" size="lg">
                Download .pkg
              </Button>
            ) : (
              <Button href={REPO} variant="primary" size="lg">
                Get the source
              </Button>
            )}
            <p className="t-caption text-fg-subtle">
              {pkg ? `${release?.prerelease ? "Pre-release · " : ""}macOS, Apple silicon and Intel` : "No macOS package yet"}
            </p>
            {pkg && (
              <p className="t-caption max-w-[36ch] text-fg-subtle md:text-right">
                Not notarised yet: macOS blocks a downloaded .pkg until you allow it in System Settings › Privacy &amp; Security › Open
                Anyway.
              </p>
            )}
            {release && (
              <LinkMono href={release.url}>
                {pkg ? "Linux and all files" : "Linux binaries"}, {release.tag} →
              </LinkMono>
            )}
          </div>
        </div>
        <dl className="grid grid-cols-2 gap-px border-t border-line bg-line sm:grid-cols-3 xl:grid-cols-6">
          {chips.map(([k, v]) => (
            <div key={k} className="bg-bg px-[var(--cell-p)] py-4">
              <dt className="t-mono-label text-fg-subtle">{k}</dt>
              <dd className="t-mono-sm mt-1 text-fg">{v}</dd>
            </div>
          ))}
        </dl>
      </section>

      <Section id="install" label={{ n: 1, total: 3, title: "Install" }} labelledBy="install-title">
        <div className="section-y px-[var(--cell-p)]">
          <Heading
            id="install-title"
            title={pkg ? "Or build it from source." : "Build it, run setup."}
            lede={
              pkg
                ? "The same code as the package. It takes one cargo build and the install script."
                : "Until a package is published, xflow installs from source. It takes one cargo build and the install script."
            }
          />
          <div id="linux" className="scroll-mt-24">
            <CommandBox methods={INSTALL} />
          </div>
          <p className="t-caption mt-4 text-fg-subtle">
            Needs Rust 1.88+. On macOS, Xcode Command Line Tools. Details in <Link className="underline underline-offset-2 hover:text-fg" href="/docs/getting-started/install">the install guide</Link>.
          </p>
        </div>
      </Section>

      <Section id="next" label={{ n: 2, total: 3, title: "What happens next" }} labelledBy="next-title">
        <div className="grid lg:grid-cols-12">
          <div className="section-y lg:col-span-7">
            <div className="px-[var(--cell-p)]">
              <Heading id="next-title" title="One guided setup." lede="Plain xflow in a terminal walks you through it, then opens the control centre." />
            </div>
            <NumberedRows
              rows={[
                { title: "Speech service", body: "Pick a provider, or a local server." },
                { title: "Key", body: "Paste it once. Hidden input." },
                { title: "Microphone", body: "Choose your input device." },
                { title: "Shortcut", body: "Fn by default. On macOS, setup opens the Globe key setting so you can set it to Do Nothing and stop it opening emoji." },
                { title: "Permissions", body: "Microphone and Accessibility. We open the exact System Settings pane; you flip one switch each." },
              ]}
            />
          </div>
          <div className="bg-dots-faint flex flex-col items-center justify-center gap-6 border-t border-line p-[var(--cell-p)] lg:col-span-5 lg:border-t-0 lg:border-l">
            <p className="t-mono-label text-fg-subtle">Then hold Fn and say something</p>
            <Pill state="listening" />
            <Pill state="processing" />
            <Pill state="success" />
          </div>
        </div>
      </Section>

      <Section id="verify" label={{ n: 3, total: 3, title: "Verify" }} labelledBy="verify-title">
        <div className="section-y grid gap-8 px-[var(--cell-p)] lg:grid-cols-2">
          <div>
            <Heading id="verify-title" title="Check what you run." lede={
                pkg
                  ? "Every line is on GitHub. Releases list a SHA-256 per file. The binaries are signed as com.xflow.xflowd with xflow's own certificate, not an Apple Developer ID, and aren't notarised yet."
                  : "Every line is on GitHub, and a source build runs exactly the code you read. Release binaries come with a SHA256SUMS file."
              }
            />
            {pkg?.sha256 && (
              <p className="t-mono-sm break-all text-fg-muted">
                {pkg.name}: <span className="text-fg">{pkg.sha256}</span>
              </p>
            )}
            <LinkMono href={release?.url ?? "/changelog"}>Release notes →</LinkMono>
          </div>
          <Terminal
            title="verify"
            lines={
              pkg
                ? [
                    { kind: "cmd", text: `shasum -a 256 xflow-${version}.pkg` },
                    { kind: "cmd", text: "codesign -dv --verbose=2 /usr/local/bin/xflowd" },
                    { kind: "muted", text: "# look for Identifier=com.xflow.xflowd" },
                  ]
                : [
                    { kind: "cmd", text: "git rev-parse HEAD", note: "the commit you built" },
                    { kind: "cmd", text: "sha256sum -c SHA256SUMS", note: "release binaries" },
                    { kind: "cmd", text: "codesign -dv --verbose=2 ~/.local/bin/xflowd" },
                    { kind: "muted", text: "# Identifier=com.xflow.xflowd after xflow service install" },
                  ]
            }
          />
        </div>
      </Section>
    </>
  );
}
