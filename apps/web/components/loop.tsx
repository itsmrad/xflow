"use client";

import { useEffect } from "react";

/** One shared IntersectionObserver: loops play only while on screen (§5.6 rule 3). */
export function LoopObserver() {
  useEffect(() => {
    const io = new IntersectionObserver((entries) => {
      for (const e of entries) e.target.toggleAttribute("data-playing", e.isIntersecting);
    });
    document.querySelectorAll("[data-loop]").forEach((el) => io.observe(el));
    return () => io.disconnect();
  }, []);
  return null;
}
