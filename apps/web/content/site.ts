export const REPO = "https://github.com/itsmrad/xflow";
/** Temporary until xflow has its own domain. Every page links support through this. */
export const SUPPORT_EMAIL = "itsmrad+xflow@gmail.com";
export const blob = (path: string) => `${REPO}/blob/main/${path}`;

/** Never hard-code a domain (§3). Vercel sets VERCEL_PROJECT_PRODUCTION_URL on every build. */
export const SITE_URL = (
  process.env.SITE_URL ??
  process.env.NEXT_PUBLIC_APP_URL ??
  (process.env.VERCEL_PROJECT_PRODUCTION_URL ? `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}` : "http://localhost:3000")
).replace(/\/$/, "");

export const NAV = [
  { href: "/#how-it-works", label: "Product" },
  { href: "/pricing", label: "Pricing" },
  { href: "/docs", label: "Docs" },
  { href: "/changelog", label: "Changelog" },
  { href: "/download", label: "Download" },
] as const;

export const FOOTER = [
  { title: "Product", links: [["/download", "Download"], ["/pricing", "Pricing"], ["/compare/wispr-flow", "Compare"]] },
  { title: "Docs", links: [["/docs/getting-started/install", "Quickstart"], ["/docs/reference/cli", "CLI reference"], ["/docs/providers", "Providers"]] },
  { title: "Project", links: [[REPO, "GitHub"], ["/changelog", "Changelog"], [`${REPO}/issues/new`, "Report an issue"]] },
  { title: "Legal", links: [["/privacy", "Privacy"], ["/terms", "Terms"], ["/security", "Security"]] },
] as const;

/** Wispr Flow facts (claims ledger #13), wisprflow.ai/pricing. */
export const WISPR = { checked: "2026-10-29", url: "https://wisprflow.ai/pricing" } as const;
