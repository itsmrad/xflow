import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";
import { GeistSans } from "geist/font/sans";
import { GeistMono } from "geist/font/mono";
import { SITE_URL } from "@/content/site";
import "./globals.css";

export const metadata: Metadata = {
  metadataBase: new URL(SITE_URL),
  title: { default: "xflow — voice dictation for developers", template: "%s — xflow" },
  description: "Hold Fn and talk. xflow types it. A native Rust daemon that turns speech into clean, correctly spelled text wherever your cursor is.",
  applicationName: "xflow",
  openGraph: { type: "website", siteName: "xflow", images: "/og/home.png" },
  twitter: { card: "summary_large_image" },
};

export const viewport: Viewport = {
  themeColor: [
    { media: "(prefers-color-scheme: light)", color: "#FAFAF9" },
    { media: "(prefers-color-scheme: dark)", color: "#0A0A0A" },
  ],
};

/** Runs before first paint (§5.9): no theme flash, no dependency. */
const THEME = `try{var t=localStorage.getItem('theme'),d=t?t==='dark':matchMedia('(prefers-color-scheme: dark)').matches;document.documentElement.dataset.theme=d?'dark':'light'}catch(_){}`;

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={`${GeistSans.variable} ${GeistMono.variable}`} suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: THEME }} />
      </head>
      <body>
        <a href="#content" className="skip-link t-label rounded-sm bg-volt px-3 py-2 text-volt-ink">
          Skip to content
        </a>
        {children}
      </body>
    </html>
  );
}
