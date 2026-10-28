import type { Metadata } from "next";
import type { ReactNode } from "react";
import { ClerkProvider } from "@clerk/nextjs";
import { GeistSans } from "geist/font/sans";
import { GeistMono } from "geist/font/mono";
import "./globals.css";

export const metadata: Metadata = {
  title: "xflow",
  description: "Voice dictation for developers.",
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <ClerkProvider>
      <html lang="en" className={`${GeistSans.variable} ${GeistMono.variable}`}>
        <body>{children}</body>
      </html>
    </ClerkProvider>
  );
}
