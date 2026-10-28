import type { ReactNode } from "react";
import { WISPR } from "./site";

/** Hosted provider for Pro (#69). Named publicly: more honest than "our provider" (§13.5). */
export const PRO_PROVIDER = "Groq";

export const FAQ: { q: string; a: ReactNode; text: string }[] = [
  {
    q: "Is the free plan really unlimited?",
    text: "Yes. Free uses your own speech provider key, so you pay the provider directly and xflow never meters you.",
    a: "Yes. Free uses your own speech provider key, so you pay the provider directly and xflow never meters you.",
  },
  {
    q: "What does Pro add?",
    text: "Our provider key and AI cleanup, with nothing to configure. Sign in once from the CLI and dictate. $5/month, unlimited for real use. We rate-limit scripts, not people.",
    a: "Our provider key and AI cleanup, with nothing to configure. Sign in once from the CLI and dictate. $5/month, unlimited for real use. We rate-limit scripts, not people.",
  },
  {
    q: "Does xflow send my audio anywhere?",
    text: `Only to the speech provider you pick, or nowhere in offline mode. On Pro, audio goes through xflow's server to ${PRO_PROVIDER}.`,
    a: `Only to the speech provider you pick, or nowhere in offline mode. On Pro, audio goes through xflow's server to ${PRO_PROVIDER}.`,
  },
  {
    q: "Which apps does it type into?",
    text: "Anything with a text cursor: native apps, browsers, Electron apps like VS Code and Slack, and terminals (single-line, for safety). If a field won't accept text, it's on your clipboard.",
    a: "Anything with a text cursor: native apps, browsers, Electron apps like VS Code and Slack, and terminals (single-line, for safety). If a field won't accept text, it's on your clipboard.",
  },
  {
    q: "Windows? iPhone?",
    text: "Not yet. macOS is first, Linux (GNOME) is in beta. Follow the changelog.",
    a: "Not yet. macOS is first, Linux (GNOME) is in beta. Follow the changelog.",
  },
  {
    q: "Why should I trust a dictation tool with my keyboard?",
    text: "You can read every line. xflow is MIT-licensed, logs no text, refuses password fields, and on macOS needs only Microphone and Accessibility. Input Monitoring is asked for only if the shortcut still isn't heard.",
    a: "You can read every line. xflow is MIT-licensed, logs no text, refuses password fields, and on macOS needs only Microphone and Accessibility. Input Monitoring is asked for only if the shortcut still isn't heard.",
  },
];

export const BILLING_FAQ: { q: string; a: string; text: string }[] = [
  { q: "Can I cancel?", a: "Yes, from the dashboard. Pro stays on until the end of the period you paid for." },
  { q: "What happens to my settings if I cancel?", a: "Nothing. xflow drops back to Free and uses your own key if you've set one." },
  { q: "Annual plan?", a: "Not yet. $5/month is already the low price." },
  { q: "Which payment methods?", a: "Whatever Dodo Payments supports in your country. Cards and local methods are shown at checkout." },
  { q: "Team or company billing?", a: "Not yet. Open an issue on GitHub if you need it, so we can see the demand." },
].map((x) => ({ ...x, text: x.a }));

export type Row = { label: string; xflow: string; wispr: string; note?: 13 };

/**
 * Only rows whose competitor cell is sourced (ledger #13) or common knowledge (closed source).
 * DESIGN.md §9.1.9 rows marked [verify] (BYOK, offline, telemetry, runtime, CLI, learning) stay out until checked.
 */
export const COMPARE: Row[] = [
  { label: "Unlimited plan", xflow: "$5/mo", wispr: "$15/mo ($12/mo billed yearly)", note: 13 },
  { label: "Free plan", xflow: "Unlimited with your own key", wispr: "2,000 words/week", note: 13 },
  { label: "Open source", xflow: "Yes, MIT", wispr: "No" },
  { label: "Platforms", xflow: "macOS, Linux (beta)", wispr: "macOS, Windows, iOS, Android", note: 13 },
];
export const COMPARE_CHECKED = WISPR.checked;

export const FREE_FEATURES = [
  "Unlimited dictation with your own key",
  "10+ speech providers, or a local server",
  "Optional AI cleanup with your own key",
  "Dictionary, replacements, learning, snippets, styles",
  "CLI, TUI, history, export",
  "Offline mode",
];
export const PRO_FEATURES = [
  "Everything in Free",
  "Hosted transcription, no provider account or key",
  "AI cleanup included",
  "Sign in once from the CLI",
  "Cancel any time from your dashboard",
];
