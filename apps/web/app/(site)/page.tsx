import type { Metadata } from "next";
import Link from "next/link";
import { CodeBlock } from "@/components/code-block";
import { Crossings, Heading, Section } from "@/components/frame";
import { HeroWindow } from "@/components/hero-window";
import { LoopObserver } from "@/components/loop";
import { Bars, ComparisonTable, Faq, faqJsonLd, FeatureGrid, Fig, JsonLd, NumberedRows, PricingCards } from "@/components/marketing";
import { Pill } from "@/components/pill";
import { ProviderGrid } from "@/components/providers";
import { CtaBand, OsButton } from "@/components/cta";
import { Terminal } from "@/components/terminal";
import { Button, Callout, Kbd, LinkMono, Ref } from "@/components/ui";
import { Wave } from "@/components/wordmark";
import { FAQ } from "@/content/marketing";

export const metadata: Metadata = { alternates: { canonical: "/" } };

export const dynamic = "force-static";

const TOTAL = 7;

function Corner({ pos, children }: { pos: string; children: string }) {
  return (
    <span aria-hidden className={`t-mono-label absolute hidden text-fg-subtle md:block ${pos}`}>
      [ {children} ]
    </span>
  );
}

function Diff({ children }: { children: React.ReactNode }) {
  return <div className="t-mono-sm mt-6 overflow-x-auto rounded-md border border-line bg-bg-sunken px-4 py-3">{children}</div>;
}

function Dev({ title, body, children }: { title: string; body: string; children: React.ReactNode }) {
  return (
    <div className="cell flex flex-col">
      <h3 className="t-h3 text-fg">{title}</h3>
      <p className="t-body-sm mt-2 max-w-[48ch] text-fg-muted">{body}</p>
      <div className="mt-auto">{children}</div>
    </div>
  );
}

const DOCTOR = [
  ["config", "ok", "~/.config/xflow/config.toml"],
  ["daemon", "ok", "0.1.0; protocol 2"],
  ["session", "ok", "desktop=macOS; session=macos"],
  ["launchd", "ok", "LaunchAgent user service management"],
  ["privacy", "ok", "Cloud transcription uploads audio to the configured provider"],
  ["dictionary hints", "ok", "All dictionary words fit the provider hint limit"],
  ["Accessibility", "ok", "xflowd text delivery"],
  ["Input Monitoring", "ok", "xflowd keyboard access"],
  ["Shortcut events", "ok", "xflowd has received keyboard events"],
  ["provider", "ok", "groq"],
  ["key", "ok", "groq: xflow keystore (encrypted file)"],
  ["keystore", "ok", "Provider keys are encrypted in xflow's own store, not config.toml"],
  ["Globe key", "ok", "AppleFnUsageType=0"],
] as const;

const CLI = `xflow listen --once --seconds 5   # print text, don't type it
xflow transcribe interview.wav    # files work too
xflow history search postgres --json
xflow history export --format csv > dictations.csv
xflow watch --json | jq .state    # live events`;

export default function Home() {
  return (
    <>
      <JsonLd data={faqJsonLd(FAQ)} />
      <LoopObserver />

      {/* 1. Hero */}
      <section className="frame rule relative -mt-px overflow-hidden" aria-labelledby="hero-title">
        <Crossings spark />
        <div aria-hidden className="bg-cells pointer-events-none absolute inset-0" />
        <Corner pos="left-[var(--cell-p)] top-6">HOLD FN</Corner>
        <Corner pos="right-[var(--cell-p)] top-6">NO TELEMETRY</Corner>
        <Corner pos="left-[var(--cell-p)] bottom-6">.WAV → TEXT</Corner>
        <Corner pos="right-[var(--cell-p)] bottom-6">MIT</Corner>
        <div className="relative px-[var(--cell-p)] pb-16 pt-16 text-center md:pb-20 md:pt-24">
          <Link
            href="/changelog"
            className="t-mono-sm inline-flex h-7 items-center gap-2 rounded-full border border-line-strong bg-bg-raised px-3 text-fg-muted transition-colors duration-[120ms] hover:text-fg"
          >
            <span aria-hidden className="size-1.5 rounded-full bg-ok" />
            <span className="whitespace-nowrap">
              v0.1 · macOS · Linux beta<span className="max-sm:hidden"> · open source</span>
            </span>
            <span aria-hidden>›</span>
          </Link>
          <h1 id="hero-title" className="t-display mx-auto mt-8 max-w-[20ch] text-fg">
            Hold Fn and talk.
            <br />
            <span className="hl">xflow types it.</span>
          </h1>
          <p className="t-lede mx-auto mt-6 max-w-[56ch] text-fg-muted">
            Voice dictation for developers. A native Rust daemon turns speech into clean, correctly spelled text in your editor, terminal and
            chat, wherever your cursor is.
          </p>
          <div className="mt-10 flex flex-col items-center justify-center gap-3 sm:flex-row">
            <OsButton />
            <Button href="/docs" variant="secondary" size="lg" className="w-full sm:w-auto">
              Read the docs →
            </Button>
          </div>
          <p className="t-caption mt-4 text-fg-subtle">Free with your own key · Pro $5/month, coming soon · MIT</p>
          <div className="mt-14 md:mt-16">
            <HeroWindow />
          </div>
        </div>
      </section>

      {/* 2. Proof strip */}
      <section className="frame rule" aria-label="Measurements">
        <Crossings />
        <div className="cells cells-4">
          <Fig n={1} value="11 MB" caption="Idle memory, release daemon (Linux)" claim={1} visual={<Bars seed={1} filled={9} />} />
          <Fig n={2} value="~3 ms" caption="Daemon start, p50 warm (Linux)" claim={2} visual={<Bars seed={2} filled={3} />} />
          <Fig n={3} value="0" caption="Telemetry calls" claim={3} visual={<Bars seed={3} filled={0} />} />
          <Fig n={4} value="10+" caption="Speech providers, plus local" claim={4} visual={<Bars seed={4} filled={32} />} />
        </div>
      </section>

      {/* 3. How it works */}
      <Section id="how-it-works" label={{ n: 1, total: TOTAL, title: "How it works" }} labelledBy="how-title">
        <div className="grid lg:grid-cols-12">
          <div className="section-y lg:col-span-5">
            <div className="px-[var(--cell-p)]">
              <Heading id="how-title" title="Three moves. No window." lede="xflow has no app to switch to. It sits in the background and types where you already are." />
            </div>
            <NumberedRows
              active={0}
              rows={[
                { title: "Hold Fn", body: "Hold the Fn key in any app: editor, terminal, browser, Slack. Double-tap it to go hands-free." },
                { title: "Talk", body: "Say it the way you'd say it to a colleague. Pauses, restarts and “um”s are fine." },
                {
                  title: "Let go",
                  body: (
                    <>
                      Clean text lands at your cursor. If the field can&apos;t take it, it&apos;s on your clipboard and <code className="t-mono-sm text-fg">xflow last</code> has it.
                    </>
                  ),
                },
              ]}
            />
          </div>
          <div className="bg-dots-faint relative flex min-h-[360px] flex-col items-center justify-center gap-8 border-t border-line p-[var(--cell-p)] lg:col-span-7 lg:border-t-0 lg:border-l">
            <div className="flex items-center gap-3">
              <Kbd active>Fn</Kbd>
              <span className="t-mono-label text-fg-subtle">held</span>
            </div>
            <Pill state="listening" timer="0:03" />
            <div className="w-full max-w-[420px] rounded-md border border-line bg-bg-raised p-4 shadow-1">
              <p className="t-mono-label text-fg-subtle">#standup · Slack</p>
              <p className="t-body-sm mt-2 text-fg">
                Shipped the retry fix for the webhook queue, reviewing the Postgres migration next.
                <span aria-hidden className="cursor ml-0.5" />
              </p>
            </div>
          </div>
        </div>
      </Section>

      {/* 4. Dev words */}
      <Section id="dev-words" label={{ n: 2, total: TOTAL, title: "Built for dev words" }} labelledBy="dev-title">
        <div className="section-y !pb-0">
          <div className="px-[var(--cell-p)]">
            <Heading
              id="dev-title"
              title="It knows Postgres isn't “post grass”."
              lede="General dictation guesses at the words you use most. xflow learns your vocabulary and hints it to the provider on every dictation."
            />
          </div>
          <div className="border-t border-line">
            <FeatureGrid>
              <Dev title="Your dictionary" body="Add the names, libraries and acronyms you say every day. They're sent as hints with every recording.">
                <Diff>
                  <span className="text-syn-muted">$ </span>xflow dictionary add Kubernetes Postgres tRPC
                </Diff>
              </Dev>
              <Dev title="Exact replacements" body="When a phrase always comes out wrong, map it once.">
                <Diff>
                  <span className="block text-fg-subtle line-through">− post grass</span>
                  <span className="block text-syn-string">+ Postgres</span>
                </Diff>
              </Dev>
              <Dev
                title="Learns from your fixes"
                body="Correct a word right after dictating and xflow adds it to your dictionary. It tells you when it does, and you can remove it."
              >
                <Diff>
                  <span className="text-ok">Added &apos;Kubernetes&apos; to your dictionary</span>
                </Diff>
              </Dev>
              <Dev title="Per-app styles" body="Casual in Slack, exact in your editor. Styles switch with the app you're in.">
                <Diff>
                  <span className="text-syn-muted">$ </span>xflow styles add chat --apps slack,discord --mode light
                </Diff>
              </Dev>
            </FeatureGrid>
          </div>
          <p className="t-caption border-t border-line px-[var(--cell-p)] py-4 text-fg-subtle">
            Optional AI cleanup fixes punctuation and filler words. If cleanup fails, you get the original transcript, never nothing.
          </p>
        </div>
      </Section>

      {/* 5. Privacy */}
      <Section id="privacy" label={{ n: 3, total: TOTAL, title: "Private by default" }} labelledBy="privacy-title">
        <div className="section-y relative">
          <Wave className="absolute right-0 top-10 h-28 w-[60%] opacity-70" />
          <div className="relative px-[var(--cell-p)]">
            <Heading
              id="privacy-title"
              title="Your voice isn't our dataset."
              lede="xflow is open source, has no telemetry, and keeps audio in memory. Read the code, or run it fully offline."
            />
            <div className="grid gap-x-10 gap-y-6 md:grid-cols-2 lg:grid-cols-3">
              <Callout type="privacy" title="Audio never touches disk.">
                Recordings live in memory and are dropped after transcription. Nothing is logged.
                <Ref n={5} />
              </Callout>
              <Callout type="privacy" title="No telemetry. None.">
                The daemon makes no network calls except to the speech and cleanup providers you chose.
                <Ref n={3} />
              </Callout>
              <Callout type="privacy" title="Offline mode.">
                Point xflow at a local whisper.cpp server and set <code>privacy.offline = true</code>. Non-local endpoints are refused.
                <Ref n={6} />
              </Callout>
              <Callout type="privacy" title="History stays local.">
                A private SQLite file on your machine, stored as plain text. Turn it off with one setting.
                <Ref n={7} />
              </Callout>
              <Callout type="privacy" title="Secure fields are off-limits.">
                On macOS, password fields refuse dictation. Multi-line text is never auto-pasted into a terminal.
                <Ref n={8} />
              </Callout>
              <Callout type="privacy" title="MIT licensed.">
                Every line that touches your audio is on GitHub.
                <Ref n={9} />
              </Callout>
            </div>
            <div className="mt-10 border-t border-line pt-6">
              <Callout type="note" title="On Pro (coming soon),">
                audio goes through xflow&apos;s server to our speech provider and isn&apos;t stored. We keep a usage record (time, duration,
                size) for billing and abuse limits.
              </Callout>
            </div>
          </div>
        </div>
      </Section>

      {/* 6. Native */}
      <Section id="native" label={{ n: 4, total: TOTAL, title: "Native" }} labelledBy="native-title">
        <div className="section-y !pb-0">
          <div className="px-[var(--cell-p)]">
            <Heading
              id="native-title"
              title="A daemon, not a browser in a trench coat."
              lede="xflow is a Rust binary with no Electron, no WebView and no model running in the background. It starts in milliseconds and idles at the size of a photo."
            />
          </div>
          <div className="border-t border-line">
            <div className="flex flex-col">
              <div className="cells cells-4 lg:!grid-cols-2">
                <Fig n={1} value="11 MB" caption="Idle RSS" claim={1} visual={<Bars seed={1} filled={9} />} />
                <Fig n={2} value="~3 ms" caption="Daemon start, p50 (warm)" claim={2} visual={<Bars seed={2} filled={3} />} />
              </div>
              <p className="t-caption border-t border-line px-[var(--cell-p)] py-4 text-fg-subtle">
                Measured on Linux (release build). macOS figures come with the signed release.
              </p>
            </div>
            <div className="cell border-t border-line">
              <Terminal
                title="xflow doctor"
                lines={[
                  { kind: "cmd", text: "xflow doctor" },
                  { kind: "muted", text: "CHECK             RESULT  DETAIL / FIX" },
                  ...DOCTOR.map(([c, r, d]) => ({
                    kind: "out" as const,
                    text: (
                      <>
                        {c.padEnd(18)}
                        <span className="text-ok">{r.padEnd(8)}</span>
                        {d}
                      </>
                    ),
                  })),
                ]}
              />
            </div>
          </div>
          <ul className="cells cells-3 border-t border-line">
            <li className="cell t-body-sm text-fg-muted">The mic opens only while you record.</li>
            <li className="cell t-body-sm text-fg-muted">
              A silent recording gets “Didn&apos;t catch anything”, not a hallucination.
              <Ref n={11} />
            </li>
            <li className="cell t-body-sm text-fg-muted">
              Your text survives: if delivery fails, <code className="t-mono-sm text-fg">xflow last</code> recovers it.
            </li>
          </ul>
        </div>
      </Section>

      {/* 7. Providers */}
      <Section id="providers" label={{ n: 5, total: TOTAL, title: "Your key or ours" }} labelledBy="providers-title">
        <div className="section-y !pb-0">
          <div className="px-[var(--cell-p)]">
            <Heading id="providers-title" title="Bring any key. Or none." lede="Free means your own provider key, unlimited, forever. Pro, coming soon, means ours, with nothing to set up." />
          </div>
          <ProviderGrid />
          <div className="grid items-center gap-6 border-t border-line p-[var(--cell-p)] md:grid-cols-2">
            <CodeBlock filename="config.toml" lang="toml" code={`[stt]\nprovider = "groq"\nmodel = "whisper-large-v3-turbo"`} />
            <div>
              <p className="t-mono text-fg">
                <span className="text-syn-muted">$ </span>xflow key set groq
              </p>
              <p className="t-caption mt-2 text-fg-subtle">Hidden input. Never printed or logged.</p>
            </div>
          </div>
        </div>
      </Section>

      {/* 8. Keyboard first */}
      <Section id="cli" label={{ n: 6, total: TOTAL, title: "Keyboard first" }} labelledBy="cli-title">
        <div className="section-y">
          <div className="px-[var(--cell-p)]">
            <Heading
              id="cli-title"
              title="Everything is a command."
              lede="A full CLI for scripts and a TUI for everything else. Pipe transcripts, search history, edit your dictionary, without leaving the terminal."
            />
          </div>
          <div className="grid gap-6 px-[var(--cell-p)]">
            <div>
              <CodeBlock filename="shell" lang="sh" code={CLI} />
              <LinkMono href="/docs/reference/cli" className="mt-4 inline-block">
                Full CLI reference →
              </LinkMono>
            </div>
            <div>
              <HelpShot />
            </div>
          </div>
        </div>
      </Section>

      {/* 9. Compare */}
      <Section id="compare" label={{ n: 7, total: TOTAL, title: "xflow vs Wispr Flow" }} labelledBy="compare-title">
        <div className="section-y px-[var(--cell-p)]">
          <Heading id="compare-title" title="A third of the price. All of the words." lede="Wispr Flow is good. xflow is built for people who live in a terminal, and it costs less." />
          <ComparisonTable />
          <LinkMono href="/compare/wispr-flow" className="mt-6 inline-block">
            Full comparison →
          </LinkMono>
        </div>
      </Section>

      {/* 10. Pricing teaser */}
      <Section labelledBy="pricing-title">
        <div className="flex flex-wrap items-end justify-between gap-4 px-[var(--cell-p)] pt-[var(--section-y)] pb-10">
          <h2 id="pricing-title" className="t-h2 max-w-[28ch] text-fg">
            Free with your key. $5 when you&apos;d rather not.
          </h2>
          <LinkMono href="/pricing">See pricing →</LinkMono>
        </div>
        <div className="border-t border-line">
          <PricingCards compact />
        </div>
      </Section>

      {/* 11. FAQ */}
      <Section labelledBy="faq-title">
        <div className="section-y grid gap-10 px-[var(--cell-p)] lg:grid-cols-12">
          <h2 id="faq-title" className="t-h2 text-fg lg:col-span-4">
            Questions
          </h2>
          <div className="lg:col-span-8">
            <Faq items={FAQ} />
          </div>
        </div>
      </Section>

      <CtaBand />
    </>
  );
}

/** Real `xflow --help` output (first lines), not a mock. */
function HelpShot() {
  const cmds: [string, string][] = [
    ["setup", "Set up a provider, microphone and optional desktop integration"],
    ["listen", "Print dictation to stdout without changing the desktop"],
    ["transcribe", "Transcribe an audio file without a running daemon"],
    ["history", "Browse, search and export local dictation history"],
    ["dictionary", "Manage vocabulary and text replacements"],
    ["snippets", "Manage spoken shortcuts that expand to saved text"],
    ["styles", "Manage cleanup styles selected by focused application"],
    ["watch", "Subscribe to daemon events until interrupted"],
    ["doctor", "Diagnose configuration and desktop dependencies without recording"],
    ["tui", "Open the terminal control centre (also plain `xflow` in a terminal)"],
  ];
  return (
    <Terminal
      title="xflow --help"
      lines={[
        { kind: "cmd", text: "xflow --help" },
        { kind: "muted", text: "Commands:" },
        ...cmds.map(([c, d]) => ({ kind: "out" as const, text: <>{"  "}<span className="text-syn-keyword">{c.padEnd(12)}</span><span className="text-fg-muted">{d}</span></> })),
        { kind: "muted", text: "  … 20 more. Explore: xflow <command> --help" },
      ]}
    />
  );
}
