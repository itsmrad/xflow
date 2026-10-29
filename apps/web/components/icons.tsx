import type { SVGProps } from "react";

/** Hand-drawn 16px pixel-grid icons (§3): 1.5px strokes on whole/half pixels, currentColor. */
const base = { width: 16, height: 16, viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", strokeWidth: 1.5, "aria-hidden": true } as const;
type P = SVGProps<SVGSVGElement>;

export const IconCopy = (p: P) => (<svg {...base} {...p}><rect x="5.75" y="5.75" width="8.5" height="8.5" rx="1.5" /><path d="M10.25 3.25v-.5c0-.55-.45-1-1-1h-6.5c-.55 0-1 .45-1 1v6.5c0 .55.45 1 1 1h.5" /></svg>);
export const IconCheck = (p: P) => (<svg {...base} {...p}><path d="m3 8.5 3.25 3.25L13 5" /></svg>);
export const IconArrow = (p: P) => (<svg {...base} {...p}><path d="M3 8h10M9 4l4 4-4 4" /></svg>);
export const IconMenu = (p: P) => (<svg {...base} {...p}><path d="M2 4.75h12M2 8h12M2 11.25h12" /></svg>);
export const IconClose = (p: P) => (<svg {...base} {...p}><path d="m3.5 3.5 9 9m0-9-9 9" /></svg>);
export const IconGitHub = (p: P) => (
  <svg width={16} height={16} viewBox="0 0 16 16" fill="currentColor" aria-hidden {...p}>
    <path d="M8 0a8 8 0 0 0-2.53 15.59c.4.07.55-.17.55-.38v-1.33c-2.23.48-2.7-1.07-2.7-1.07-.36-.92-.89-1.17-.89-1.17-.73-.5.06-.49.06-.49.8.06 1.23.83 1.23.83.71 1.22 1.87.87 2.33.66.07-.52.28-.87.5-1.07-1.78-.2-3.65-.89-3.65-3.95 0-.88.31-1.59.83-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82a7.6 7.6 0 0 1 4 0c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48v2.2c0 .21.15.46.55.38A8 8 0 0 0 8 0Z" />
  </svg>
);
export const IconBook = (p: P) => (<svg {...base} {...p}><path d="M2.75 2.75h4c.69 0 1.25.56 1.25 1.25v9.25c0-.69-.56-1.25-1.25-1.25h-4zM13.25 2.75h-4C8.56 2.75 8 3.31 8 4v9.25c0-.69.56-1.25 1.25-1.25h4z" /></svg>);
export const IconKey = (p: P) => (<svg {...base} {...p}><circle cx="5" cy="11" r="2.75" /><path d="m7 9 6.25-6.25M11 5l2 2M9.5 6.5l1.5 1.5" /></svg>);
export const IconTerminal = (p: P) => (<svg {...base} {...p}><rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.5" /><path d="m4.5 6 2 2-2 2M8.5 10.25h3" /></svg>);
export const IconKeyboard = (p: P) => (<svg {...base} {...p}><rect x="1.75" y="3.75" width="12.5" height="8.5" rx="1.5" /><path d="M4.5 6.5h1M7.5 6.5h1M10.5 6.5h1M5 9.5h6" /></svg>);
export const IconBox = (p: P) => (<svg {...base} {...p}><path d="M8 1.75 13.75 5v6L8 14.25 2.25 11V5zM2.25 5 8 8.25 13.75 5M8 8.25v6" /></svg>);
export const IconWrench = (p: P) => (<svg {...base} {...p}><path d="M9.75 2.25a3.5 3.5 0 0 0-3.2 4.9L2.25 11.5l2.25 2.25 4.35-4.3a3.5 3.5 0 0 0 4.9-3.2l-2 2-2-2z" /></svg>);
export const IconMic = (p: P) => (<svg {...base} {...p}><rect x="5.75" y="1.75" width="4.5" height="8" rx="2.25" /><path d="M3.25 7.5a4.75 4.75 0 0 0 9.5 0M8 12.25v2" /></svg>);
