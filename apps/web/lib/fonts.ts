import { Geist, Geist_Mono } from "next/font/google";

/**
 * Geist via next/font: the latin subset (§10.2: two variable files, ≤ 110 KB), downloaded at build
 * time and served from our origin, with a size-adjusted fallback. The LCP heading is sans.
 */
export const sans = Geist({ subsets: ["latin"], variable: "--font-geist-sans", display: "swap" });

export const mono = Geist_Mono({
  subsets: ["latin"],
  variable: "--font-geist-mono",
  display: "swap",
  fallback: ["ui-monospace", "SFMono-Regular", "Menlo", "monospace"],
});
