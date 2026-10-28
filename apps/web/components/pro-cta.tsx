"use client";

import Link from "next/link";
import { useSyncExternalStore } from "react";
import { buttonClass } from "./ui";

const signedIn = () => /(?:^|; )__client_uat=(?!0(?:;|$))\d+/.test(document.cookie);

/** §7.15 state-aware Pro CTA in a fixed box; signed-out → sign up first, then checkout. */
export function ProCta({ size = "lg", label = "Get Pro" }: { size?: "md" | "lg"; label?: string }) {
  const on = useSyncExternalStore(() => () => {}, signedIn, () => false);
  return (
    <Link href={on ? "/checkout" : "/sign-up?redirect_url=/checkout"} className={buttonClass("primary", size, "w-full sm:w-[168px]")}>
      {label}
    </Link>
  );
}
