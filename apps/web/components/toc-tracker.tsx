"use client";

import { useEffect } from "react";

/**
 * Marks the TOC link of the heading being read (§7.21) with aria-current, in ~20 lines instead of
 * Fumadocs' client bundle: the last heading whose top has scrolled under the 64 px header. Driven by a
 * passive, frame-throttled scroll listener, so jumps (PageDown, End, scrollTo) are caught too.
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
    let frame = 0;
    const onScroll = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(mark);
    };
    addEventListener("scroll", onScroll, { passive: true });
    mark();
    return () => {
      removeEventListener("scroll", onScroll);
      cancelAnimationFrame(frame);
    };
  }, []);
  return null;
}
