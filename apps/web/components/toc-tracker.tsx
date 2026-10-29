"use client";

import { useEffect } from "react";

/**
 * Marks the TOC link of the heading being read (§7.21) with aria-current, in ~20 lines instead of
 * Fumadocs' client bundle. The IntersectionObserver only signals that something crossed the top band;
 * the active heading is the last one whose top has scrolled above it (below the 64 px header).
 */
export function TocTracker() {
  useEffect(() => {
    const links = [...document.querySelectorAll<HTMLAnchorElement>('nav[aria-label="On this page"] a')];
    const heads = links.map((a) => document.getElementById(a.hash.slice(1)));
    const mark = () => {
      let active = 0;
      heads.forEach((h, i) => h && h.getBoundingClientRect().top < 96 && (active = i));
      links.forEach((a, i) => a.toggleAttribute("aria-current", i === active) && a.setAttribute("aria-current", "location"));
    };
    const io = new IntersectionObserver(mark, { rootMargin: "-64px 0px -60% 0px" });
    heads.forEach((h) => h && io.observe(h));
    mark();
    return () => io.disconnect();
  }, []);
  return null;
}
