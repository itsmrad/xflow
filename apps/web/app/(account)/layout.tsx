import type { Metadata } from "next";
import type { ReactNode } from "react";
import { ClerkProvider } from "@clerk/nextjs";
import { SiteFooter } from "@/components/site-footer";
import { SiteHeader } from "@/components/site-header";

export const metadata: Metadata = { robots: { index: false, follow: false } };

/** Clerk themed to the tokens (§9.6). Clerk loads only on these routes, never on marketing or docs. */
const appearance = {
  variables: {
    colorPrimary: "#C6F135",
    colorPrimaryForeground: "#0A0A0A",
    colorBackground: "var(--bg-raised)",
    colorForeground: "var(--fg)",
    colorMutedForeground: "var(--fg-muted)",
    colorInput: "var(--bg-raised)",
    colorInputForeground: "var(--fg)",
    colorNeutral: "var(--fg)",
    colorDanger: "var(--rec)",
    colorBorder: "var(--line-input)",
    borderRadius: "6px",
    fontFamily: "var(--font-geist-sans)",
    fontFamilyButtons: "var(--font-geist-sans)",
  },
  elements: {
    cardBox: { boxShadow: "none", border: "1px solid var(--line)", borderRadius: "10px" },
    card: { boxShadow: "none" },
    footer: { background: "var(--bg-raised)" },
    // Volt is a fill, never text on light (§5.1): links use --volt-text.
    footerActionLink: { color: "var(--volt-text)", fontWeight: 500 },
    formFieldAction: { color: "var(--volt-text)" },
    identityPreviewEditButton: { color: "var(--volt-text)" },
    // Clerk draws this border with a shadow that disappears on dark backgrounds.
    socialButtonsBlockButton: { border: "1px solid var(--line-input)", boxShadow: "none" },
  },
};

export default function AccountLayout({ children }: { children: ReactNode }) {
  return (
    <ClerkProvider appearance={appearance}>
      <SiteHeader />
      <main id="content">{children}</main>
      <SiteFooter />
    </ClerkProvider>
  );
}
