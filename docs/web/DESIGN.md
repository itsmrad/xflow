# xflow website: design system and page specs

Status: proposed · Owner: web · Builds into `apps/web` (Next.js) · Build issue: #73 "Website: production site"
Backend contract: issue #69 (Clerk auth, device flow, Dodo billing, hosted proxy). This doc covers what users see. #69 covers what the server does.

This is the build spec for the production site. Match it closely. Where this doc says **TBD** or **[verify]**, stop and ask rather than invent. Never ship a number or a competitor claim that isn't in the [claims ledger](#11-claims-ledger).

---

## 0. Summary

- **Look:** a light *and* dark "engineering paper" site. A visible 1px grid frame (vertical rails, full-bleed horizontal rules, `+` marks where they cross), dashed dividers inside cells, mono labels like `[ 02 / 07 ] · PRIVACY`, and real terminal and pill components as the hero art. Layout ideas come from firecrawl.dev, the restraint from exa.ai, and the mono texture from opencode, warp and ghostty.
- **Accent:** **Volt** `#C6F135`, an acid lime. It is used as a *fill* (primary button, highlighter, active states) and never as text on a light background. A red **Rec** dot is the only other colour and appears only while something is recording. This keeps xflow clear of firecrawl's orange and exa's blue.
- **Type:** Geist Sans for headings and body, Geist Mono for labels, numbers, code and anything a machine says. Both are variable woff2, self-hosted through `next/font`.
- **Stack:** Next.js App Router (current stable, ≥ 16) (RSC, static by default) · Tailwind CSS v4 (CSS-first `@theme`) · `geist` font package · Fumadocs (MDX docs and search) · Shiki at build time · Clerk · Dodo Payments · Vercel. No component library, no animation library, no CSS-in-JS.
- **Budgets:** Lighthouse ≥ 95 in all four categories on every public route (mobile profile). CLS = 0.00 by construction. LCP ≤ 1.8 s, INP ≤ 200 ms, ≤ 90 KB gzipped first-load JS on marketing routes.

---

## 1. Research: what we took and why

All values below were pulled from the live sites on 2026-10-29: computed CSS from their `_next` stylesheets, plus headless Chrome screenshots at 1440 px.

### 1.1 firecrawl.dev (the primary reference)

| Aspect | What they do | What we take |
|---|---|---|
| Frame | A centred container of **1112 px** (`--container-width`, padding variant 1144, gutter 16, mobile gutter 10) with **1 px vertical rails** running the full page height. A full-bleed **horizontal rule** runs at every section boundary. Small `+`/sparkle glyphs mark where rails and rules cross. | Yes. This is the signature. Ours is 1200 px (§4). |
| Hero grid | A field of ~100 px square cells behind the hero, drawn in `--border-faint`. Mono bracket labels sit in the corners (`[ 200 OK ]`, `[ SCRAPE ]`, `[ .JSON ]`, `[ .MD ]`). Two orange 4-point sparkles sit on grid intersections. | Yes, with our own labels (`[ HOLD FN ]`, `[ .WAV → TEXT ]`, `[ NO TELEMETRY ]`, `[ MIT ]`). |
| Section labels | `[ 02 / 07 ] · POWER YOUR AGENT` in Roboto Mono, 12 px, uppercase. The current index is in accent and the total is muted. | Yes, verbatim pattern. |
| Headline | Two lines, Suisse 500, 60/64, tracking −0.3 px. Line 2 is in the accent colour ("web data and beyond"). | Pattern yes, colour no. Our line 2 gets a Volt highlighter so it reads in both themes. |
| Type scale | Named utilities: `title-h1` 60/64, `h2` 52/56, `h3` 40/44, `h4` 32/36, `h5` 24/32; `body-large` 18/26, `body-medium` 16/24, `body-small` 15/22, `x-small` 12–13/20; `label-*` use weight **450**; `mono-medium` 14/22, `mono-small` 13/20 (500), `mono-x-small` 12/16. | The structure: separate title, body, label and mono roles, and a 450 label weight. |
| Colour | `--background-base #f9f9f9` / dark `#0a0a0a`; `--surface #fff` / `#171717`; borders `faint #ededed`, `muted #e8e8e8`, `loud #e6e6e6` (dark `#2a2a2a`, `#333`, `#404040`). Heat ramp `#fa5d19` at alpha 4–100 %. Neutral alpha ramp `black-alpha-1…88`. | The *method*: one accent with an alpha ramp, plus neutral alpha borders that work on any surface. Not the orange. |
| Code blocks | Tabbed language switcher (Python / Node.js / cURL / CLI) with a "Copy code" button. Line numbers in muted mono. Window chrome uses **outline** circles, not traffic-light colours. Response pane labelled `[ .JSON ]`. | Yes. Outline-circle chrome reads as engineered, not "fake macOS". |
| Feature cells | Three equal cells split by 1 px rules. Each has a 16 px pixel-grid icon, a title, a two-line body and an accent "Learn more ›". | Yes. |
| Radius | 8 px is the most common, then 4, 10, 12, 16. Pills use 999. | 6 px for controls, 10 px for panels, 999 for the pill. |
| Motion | `transition-duration .2s`, `ease`. Almost nothing else. | Yes: fast, few, purposeful. |
| Texture | ASCII and dot-matrix art in muted grey behind sections, plus dotted underlines drawn as SVG data URIs. | Yes. An ASCII *waveform* field is our version. |

### 1.2 exa.ai

| Aspect | What they do | What we take |
|---|---|---|
| Restraint | A white page with a serif display (Arizona), one black CTA (`radius 2px`) and a single product widget as the hero. | Fewer things per screen, and one product widget as the hero. |
| Micro-labels | Tiny letter-spaced mono caps (`LATENCY`, `CATEGORY`, `OUTPUT`). Latency chips show values in mono inside the label (`Lite 450ms`, `Auto 1s`, `Deep ~10s`). | Yes. This is exactly how we show provider and model choices and timings. |
| Tokens | `--hairline: rgba(16,19,33,.12)`, `--muted-ink: rgba(16,19,33,.62)`, `--nav-height 60`, containers 1040–1100 px, `--resize-ease: cubic-bezier(.22,1,.36,1)` over 300 ms. | The alpha hairline idea and that ease curve for anything that resizes. |
| Fonts | ABC Diatype / Suisse Intl for sans, Geist Mono / Suisse Intl Mono for mono, Arizona serif for display. | Geist Mono confirmed as a credible peer choice. Geist Sans stands in for Diatype (free, variable, same neutrality). |

### 1.3 Terminal-aesthetic references

| Site | Pattern worth stealing | Use it for |
|---|---|---|
| **warp.dev** | A faint **dotted background** inside the frame. Section heads written as code comments (`# SDLC COVERAGE [ONLINE]`). **Numbered rows** `01`…`06` with a 1 px rule between them and the active row tinted. Ticks on the frame edges. Vertical side text `WARP · HOME · 2026`. | Numbered "how it works" rows. Dotted fill for the hero and the CTA band. |
| **opencode.ai** | The whole page is **IBM Plex Mono**, dark `#121110`-ish, and the frame is a single bordered column. "**Fig 1. 220K GitHub Stars**" captions under line, dot-matrix and bar-code mini charts. FAQ items prefixed by a thin `+`. Privacy callout prefixed `[*]`. | The "Fig N." stat strip, `[*]` callouts, and `+` FAQ. |
| **ghostty.org** | The hero is literally the product: a terminal window rendering ASCII art (JetBrains Mono), with two plain outlined buttons below (Download / Documentation). | The hero shows the real pill and a real `xflow watch` stream, not an illustration. |
| **charm.sh** | Personality via playful labels (`Glossiness: Very`), key–value chips. | Key–value "spec chips" on the download page (`Idle RAM 11 MB`). Their saturated purple and 3D mascots are **not** our tone. |
| **zed.dev** | Thin vertical rails at the frame edge on a dark ground. Editor-accurate syntax colours. | Confirms the rail system works in dark mode. |
| Dribbble / Pinterest / style libraries (searched "terminal aesthetic developer tool landing") | Recurring good ideas: hairline-bordered tiles on near-black, a monospace information layer at 11–16 px, terminal-styled install boxes as the primary CTA, "real compiling code as the hero". Recurring bad ideas: neon glow, CRT scanlines, matrix green, glitch effects. | Keep the information-dense mono layer. Ban glow, scanlines, phosphor green and glitch. |

**Reverse-engineering tooling.** We evaluated GitHub's "REA – Reverse Engineer Anything" (`morluto/rea`, MIT). It works by installing an MCP server and workflow files into the agent's configuration, and its strength is binaries and Electron apps. For two public marketing sites, plain `curl` of the shipped CSS plus headless-Chrome screenshots gave the same facts (exact tokens and type scales) without changing anyone's agent setup, so we did not use REA here.

### 1.4 Wispr Flow (positioning target)

Pricing, from wisprflow.ai/pricing on 2026-10-29:

| Plan | Price | Limits |
|---|---|---|
| Free | — | 2,000 words/week on desktop |
| Pro | $15/user/mo, or $12/mo billed annually | — |
| Growth | $23/user/mo, or $18/mo billed annually | — |
| Enterprise | custom | — |

xflow Free is BYOK and unlimited. xflow Pro is **$5/mo** and unlimited. That makes Pro a third of Wispr Pro's monthly price, and Free is uncapped. These are facts and we can say them.

---

## 2. Principles

1. **The product is the illustration.** Every hero and section visual is a real component: the recording pill, a terminal stream, a TUI capture, a dictionary diff. No stock 3D, no blobs, no mascots.
2. **Show the grid.** Structure is visible: rails, rules, `+` marks and cells. The grid is decoration *and* the layout, so nothing floats off it.
3. **Mono means machine.** Anything a computer says or measures is set in Geist Mono: numbers, units, commands, keys, labels, states and timestamps. Human sentences are set in Geist Sans.
4. **One accent, used like a highlighter.** Volt marks the single most important thing on a screen. If two things are Volt, one of them is wrong.
5. **Honest numbers only.** Every figure links to its source (benchmark JSON or doc), via a "Fig." caption or a footnote. We beat Wispr Flow on facts, not adjectives.
6. **Fast is a feature.** The site must feel like the daemon: static HTML, almost no JS and no layout shift. A slow site for a "fast" tool undermines the pitch.

---

## 3. Stack (recommendation)

| Layer | Choice | Why / notes |
|---|---|---|
| Framework | **Next.js (current stable, ≥ 16), App Router**, TypeScript strict, in `apps/web` (Bun workspace) | Matches #69. RSC plus static generation gives zero JS for most marketing markup. Use `export const dynamic = 'force-static'` on marketing and docs routes. Only `/dashboard`, `/device` and `/checkout/*` are dynamic. |
| Styling | **Tailwind CSS v4** (`@import "tailwindcss"` with an `@theme` block, see §5.8) | CSS-first tokens with no `tailwind.config.js`. Tokens live in one `app/globals.css`. |
| Fonts | **`geist`** npm package (`GeistSans`, `GeistMono`) via `next/font` | Self-hosted, variable, `display: swap` with automatic `size-adjust` fallback metrics, so font swap causes no CLS. Fallback: JetBrains Mono, only if Geist Mono's `0`/`O` or `l`/`1` disambiguation fails review. |
| Docs | **Fumadocs** (`fumadocs-core`, `fumadocs-ui`, `fumadocs-mdx`) | MDX, sidebar from the file tree, TOC and static Orama search with no external service. Restyle through our tokens (§9.4). The alternative is plain `@next/mdx` plus a hand-built sidebar, but then we would own search ourselves. |
| Code highlighting | **Shiki** at build time (Fumadocs uses it already), with a custom theme mapped to the syntax tokens in §5.1 | Zero runtime JS. Light and dark themes are switched with CSS variables. |
| Icons | **Inline SVG**, hand-drawn on a 16 px pixel grid (`components/icons.tsx`) | Fewer than 25 icons. No icon library dependency. Lucide is acceptable only for dashboard chrome if needed. |
| Auth UI | `@clerk/nextjs` `<SignIn/>`, `<SignUp/>`, `<UserButton/>`, `<UserProfile/>` with `appearance.variables` mapped to our tokens (§9.6) | Matches #69. |
| Billing | Dodo Payments hosted checkout and customer portal (redirects) | No card fields on our domain. Dodo is the merchant of record and handles tax. |
| Analytics | **None** at launch. If ever added: Vercel Web Analytics only (cookieless). Never session replay. | "No telemetry" is a product claim, and the site has to live up to it. |
| Hosting | Vercel (`*.vercel.app` until a domain exists, **TBD**) | `SITE_URL` env var everywhere. Never hard-code a domain. |
| Testing | Playwright smoke test per route plus axe-core. Lighthouse CI (`@lhci/cli`) with the budgets from §10 as assertions. | Run through `.tools/heavy-locked`. |

Explicitly **not** used: framer-motion, GSAP, shadcn/Radix (native `<details>`, `<dialog>` and popover cover our needs), next-themes (a 6-line inline script does it, §5.9), any CSS-in-JS, and any carousel library.

---

## 4. Grid system

### 4.1 The frame

```
 viewport edge                                                           viewport edge
 │          ┌──────────────────── 1200px frame ────────────────────┐          │
 │          │ rail                                            rail │          │
─┼──────────+──────────────────────────────────────────────────────+──────────┼─  ← full-bleed rule (section boundary)
 │          │  [ 01 / 07 ] · HOW IT WORKS                          │          │
 │          │                                                      │          │
 │          │  section content (padded 32px inside the rails)      │          │
 │          │                                                      │          │
─┼──────────+──────────────┬───────────────┬───────────────────────+──────────┼─
 │          │ cell         ┆ cell          ┆ cell                  │          │  ┆ = dashed inner divider
 │          │              ┆               ┆                       │          │
─┼──────────+──────────────┴───────────────┴───────────────────────+──────────┼─
```

- `--frame: 1200px`. Above 1264 px the frame is centred with **1 px rails** (`--line`). Below 1264 px, rails sit 16 px in from the viewport edge (`--gutter: 16px`; at ≤ 640 px the gutter is 12 px and the rails stay).
- **Section boundary:** a 1 px `--line` rule that bleeds to both viewport edges. Implement it with `border-top` on `<section>` plus a `::before` of `inset-inline: calc(50% - 50vw)`. Clip with `overflow-x: clip` on `body` (never `hidden`, which breaks `position: sticky`).
- **Crossings:** where a rule meets a rail, render a 9 × 9 px `+` (two 1 px bars, `--line-strong`) centred on the intersection. Implement it as a pseudo-element on the section (`top: -5px; left: -5px` and `right: -5px`), `aria-hidden`. In the hero only, two crossings use a 16 px Volt 4-point spark instead.
- **Inner dividers** between cells are **dashed**: `background-image: linear-gradient(to bottom, var(--line-strong) 50%, transparent 0); background-size: 1px 8px;` (4 px dash, 4 px gap). This gives a pixel-exact, consistent dash in every browser, which `border-style: dashed` does not. Horizontal inner dividers use the same rule rotated (`to right`, `background-size: 8px 1px`).
- **Section label row:** every major section opens with a 48 px tall label row (`[ NN / TT ] · TITLE`), separated from the content by a full-bleed rule, exactly like firecrawl.

### 4.2 Columns and spacing inside the frame

- **12 columns**, gutterless. Cells abut and divide with rules, so the "gutter" is cell padding: 32 px on desktop, 24 on tablet, 20 on mobile.
- Common splits: `12` (full), `6 | 6`, `4 | 4 | 4`, `5 | 7` (text and visual), `3 × 4` (provider tiles).
- **Readable measure:** body text max 64ch. Headings max 20ch for H1 and 28ch for H2.
- Section vertical padding: `--section-y: clamp(64px, 8vw, 112px)`.

### 4.3 Breakpoints (Tailwind v4 defaults, mobile-first)

| Token | Min width | Layout behaviour |
|---|---|---|
| base | 0 | 1 column. Cells stack, and dashed dividers turn horizontal. Nav collapses to a menu button. |
| `sm` | 640 | 2-up tiles. |
| `md` | 768 | `6 / 6` splits allowed. The hero becomes a 2-pane window. |
| `lg` | 1024 | Full nav, `4 / 4 / 4`, `5 / 7`. Docs get a sidebar. |
| `xl` | 1280 | Frame at the full 1200 px. Docs get a sidebar and a TOC. |

### 4.4 Background textures (decorative, `aria-hidden`, CSS only)

| Name | Where | Recipe |
|---|---|---|
| `bg-cells` | Hero | `background-image: linear-gradient(var(--line) 1px, transparent 1px), linear-gradient(90deg, var(--line) 1px, transparent 1px); background-size: 96px 96px;` masked with `mask-image: radial-gradient(ellipse 70% 60% at 50% 40%, #000 40%, transparent 100%)` |
| `bg-dots` | CTA band, pricing, `/device` | `radial-gradient(var(--line-strong) 1px, transparent 1px) 0 0 / 16px 16px` at 60 % opacity |
| `ascii-wave` | Behind the "Fast" and "Privacy" headings | A **static, pre-rendered SVG** of mono glyphs (`· : ▁ ▂ ▃ ▅ ▇`) forming a waveform, fill `--line-strong`. It is a file, not a DOM text node (keeps DOM small and avoids CLS). Under 6 KB gzipped. |

---

## 5. Tokens

### 5.1 Colour

Contrast ratios were computed with the WCAG 2.x formula against the stated background. Every text pair passes **AA** (≥ 4.5:1 normal, ≥ 3:1 for large text and UI). Rules and borders are deliberately low-contrast decoration. Interactive boundaries use `--line-input` (≥ 3:1).

| Token | Light | Dark | Use | Contrast (light / dark) |
|---|---|---|---|---|
| `--bg` | `#FAFAF9` | `#0A0A0A` | Page | — |
| `--bg-raised` | `#FFFFFF` | `#141414` | Cards, terminals, inputs | — |
| `--bg-sunken` | `#F3F3F1` | `#070707` | Code blocks, table header, hovered row | — |
| `--fg` | `#111110` | `#EDEDEC` | Primary text | 18.1 / 16.9 |
| `--fg-muted` | `#5C5C57` | `#A1A19B` | Secondary text, body copy under headings | 6.4 / 7.6 |
| `--fg-subtle` | `#6F6F6A` | `#85857F` | Captions, line numbers, `/ 07` in labels | 4.8 / 5.3 (≥ 4.5 on every surface) |
| `--line` | `#E4E4E0` | `#262625` | Rails, rules, cell borders | decorative |
| `--line-strong` | `#C9C9C4` | `#3A3A38` | `+` crossings, dashed dividers, dots | decorative |
| `--line-input` | `#8C8C86` | `#6A6A66` | Input and checkbox borders | 3.4 / 3.4 (vs `--bg-raised`) |
| `--volt` | `#C6F135` | `#C6F135` | **Accent fill**: primary button, highlighter, active tab underline, progress | ink on volt 15.1 |
| `--volt-ink` | `#0A0A0A` | `#0A0A0A` | Text and icons *on* Volt | — |
| `--volt-text` | `#3F6212` | `#C6F135` | Accent as *text*: section index `01`, inline links in mono labels | 6.8 / 15.1 |
| `--volt-soft` | `#C6F13529` | `#C6F1351F` | Highlighter behind H1 line 2, selected row tint | — |
| `--focus` | `#4D7C0F` | `#C6F135` | 2 px focus ring | 5.0 / 15.1 |
| `--rec` | `#C62A2F` | `#FF6369` | **Only** the recording dot, destructive actions, error text | 5.3 / 6.8 |
| `--ok` | `#2B7A3D` | `#5BD47A` | "Delivered", success states | 5.1 / 10.5 |
| `--warn` | `#8A5A00` | `#F0C550` | Warnings and callouts | 5.7 / 12.1 |

Rule: **Volt is never text on a light background** (it would be about 1.2:1). In light mode, accent text uses `--volt-text`. In both modes, accent *emphasis* inside headings is the highlighter (`--volt-soft` underlay), not coloured glyphs.

**Syntax tokens** (Shiki theme and the terminal component):

| Token | Light | Dark | Used for |
|---|---|---|---|
| `--syn-fg` | `#111110` | `#EDEDEC` | default |
| `--syn-muted` | `#6F6F6A` | `#85857F` | comments, line numbers, `$` prompt |
| `--syn-keyword` | `#6B3FD4` | `#B49CFF` | keywords, CLI subcommands |
| `--syn-string` | `#3F6212` | `#C6F135` | strings, transcribed text |
| `--syn-number` | `#9A4A00` | `#FFB86B` | numbers, durations |
| `--syn-key` | `#1F5FBF` | `#8DB8FF` | object keys, flags (`--json`) |
| `--syn-punct` | `#6F6F6A` | `#85857F` | punctuation |

Every syntax colour is ≥ 4.5:1 on `--bg`, `--bg-raised` and `--bg-sunken` in its theme. The lowest is `--syn-muted`/`--syn-punct` light on sunken at 4.55.

The **alpha ramps** (firecrawl method) cover hover and pressed tints on any surface: `--ink-a4 / a8 / a12 / a16` are `color-mix(in oklab, var(--fg) 4% / 8% / 12% / 16%, transparent)`.

### 5.2 Typography

Families:

- `--font-sans: var(--font-geist-sans), ui-sans-serif, system-ui, sans-serif`
- `--font-mono: var(--font-geist-mono), ui-monospace, "SFMono-Regular", Menlo, monospace`

Font features: `font-variant-numeric: tabular-nums` on every number in tables, stats and terminals. Check in the styleguide that Geist Mono's `0`/`O` and `1`/`l`/`I` are distinguishable at 12 px. If they aren't, enable the font's slashed-zero stylistic set (check the installed version's feature list) or fall back to JetBrains Mono. Use `text-wrap: balance` on headings and `text-wrap: pretty` on paragraphs.

| Role | Size / line-height (desktop → mobile) | Weight | Tracking | Family |
|---|---|---|---|---|
| `display` (hero H1) | `clamp(44px, 6.2vw, 76px)` / 1.0 | 560 | −0.035em | sans |
| `h1` (page title) | `clamp(36px, 4.6vw, 56px)` / 1.05 | 560 | −0.03em | sans |
| `h2` (section title) | `clamp(28px, 3.4vw, 40px)` / 1.1 | 540 | −0.025em | sans |
| `h3` (cell title) | 20 / 28 | 520 | −0.01em | sans |
| `h4` | 16 / 24 | 520 | 0 | sans |
| `body-lg` (lede) | 18 / 28 (mobile 17 / 26) | 400 | −0.005em | sans |
| `body` | 16 / 26 | 400 | 0 | sans |
| `body-sm` | 14 / 22 | 400 | 0 | sans |
| `label` (buttons, nav, tabs) | 14 / 20 | **450** | 0 | sans |
| `caption` | 13 / 20 | 400 | 0 | sans |
| `mono` (code) | 14 / 22 | 400 | 0 | mono |
| `mono-sm` (terminal, chips, table numbers) | 13 / 20 | 400 / 500 | 0 | mono |
| `mono-label` (section labels, eyebrow, `LATENCY`) | 12 / 16 | 500 | **+0.08em**, uppercase | mono |
| `stat` (Fig numbers) | `clamp(32px, 4vw, 48px)` / 1 | 500 | −0.02em, `tnum` | mono |

Only these sizes exist. If a design needs a new one, add a role here first.

### 5.3 Spacing (4 px base)

`--space-0.5: 2px`, `1: 4`, `1.5: 6`, `2: 8`, `3: 12`, `4: 16`, `5: 20`, `6: 24`, `8: 32`, `10: 40`, `12: 48`, `16: 64`, `20: 80`, `24: 96`, `28: 112`. Tailwind v4's default `--spacing: 0.25rem` already produces this scale, so don't redefine it. Use `p-8` and similar.

Fixed rhythms:

| Element | Spacing |
|---|---|
| Nav height | 64 |
| Section label row | 48 |
| Cell padding | 32 / 24 / 20 |
| Label to H2 | 16 |
| H2 to lede | 16 |
| Lede to content | 40 |
| Stacked CTAs | 12 |

### 5.4 Radius

| Token | Value | Use |
|---|---|---|
| `--radius-xs` | 4 px | chips, kbd, badges |
| `--radius-sm` | 6 px | buttons, inputs, tabs |
| `--radius-md` | 10 px | cards, terminal windows, code blocks, dialogs |
| `--radius-pill` | 999 px | the recording pill, toggles |

Cells inside the frame have **0 radius**, because they are the grid. Radius belongs only to *objects placed on* the grid.

### 5.5 Elevation

The design is flat. Lines do the work, not shadows.

| Token | Value | Use |
|---|---|---|
| `--shadow-0` | `none` | default |
| `--shadow-1` | `0 1px 0 var(--ink-a4), 0 1px 2px var(--ink-a8)` | raised objects (terminal window, hero input) |
| `--shadow-2` | `0 12px 32px -12px var(--ink-a16), 0 1px 2px var(--ink-a8)` | popovers, dialog, mobile menu |

In dark mode, swap the shadows for a 1 px `--line-strong` outline (shadows don't read on `#0A0A0A`).

### 5.6 Motion

| Token | Value |
|---|---|
| `--dur-1` | 120 ms (hover, press) |
| `--dur-2` | 200 ms (tabs, accordions, theme) |
| `--dur-3` | 320 ms (panel resize, dialog) |
| `--ease-out` | `cubic-bezier(.22, 1, .36, 1)` (exa's resize ease, our default) |
| `--ease-in-out` | `cubic-bezier(.65, 0, .35, 1)` |

Rules:

1. Animate only `transform` and `opacity`, plus `grid-template-rows` for accordion height via `0fr → 1fr`. Never animate width, height, top or left.
2. **Every looping animation stops under `prefers-reduced-motion: reduce`** and shows its final, meaningful frame. That means the pill shows a static waveform and the terminal shows the finished stream.
3. Loops pause when off-screen. A single shared `IntersectionObserver` toggles `data-playing` on the element, and CSS uses `animation-play-state`.
4. Motion budget per screen: at most **one** looping animation in view.

Signature motions:

- **Pill waveform:** 9 bars, `scaleY` between 0.25 and 1, staggered `animation-delay`, 900 ms, `steps(6)`. The steps make it look sampled and digital, not smooth.
- **Terminal stream:** lines appear one by one (`opacity` 0 → 1 plus `translateY(4px)` → 0, 200 ms, 700 ms apart). The text already occupies its final layout, so there is no reflow. The typed transcript uses a CSS `steps()` clip-path reveal on a pre-laid-out line.
- **Cursor:** a 2 px × 1em block in `--volt`, `opacity` blink 1 s `steps(1)`.

### 5.7 Z-index

| Token | Value | Use |
|---|---|---|
| `--z-base` | 0 | — |
| `--z-sticky` | 10 | nav |
| `--z-overlay` | 40 | mobile menu |
| `--z-dialog` | 50 | — |
| `--z-toast` | 60 | — |

### 5.8 `app/globals.css` (authoritative token source)

```css
@import "tailwindcss";

@custom-variant dark (&:where([data-theme="dark"], [data-theme="dark"] *));

:root {
  color-scheme: light;
  --bg: #FAFAF9; --bg-raised: #FFFFFF; --bg-sunken: #F3F3F1;
  --fg: #111110; --fg-muted: #5C5C57; --fg-subtle: #6F6F6A;
  --line: #E4E4E0; --line-strong: #C9C9C4; --line-input: #8C8C86;
  --volt: #C6F135; --volt-ink: #0A0A0A; --volt-text: #3F6212; --volt-soft: #C6F13529;
  --focus: #4D7C0F; --rec: #C62A2F; --ok: #2B7A3D; --warn: #8A5A00;
  --syn-fg: #111110; --syn-muted: #6F6F6A; --syn-keyword: #6B3FD4; --syn-string: #3F6212;
  --syn-number: #9A4A00; --syn-key: #1F5FBF; --syn-punct: #6F6F6A;
  --frame: 1200px; --gutter: 16px; --nav-h: 64px;
  --section-y: clamp(64px, 8vw, 112px);
}
:root[data-theme="dark"] {
  color-scheme: dark;
  --bg: #0A0A0A; --bg-raised: #141414; --bg-sunken: #070707;
  --fg: #EDEDEC; --fg-muted: #A1A19B; --fg-subtle: #85857F;
  --line: #262625; --line-strong: #3A3A38; --line-input: #6A6A66;
  --volt-text: #C6F135; --volt-soft: #C6F1351F;
  --focus: #C6F135; --rec: #FF6369; --ok: #5BD47A; --warn: #F0C550;
  --syn-fg: #EDEDEC; --syn-muted: #85857F; --syn-keyword: #B49CFF; --syn-string: #C6F135;
  --syn-number: #FFB86B; --syn-key: #8DB8FF; --syn-punct: #85857F;
}
@media (max-width: 640px) { :root { --gutter: 12px; } }

@theme inline {
  --color-bg: var(--bg); --color-bg-raised: var(--bg-raised); --color-bg-sunken: var(--bg-sunken);
  --color-fg: var(--fg); --color-fg-muted: var(--fg-muted); --color-fg-subtle: var(--fg-subtle);
  --color-line: var(--line); --color-line-strong: var(--line-strong); --color-line-input: var(--line-input);
  --color-volt: var(--volt); --color-volt-ink: var(--volt-ink); --color-volt-text: var(--volt-text);
  --color-volt-soft: var(--volt-soft); --color-focus: var(--focus); --color-rec: var(--rec);
  --color-ok: var(--ok); --color-warn: var(--warn);
  --font-sans: var(--font-geist-sans), ui-sans-serif, system-ui, sans-serif;
  --font-mono: var(--font-geist-mono), ui-monospace, SFMono-Regular, Menlo, monospace;
  --radius-xs: 4px; --radius-sm: 6px; --radius-md: 10px;
  --ease-out: cubic-bezier(.22, 1, .36, 1);
}

html { background: var(--bg); color: var(--fg); }
body { overflow-x: clip; font-family: var(--font-sans); -webkit-font-smoothing: antialiased; }
:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; border-radius: var(--radius-xs); }
::selection { background: var(--volt); color: var(--volt-ink); }
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { animation-duration: 0.01ms !important; animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important; scroll-behavior: auto !important; }
}
```

### 5.9 Theme switching (no flash, no dependency)

Put this inline at the top of `<head>` in `app/layout.tsx`. It must run before first paint. Set `suppressHydrationWarning` on `<html>`.

```html
<script>
  try {
    const t = localStorage.getItem('theme');
    const d = t ? t === 'dark' : matchMedia('(prefers-color-scheme: dark)').matches;
    document.documentElement.dataset.theme = d ? 'dark' : 'light';
  } catch (_) {}
</script>
```

The theme toggle (footer and docs header) is a 3-way segmented control: **System / Light / Dark**. "System" removes the key and follows `matchMedia` changes. The default is System.

---

## 6. Voice and copy rules

- **Plain, specific, dry.** Say "Audio stays in memory and is never written to disk", not "Military-grade privacy".
- **Second person, present tense, short.** One idea per sentence.
- **Numbers carry units and sources.** "11 MB idle memory ⁽¹⁾". The footnote links to the benchmark file.
- **Comparisons are factual and dated.** "Wispr Flow Pro: $15/mo (Oct 2026)". Never "Wispr is slow". Show the table and let it speak.
- **Commands are copyable.** Any `$ …` on the site is real, current and has a copy button.
- **Banned words:** revolutionary, seamless, effortless, blazing, magical, AI-powered (as an adjective), supercharge, unlock, game-changer.
- **Product name:** always lowercase `xflow`, even at the start of a sentence ("xflow types it"). The daemon is `xflowd` and only appears in docs. **Pro** is capitalised.
- **Keys:** `Fn` (with `🌐` only in macOS-specific docs), `⌘V`, `Esc`, written as `<Kbd>` components.

---

## 7. Component inventory

Each component lives in `apps/web/components/`. Server Components are the default. ⚡ marks components that need client JS, and only those get `"use client"`.

### 7.1 `SiteHeader`

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ xflow▍     Product  Pricing  Docs  Changelog  Download     ★ 1.2k  Sign in  [ Get xflow ] │
└──────────────────────────────────────────────────────────────────────────────┘ ← 1px --line, full-bleed
```

- Height `--nav-h` (64). `position: sticky; top: 0`. Background `color-mix(in oklab, var(--bg) 85%, transparent)` with `backdrop-filter: blur(12px)`. Bottom rule is full-bleed.
- **Wordmark:** `xflow` in Geist Mono 500, 18 px, followed by a Volt block cursor `▍` that blinks (it is the only always-on animation in the nav, and it stops under reduced motion). Links to `/`.
- **Links:** `label` style, `--fg-muted`; hover `--fg`. The current page is `--fg` with a 2 px Volt underline 18 px below the baseline, and `aria-current="page"`.
- **GitHub stars:** fetched at **build time** via the GitHub API with ISR `revalidate: 3600`. Show only if ≥ 100. Set in mono-sm. Links to the repo. The width is reserved with `min-width: 6ch` to avoid shift.
- **Auth slot:** signed out shows "Sign in" (ghost button). Signed in shows "Dashboard" plus `<UserButton/>`. Render it client-side ⚡ inside a fixed 96 × 32 box so it never shifts.
- **Primary CTA:** "Get xflow" goes to `/download` (primary button, sm).
- **< lg:** the wordmark, then a "Get xflow" button, then a menu button (`aria-expanded`, `aria-controls`). The menu is a native `popover` panel, full width under the nav, links stacked 48 px tall, `--shadow-2`. Close it on Esc and on navigation.
- **Optional announcement bar** above the nav (32 px, `--bg-sunken`, mono-sm, dismissible ⚡ with `localStorage`). Use it only for releases. It is off by default.

### 7.2 `Button`

| Variant | Background | Text | Border | Hover | Use |
|---|---|---|---|---|---|
| `primary` | `--volt` | `--volt-ink` | none | `filter: brightness(.95)` | One per view |
| `secondary` | `--bg-raised` | `--fg` | 1 px `--line-strong` | bg `--ink-a4` | Second action |
| `ghost` | transparent | `--fg-muted` | none | bg `--ink-a4`, text `--fg` | Nav, tertiary |
| `danger` | transparent | `--rec` | 1 px `--rec` @ 40 % | bg `--rec` @ 8 % | Revoke, cancel plan |
| `link-mono` | — | `--volt-text`, mono-sm | — | underline | "Read the docs →" |

- Sizes: `sm` 32 px tall, 12 px padding-x; `md` 40 px, 16 px; `lg` 48 px, 20 px. Label style, radius 6. An icon slot of 16 px with 8 px gap is optional.
- Pressed state: `transform: translateY(1px)`. Disabled: 40 % opacity plus `aria-disabled`, and the button keeps its focusability when it has a tooltip.
- Render as `<a>` when there's an `href`, otherwise `<button type="button">`. Never a `div`.

### 7.3 `SectionLabel`

```
[ 02 / 07 ] · BUILT FOR DEV WORDS
```

- mono-label. Brackets and `/ 07` use `--fg-subtle`, `02` uses `--volt-text`, the title uses `--fg-muted`.
- Sits in the 48 px label row. Rendered as `<p aria-hidden="true">`, because the section's real heading is the H2.

### 7.4 `Eyebrow` (hero chip)

```
( ● v0.1 · macOS · Linux beta · open source  › )
```

- 28 px tall pill: 1 px `--line-strong`, `--bg-raised`, mono-sm. The leading 6 px dot is `--ok`. Links to `/changelog`.

### 7.5 `Kbd`

```
╭────╮
│ Fn │
╰────╯
```

- `<kbd>`, mono-sm 500, min-width 28 px, height 24 px, padding 0 6 px, radius 4, 1 px `--line-strong`, bottom border 2 px (keycap depth), `--bg-raised`. The active (held) state is a `--volt` fill with `--volt-ink` text, used in the "how it works" animation.

### 7.6 `Badge`

mono-label, 20 px tall, padding 0 6 px, radius 4.

| Variant | Colours |
|---|---|
| `NEW` | `--volt-soft` background, `--volt-text` text |
| `BETA` | `--warn` text on `--warn` @ 12 % |
| `PRO` | `--volt` background, `--volt-ink` text |
| `BYOK`, `STT`, `CLEANUP`, `LOCAL` | `--ink-a8` background, `--fg-muted` text |

### 7.7 `CommandBox` ⚡ (copy only)

```
┌─────────────────────────────────────────────────────────────┐
│ macOS (.pkg)   Linux   From source                          │ ← tabs, label, 2px Volt underline on active
├─────────────────────────────────────────────────────────────┤
│ $ curl -fsSL https://{SITE}/install.sh | sh          [ ⧉ ]  │ ← mono 14, `$` in --syn-muted, not selectable
└─────────────────────────────────────────────────────────────┘
```

- `--bg-raised`, 1 px `--line`, radius 10, `--shadow-1`. Tabs use `role="tablist"`. The panel uses `<pre><code>`.
- The copy button is a 32 × 32 icon button with `aria-label="Copy command"`. On success the icon swaps to ✓ for 1.5 s, and an `aria-live="polite"` region says "Copied". It uses `navigator.clipboard.writeText` and falls back to selecting the text.
- The `$ ` prompt sits in a separate `user-select: none` span, so copying never includes it.
- **The commands must be real.** The content source is `content/install.ts`, one object per method. A method whose installer does not exist yet is **omitted, not shown disabled** (§11 tracks this).

### 7.8 `Terminal`

```
╭───────────────────────────────────────────────────────────╮
│ ○ ○ ○            xflow watch                    [ LIVE ●] │ ← 36px chrome, outline circles (--line-strong)
├───────────────────────────────────────────────────────────┤
│ $ xflow watch                                             │
│ 12:04:31  listening    hold · Fn                          │
│ 12:04:35  processing   4.2 s · groq · whisper-large-v3-turbo │
│ 12:04:36  success      61 chars → Cursor                  │
│ ▍                                                         │
╰───────────────────────────────────────────────────────────╯
```

- `--bg-raised` in both themes (light terminal in light mode, like firecrawl), 1 px `--line`, radius 10, `--shadow-1`. Body padding 20 px, mono-sm. Timestamps use `--syn-muted`. State words are coloured: `listening` `--rec`, `processing` `--syn-number`, `success` `--ok`, `error` `--rec`.
- Props: `title`, `lines: {t, kind, text}[]`, `animate?: boolean`, `lineNumbers?: boolean`.
- **No CLS:** the full content is server-rendered, and animation only reveals it (§5.6). Height is intrinsic to the final content. Real `<pre>` text is accessible. The animated layer is the same DOM, not a duplicate.
- Window circles are outline only. **No** red/yellow/green traffic lights, because they read as a fake Mac.
- The `[ LIVE ● ]` badge is shown only while animating. The dot is `--rec` and pulses (opacity only).
- **State names must match the daemon**: `idle`, `listening`, `processing`, `success`, `error` (`xflow-core::State`). Line text must match real `xflow watch` output formatting. Capture a real run and copy it **[verify before launch]**.

### 7.9 `Pill` (the recording pill, product-accurate)

```
     ╭──────────────────────────╮
     │   ▌▍▌█▌█▌▍▌   0:04       │   listening (bars animate)
     ╰──────────────────────────╯
```

- A CSS recreation of the real overlay (`docs/img/pill-*.png`): 999 radius, `#1F1F1F` fill in **both** themes (it's an OS overlay, so it stays dark), 1 px `#FFFFFF1A` inner stroke, height 40, padding 0 16.
- States, following `State`:

| State | Look |
|---|---|
| `listening` | 9 white bars animating, plus a mono timer |
| `processing` | Bars collapse into three pulsing dots |
| `success` | ✓ in `--ok` and "Typed", fades out after 1.2 s |
| `error` | `--rec` "Didn't catch anything" |
| `hands-free` | A Volt ring around the pill (double-tap mode) |

- Used in the hero (floating over the editor pane), "How it works" and the docs hotkeys page. `role="img"` with an `aria-label` that names the state.

### 7.10 `CodeBlock`

- Build-time Shiki, theme `xflow-light` / `xflow-dark` from the syntax tokens.
- Optional tabs, a filename header (`[ config.toml ]` mono-label right-aligned, firecrawl style), line numbers (`--syn-muted`, `user-select: none`), highlighted lines (`--volt-soft` row background with a 2 px Volt left bar), and a copy button. `overflow-x: auto` with `tabindex="0"` so keyboard users can scroll it.

### 7.11 `FeatureCell` and `FeatureGrid`

```
┌───────────────────────┬┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┬┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┐
│ ▦                     ┆ ▦                     ┆ ▦                     │
│ Title  [NEW]          ┆ Title                 ┆ Title                 │  h3
│ Two lines of body     ┆ Two lines of body     ┆ Two lines of body     │  body-sm --fg-muted
│ copy, max 120 chars.  ┆ copy.                 ┆ copy.                 │
│                       ┆                       ┆                       │
│ [visual slot 16:10]   ┆ [visual slot]         ┆ [visual slot]         │  optional, fixed aspect-ratio
│ Learn more ›          ┆                       ┆                       │  link-mono
└───────────────────────┴┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┴┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┘
```

- The icon is 16 px pixel-grid SVG in `--fg-subtle` and goes `--volt-text` on cell hover.
- Hover: background `--ink-a4`, 120 ms. The whole cell is clickable only when it has a single link (use a stretched-link `::after` and keep the visible link).
- Visual slots always declare `aspect-ratio`.

### 7.12 `NumberedRows` (warp style)

```
┌──────────────────────────────────────────────┐
│ 01  Hold Fn                                  │  ← active row: --bg-sunken, 2px Volt left bar
│     Anywhere your cursor is.                 │
├──────────────────────────────────────────────┤
│ 02  Talk                                     │
├──────────────────────────────────────────────┤
│ 03  Let go                                   │
└──────────────────────────────────────────────┘
```

- The number is mono-sm `--fg-subtle`, the title is h3 and the body is body-sm.
- Paired with a visual pane on the right (`5 / 7` split). Static by default. If it auto-advances on scroll, it uses IntersectionObserver ⚡ and is never time-based, so it doesn't fight the reader.

### 7.13 `Fig` (stat strip, opencode style)

```
  11 MB                  ~3 ms                   0                    10+
  ───────                ───────                 ───────              ───────
  Fig 1. idle memory     Fig 2. daemon start     Fig 3. telemetry     Fig 4. speech
  (Linux, release)       p50, warm               calls                providers
```

- Each figure is a cell. The number is the `stat` role; the caption is mono-sm `--fg-subtle` with "Fig N." in `--fg-muted`. The caption links to its source (§11).
- Above the number there may be a 48 px tall mini-visual: a dot matrix, bar code or sparkline as static SVG, `aria-hidden`.

### 7.14 `ComparisonTable`

```
                         xflow               Wispr Flow
 ──────────────────────────────────────────────────────────
 Price (unlimited)       $5/mo  ← volt-soft  $15/mo ($12/mo yearly)
 Free tier               Unlimited (BYOK)    2,000 words/week
 ...
```

- A real `<table>` with `<caption>` (visually hidden) and `scope` on headers. The xflow column has a `--volt-soft` background, and its header is a PRO-style badge.
- Values: ✓ / ✗ glyphs **always** come with text ("Yes", "No", "macOS, Linux"), never icon-only.
- Each competitor cell links to its source and date (footnote markers). A caption row at the bottom reads "Competitor data from public pages, checked {date}. Tell us if it changed."
- Mobile: two columns (row label plus xflow) with competitor values stacked under each row in `--fg-subtle`. Never scroll the table horizontally.

### 7.15 `PricingCard`

```
┌──────────────────────────────┐ ┌──────────────────────────────┐
│ FREE · BYOK                  │ │ PRO                [PRO]     │  mono-label
│ $0                           │ │ $5 /month                    │  stat + body-sm
│ Your provider key.           │ │ Our key. Nothing to set up.  │
│ Unlimited.                   │ │ Unlimited.                   │
│ [ Download ]                 │ │ [ Get Pro ]  ← primary       │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄   │ │ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄   │
│ ✓ feature                    │ │ ✓ Everything in Free         │
│ ✓ feature                    │ │ ✓ feature                    │
└──────────────────────────────┘ └──────────────────────────────┘
```

- Two cells in a `6 / 6` split on the grid (cells, not floating cards). Pro has a 2 px Volt top border and a `bg-dots` texture at 30 %.
- The price is `stat` role mono. "/month" is body-sm `--fg-muted`.
- The Pro CTA is state-aware ⚡ (inside a fixed-size box, so no shift):

| User state | CTA |
|---|---|
| Signed out | "Get Pro" → `/sign-up?redirect_url=/checkout` |
| Signed in, free | "Get Pro" → `/checkout` |
| Pro | "Manage plan" → `/dashboard#plan` |

### 7.16 `FAQ`

- Native `<details>`/`<summary>`, one per question. The summary is `h4` style with a leading `+` (mono, `--fg-subtle`) that rotates 45° to `×` when open (`transform`, 200 ms). The answer is body `--fg-muted`, max 64ch.
- Animate height with `interpolate-size: allow-keywords` plus `::details-content` transitions where supported. Elsewhere it opens instantly, which is fine.
- Emit `FAQPage` JSON-LD from the same data.

### 7.17 `Callout`

```
[*] Audio stays in memory. It is never saved or logged unless you turn it on.
```

The `[*]` / `[!]` / `[i]` prefix is mono `--fg-subtle`. The variants Note, Privacy, Warning and Danger are distinguished by a left 2 px bar (`--line-strong`, `--volt`, `--warn`, `--rec`). Used in docs and on the marketing privacy section.

### 7.18 `ProviderTile` (the "store" grid)

```
┌┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┐
┆ groq          [STT]┆
┆ whisper-large-v3…  ┆  mono-sm --fg-subtle (default model)
┆ BYOK · Pro         ┆
└┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┘
```

- 4 columns at `xl`, 3 at `lg`, 2 at `sm`. The name is in mono 500. **No third-party logos** at launch, to avoid trademark and brand-guideline work; names in mono look more "CLI store" anyway.
- Links to `/docs/providers/{id}`. The source of truth is a generated `content/providers.json` (§9.3), never hand-typed.

### 7.19 `Footer`

```
─┼────────────────────────────────────────────────────────────────────┼─
 │ Product        Docs            Project          Legal               │
 │ Download       Quickstart      GitHub           Privacy             │
 │ Pricing        CLI reference   Changelog        Terms               │
 │ Compare        Providers       Report an issue  Security            │
 ├┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┤
 │                                                                     │
 │   ██╗  ██╗███████╗██╗      ██████╗ ██╗    ██╗   ← ASCII wordmark,   │
 │   ...                                            SVG, --line-strong  │
 │                                                                     │
 │ © 2026 xflow · MIT licensed     [ System | Light | Dark ]   v0.1.0  │
─┴─────────────────────────────────────────────────────────────────────┴─
```

- Column headers are mono-label `--fg-subtle`. Links are body-sm `--fg-muted`.
- The ASCII wordmark is decorative SVG (`aria-hidden`), full frame width, height fixed via `aspect-ratio`.
- The version string is read at build time from the latest release tag (or `Cargo.toml` workspace version).

### 7.20 Form controls (dashboard, `/device`)

- **Input:** 40 px tall, `--bg-raised`, 1 px `--line-input`, radius 6, body text. Focus uses the `--focus` ring plus border `--focus`. Errors use border `--rec` and a message below in caption `--rec` linked via `aria-describedby`.
- **CodeInput** (device code `XXXX-XXXX`): one `<input>` (not 8 boxes, which break paste, autofill and screen readers) in mono 28/36, letter-spacing 0.3em, `autocomplete="one-time-code"`, `inputmode="text"`, `autocapitalize="characters"`, `maxlength="9"`. It auto-inserts the dash, uppercases, and strips characters outside the device-code alphabet (the server alphabet from #69 omits ambiguous characters; mirror the exact set from `packages/shared` when it exists).
- **Table:** row height 48, header mono-label on `--bg-sunken`, 1 px `--line` row dividers, numbers `tnum` right-aligned.
- **Dialog:** native `<dialog>` with `showModal()`, radius 10, `--shadow-2`, max-width 440, and an Esc / backdrop close. Used for "Revoke token?" and "Cancel Pro?" confirmation (destructive action in `danger`; the focus default is **Cancel**).
- **Toast:** bottom-right, mono-sm, auto-dismiss after 4 s, `role="status"`. Used only to confirm async actions.
- **Skeleton:** `--ink-a4` block **with explicit dimensions equal to the loaded content**. Shimmer is off; use a static tint (reduced motion is the default here).

### 7.21 Docs chrome (Fumadocs, restyled)

| Element | Spec |
|---|---|
| Sidebar | 264 px, sticky under the nav, groups as mono-label, items body-sm. The active item is `--fg` with a 2 px Volt left bar. |
| TOC | 220 px at `xl`, caption size. The active heading is tracked by Fumadocs. |
| Search | `⌘K` / `/` opens a `<dialog>` with static Orama search. |
| Prose | body 16/26, H2 with a full-width `--line` rule above, inline code as mono-sm on `--bg-sunken` (radius 4, padding 1 4). |
| Page footer | "Edit on GitHub" link, last updated (git date), prev / next cells. |

---

## 8. Information architecture

```
/                       Landing                          static
/pricing                Pricing                          static (+ CTA island)
/download               Store-style listing + install    static (ISR 1h for release data)
/compare/wispr-flow     Full comparison                  static
/docs/**                Docs (Fumadocs)                  static
/changelog              Changelog index (+ /rss.xml)     static
/changelog/[slug]       Entry                            static
/sign-in, /sign-up      Clerk                            dynamic
/device                 CLI device approval              dynamic, auth required
/dashboard              Account: plan, usage, tokens     dynamic, auth required
/checkout               → Dodo hosted checkout (route handler redirect)
/checkout/success       Activation status                dynamic
/privacy /terms /security  Legal                        static, content TBD by owner
/not-found              Terminal-style 404               static
```

Global meta: `metadataBase = SITE_URL`, titles as `"{Page} — xflow"`, and the home title "xflow — voice dictation for developers". `sitemap.ts` and `robots.ts` exist. `/dashboard`, `/device` and `/checkout/*` are `noindex`.

OG images are built with `next/og` at build time: 1200 × 630, `--bg` dark, the grid frame, the page title in Geist 560, the `xflow▍` wordmark and the section label of the page. One template, with the title as the only parameter.

---

## 9. Page specs and copy

Copy below is final draft. Strings in `{braces}` are data. ⁽ⁿ⁾ marks a claim that must resolve to the [claims ledger](#11-claims-ledger).

### 9.1 `/` Landing

Section order (each is a `<section>` on the grid with a full-bleed top rule):

| # | Section | Label |
|---|---|---|
| 0 | Header | — |
| 1 | Hero | — |
| 2 | Proof strip (Fig) | — |
| 3 | How it works | `[ 01 / 07 ] · HOW IT WORKS` |
| 4 | Dev words | `[ 02 / 07 ] · BUILT FOR DEV WORDS` |
| 5 | Privacy | `[ 03 / 07 ] · PRIVATE BY DEFAULT` |
| 6 | Native and fast | `[ 04 / 07 ] · NATIVE` |
| 7 | Providers | `[ 05 / 07 ] · YOUR KEY OR OURS` |
| 8 | CLI and TUI | `[ 06 / 07 ] · KEYBOARD FIRST` |
| 9 | Compare | `[ 07 / 07 ] · XFLOW VS WISPR FLOW` |
| 10 | Pricing teaser | — |
| 11 | FAQ | — |
| 12 | CTA band | — |
| 13 | Footer | — |

#### 1. Hero

```
┌──────────────────────────────────────────────────────────────────────────┐
│[ HOLD FN ]          bg-cells (masked)                     [ NO TELEMETRY ]│
│                 ✦                                     ✦                   │
│            ( ● v0.1 · macOS · Linux beta · open source › )               │
│                                                                          │
│                      Hold Fn and talk.                                   │  display
│                      ▓xflow types it.▓        ← volt-soft highlighter    │
│                                                                          │
│     Voice dictation for developers. A native Rust daemon turns speech    │  body-lg, --fg-muted, 56ch
│     into clean, correctly spelled text in your editor, terminal and      │
│     chat — wherever your cursor is.                                      │
│                                                                          │
│            [ Download for macOS ]   [ Read the docs → ]                  │  primary lg + secondary lg
│            Free with your own key · Pro $5/month · MIT                   │  caption --fg-subtle
│                                                                          │
│ ┌─ window: editor pane (7) ────────────────┬─ terminal pane (5) ───────┐ │
│ │ ○ ○ ○  deploy.md                         │ xflow watch       [LIVE ●]│ │
│ │                                          │ $ xflow watch             │ │
│ │  ## Rollout                              │ 12:04:31 listening hold·Fn│ │
│ │  Deploy the API to Kubernetes and bump   │ 12:04:35 processing 4.2 s │ │
│ │  the Postgres pool to 20.▍               │ 12:04:36 success 61 chars │ │
│ │           ╭─────────────────╮            │                           │ │
│ │           │ ▌▍▌█▌█▌▍▌  0:04 │  ← Pill    │                           │ │
│ │           ╰─────────────────╯            │                           │ │
│ └──────────────────────────────────────────┴───────────────────────────┘ │
│[ .WAV → TEXT ]                                                    [ MIT ]│
└──────────────────────────────────────────────────────────────────────────┘
```

- **H1:** "Hold Fn and talk." / "xflow types it." Line 2 carries the highlighter: `background: linear-gradient(transparent 58%, var(--volt-soft) 58%)` in light, and a Volt underlay at 22 % in dark. The highlight is drawn with `box-decoration-break: clone` so it wraps properly.
- **Lede:** "Voice dictation for developers. A native Rust daemon turns speech into clean, correctly spelled text in your editor, terminal and chat — wherever your cursor is."
- **CTAs:**
  - The primary button is OS-aware. The page stays static and defaults to "Download for macOS" (→ `/download`). A ≤ 1 KB inline script swaps the label to "Install on Linux" (→ `/download#linux`) **before paint** when `navigator.userAgentData?.platform` or the UA says Linux. The button has a fixed width, so nothing shifts. The secondary is "Read the docs →" (`/docs`).
  - The caption under the CTAs reads "Free with your own key · Pro $5/month · MIT".
- **Hero window:** one `--bg-raised` object (radius 10, `--shadow-1`), max-width 1040, split `7 / 5` with a 1 px rule between the panes (stacked on mobile: the editor first, the terminal collapsed to 3 lines).
- **Editor pane:** mono 15/24 prose, with the transcript revealed by `steps()` and the pill floating 24 px above the text baseline.
- **Terminal pane:** the `Terminal` component, animated.
- **Loop:** listening (2.5 s) → processing (1 s) → success (text in place, 2 s) → idle (1 s), restart. Static frame under reduced motion: the text is typed, the pill shows `success`, and every terminal line is visible.
- **LCP element:** the H1 text (not an image). The window is pure HTML and CSS, so no image weight lands in the hero.

#### 2. Proof strip (`Fig` ×4)

| Fig | Number | Caption | Ledger |
|---|---|---|---|
| 1 | `11 MB` | Fig 1. Idle memory, release daemon | ⁽¹⁾ |
| 2 | `~3 ms` | Fig 2. Daemon start (p50) | ⁽²⁾ |
| 3 | `0` | Fig 3. Telemetry calls | ⁽³⁾ |
| 4 | `10+` | Fig 4. Speech providers, plus local | ⁽⁴⁾ |

#### 3. How it works

- **H2:** "Three moves. No window."
- **Lede:** "xflow has no app to switch to. It sits in the background and types where you already are."
- Uses `NumberedRows` on the left (5 cols) and a visual on the right (7 cols: `Kbd` Fn in the held state, the `Pill` and the typed result):
  - `01 Hold Fn` — "Hold the Fn key in any app — editor, terminal, browser, Slack. Double-tap it to go hands-free."
  - `02 Talk` — "Say it the way you'd say it to a colleague. Pauses, restarts and 'um's are fine."
  - `03 Let go` — "Clean text lands at your cursor. If the field can't take it, it's on your clipboard and `xflow last` has it."

#### 4. Built for dev words (accuracy)

- **H2:** "It knows Postgres isn't 'post grass'."
- **Lede:** "General dictation guesses at the words you use most. xflow learns your vocabulary and fixes it the same way every time."
- Uses a `FeatureGrid` of 4 (2 × 2). Each visual is a small before/after diff in mono: `−` lines in `--fg-subtle` with strikethrough, `+` lines in `--syn-string`.

| Cell | Title | Body | Diff visual |
|---|---|---|---|
| a | Your dictionary | "Add the names, libraries and acronyms you say every day. They're sent as hints with every recording." | `$ xflow dictionary add Kubernetes Postgres tRPC` |
| b | Exact replacements | "When a phrase always comes out wrong, map it once." | `− post grass` / `+ Postgres` |
| c | Learns from your fixes | "Correct a word right after dictating and xflow adds it to your dictionary. It tells you when it does, and you can remove it." | `Added 'Kubernetes' to your dictionary` |
| d | Per-app styles | "Casual in Slack, exact in your editor. Styles switch with the app you're in." | `$ xflow styles add chat --apps slack,discord --mode light` |

- **Footnote line** (caption): "Optional AI cleanup fixes punctuation and filler words. If cleanup fails, you get the original transcript — never nothing."

#### 5. Private by default

- **H2:** "Your voice isn't our dataset."
- **Lede:** "xflow is open source, has no telemetry, and keeps audio in memory. Read the code, or run it fully offline."
- `ascii-wave` sits behind the heading. Six `Callout`s in a 2 × 3 grid:
  - `[*]` "**Audio never touches disk.** Recordings live in memory and are dropped after transcription. Nothing is logged." ⁽⁵⁾
  - `[*]` "**No telemetry. None.** The daemon makes no network calls except to the speech provider you chose." ⁽³⁾
  - `[*]` "**Offline mode.** Point xflow at a local whisper.cpp server and set `privacy.offline = true`. Non-local endpoints are refused." ⁽⁶⁾
  - `[*]` "**History stays local.** A private SQLite file on your machine. Turn it off with one setting." ⁽⁷⁾
  - `[*]` "**Secure fields are off-limits.** Password fields refuse dictation. Multi-line text is never auto-pasted into a terminal." ⁽⁸⁾
  - `[*]` "**MIT licensed.** Every line that touches your audio is on GitHub." ⁽⁹⁾
- **Pro disclosure** (`Callout` Note, full width): "On Pro, audio goes through xflow's server to our speech provider ({PROVIDER}) and is not stored. We keep a usage record — time, duration, size — for billing and abuse limits." ⁽¹⁰⁾ This line is **required**. Don't remove it to make the section look cleaner.

#### 6. Native and fast

- **H2:** "A daemon, not a browser in a trench coat."
- **Lede:** "xflow is a Rust binary with no Electron, no WebView and no model running in the background. It starts in milliseconds and idles at the size of a photo."
- Two cells (`6 / 6`):
  - **Left:** a `Fig` pair with mini bar visuals: "11 MB idle RSS ⁽¹⁾" and "~3 ms cold start ⁽²⁾". The caption says "Measured on Linux (release build). macOS figures coming with the signed release." (Remove that line once ⁽¹ᵐ⁾ exists.)
  - **Right:** a `Terminal` (static) showing `$ xflow doctor` output **captured from a real run** [verify], ending in all-green checks.
- **Strip below** (3 small cells): "Mic opens only while you record" · "Stops on silence: 'Didn't catch anything' instead of a hallucination ⁽¹¹⁾" · "Your text survives: if delivery fails, `xflow last` recovers it".

#### 7. Your key or ours (providers)

- **H2:** "Bring any key. Or none."
- **Lede:** "Free means your own provider key, unlimited, forever. Pro means ours — nothing to set up."
- `ProviderTile` grid from `content/providers.json` ⁽⁴⁾: Groq, OpenAI, Deepgram, AssemblyAI, ElevenLabs, Gemini, Mistral, Together, DeepInfra, OpenRouter, plus "Local / OpenAI-compatible" (badge `LOCAL`) and "xflow Pro" (badge `PRO`, Volt top border).
- Under the grid, a `CodeBlock` (toml):

```toml
[stt]
provider = "groq"
model = "whisper-large-v3-turbo"
```

  Next to it, in mono: `$ xflow key set groq` with the caption "Hidden input. Stored in xflow's encrypted local store." ⁽¹²⁾

#### 8. Keyboard first (CLI and TUI)

- **H2:** "Everything is a command."
- **Lede:** "A full CLI for scripts and a TUI for everything else. Pipe transcripts, search history, edit your dictionary — without leaving the terminal."
- Split `5 / 7`:
  - **Left:** a `CodeBlock` (shell) with real commands from the README, each with a `#` comment:

```sh
xflow listen --once --seconds 5   # print text, don't type it
xflow transcribe interview.wav    # files work too
xflow history search postgres --json
xflow history export --format csv > dictations.csv
xflow watch --json | jq .state    # live events
```

  - **Right:** a **real TUI capture**: an AVIF screenshot of `xflow tui` at 120 × 36 in both themes (or one dark capture inside the terminal frame), `width`/`height` attributes set, lazy-loaded (`loading="lazy"`), with alt text "xflow TUI home screen showing status, recent dictations and the keymap". It **must be captured from the real TUI**. Never mock it.

#### 9. xflow vs Wispr Flow

- **H2:** "A third of the price. All of the words."
- **Lede:** "Wispr Flow is good. xflow is built for people who live in a terminal — and it costs less."
- `ComparisonTable` rows (all facts in §11; cells marked [verify] must be checked against Wispr's public pages before launch):

| | xflow | Wispr Flow |
|---|---|---|
| Unlimited plan | **$5/mo** | $15/mo ($12/mo billed yearly) ⁽¹³⁾ |
| Free plan | **Unlimited with your own key** | 2,000 words/week ⁽¹³⁾ |
| Bring your own provider key | Yes, 10+ providers | No [verify] |
| Fully offline / local model | Yes (local whisper.cpp server) | No [verify] |
| Open source | Yes, MIT | No |
| Telemetry | None | [verify their privacy policy wording] |
| App runtime | Native Rust daemon | Desktop app [verify runtime] |
| CLI and scriptable | Yes | No [verify] |
| Learns from your corrections | Yes | Yes [verify] |
| Platforms | macOS, Linux (beta) | macOS, Windows, iOS, Android ⁽¹³⁾ |

- The **platforms row stays.** It's where Wispr wins, and leaving it out would make the rest less credible.
- Link: "Full comparison →" (`/compare/wispr-flow`).

#### 10. Pricing teaser

- Two `PricingCard`s (the same component as `/pricing`, compact variant: features truncated to 3 rows) and the link "See pricing →".

#### 11. FAQ

Native `<details>`, 6 items:

1. **"Is the free plan really unlimited?"** — "Yes. Free uses your own speech provider key, so you pay the provider directly — often fractions of a cent per minute — and xflow never meters you."
2. **"What does Pro add?"** — "Our provider key and AI cleanup, with nothing to configure. Sign in from the CLI and dictate. $5/month, unlimited for real use. We rate-limit scripts, not people."
3. **"Does xflow send my audio anywhere?"** — "Only to the speech provider you pick — or nowhere, in offline mode. On Pro, audio passes through our server to {PROVIDER} and isn't stored."
4. **"Which apps does it type into?"** — "Anything with a text cursor. Native apps, browsers, Electron apps like VS Code and Slack, and terminals (single-line, for safety). If a field won't accept text, it's on your clipboard."
5. **"Windows? iPhone?"** — "Not yet. macOS is first, Linux (GNOME) is in beta. Follow the changelog."
6. **"Why should I trust a dictation tool with my keyboard?"** — "You can read every line. xflow is MIT-licensed, logs no text, refuses password fields, and asks macOS only for the three permissions it needs: Microphone, Accessibility and Input Monitoring."

#### 12. CTA band

```
┌──────────────────── bg-dots ────────────────────┐
│            $ xflow setup▍                       │  stat-size mono, cursor blinks
│     Five minutes from download to first words.  │  body-lg --fg-muted
│     [ Download for macOS ]  [ Get Pro — $5 ]    │
└─────────────────────────────────────────────────┘
```

The copy "five minutes" ⁽¹⁴⁾ needs a timed fresh-install run before it ships. Until then, use "From download to first words in one guided setup."

### 9.2 `/pricing`

1. **Header**
   - Label: `[ PRICING ]`
   - H1: "Free with your key." / ▓"$5 when you'd rather not."▓
   - Lede: "Both plans are unlimited. The only difference is whose provider key does the work."
2. **Cards** (`6 / 6`)

   **FREE · BYOK** — `$0` — "Your provider key. Unlimited." — [Download] (secondary)
   - ✓ Unlimited dictation with your own key
   - ✓ 10+ speech providers, or a local server
   - ✓ Optional AI cleanup with your own key
   - ✓ Dictionary, replacements, learning, snippets, styles
   - ✓ CLI, TUI, history, export
   - ✓ Offline mode

   **PRO** — `$5 /month` — "Our key. Nothing to set up. Unlimited." — [Get Pro] (primary)
   - ✓ Everything in Free
   - ✓ Hosted transcription — no provider account or key
   - ✓ AI cleanup included
   - ✓ Sign in once from the CLI (`xflow login` [verify command name])
   - ✓ Cancel any time from your dashboard

   Under the cards (caption): "Prices in USD. Taxes calculated at checkout. Payments handled by Dodo Payments, our merchant of record."

3. **"What you'd pay elsewhere"** — a slim 3-row comparison:

   | Plan | Price | Limits |
   |---|---|---|
   | xflow Pro | $5/mo | unlimited |
   | xflow Free | $0 | unlimited, BYOK |
   | Wispr Flow Pro | $15/mo or $12/mo yearly | — ⁽¹³⁾ |

4. **Fair use** (`Callout`, Note): "Pro is unlimited for people. To keep it that way, we cap concurrent requests and per-minute bursts, so scripts can't drain the shared key. You'll never hit the limit by talking." Numbers come from #69 config: concurrency 2, plus a per-minute limit. Don't print the exact per-minute limit until it's final.
5. **Billing FAQ**
   - "Can I cancel?" — "Yes, from the dashboard. Pro stays on until the end of the period you paid for."
   - "What happens to my settings if I cancel?" — "Nothing. xflow drops back to Free and uses your own key if you've set one."
   - "Do you offer refunds?" — **TBD by owner. Don't ship an answer until decided.**
   - "Annual plan?" — "Not yet. $5/month is already the low price."
   - "Which payment methods?" — "Whatever Dodo Payments supports in your country — cards and local methods are shown at checkout."
   - "Team or company billing?" — "Not yet. Email {SUPPORT_EMAIL} [TBD]."
6. **CTA band** (shared).

JSON-LD: a `Product` with two `Offer`s (`price` 0 and 5, `priceCurrency` USD).

### 9.3 `/download` (store listing)

This is the "app store" page: one product listing laid out like a store detail page.

```
┌────────────────────────────────────────────────────────────────────────┐
│ ┌──────┐  xflow                                       [ Download .pkg ]│
│ │ xf▍  │  Voice dictation for developers               macOS 13+ ⁽ᵛ⁾   │
│ └──────┘  by the xflow project · MIT · open source                     │
├┄┄┄┄┄┄┄┄┄┄┄┬┄┄┄┄┄┄┄┄┄┄┄┬┄┄┄┄┄┄┄┄┄┄┄┬┄┄┄┄┄┄┄┄┄┄┄┬┄┄┄┄┄┄┄┄┄┄┄┬┄┄┄┄┄┄┄┄┄┄┤
│ VERSION   ┆ SIZE      ┆ IDLE RAM  ┆ TELEMETRY ┆ LICENSE   ┆ PLATFORM │  spec chips
│ 0.1.0     ┆ {MB}      ┆ 11 MB     ┆ none      ┆ MIT       ┆ mac·linux│  mono-label / mono-sm
├┄┄┄┄┄┄┄┄┄┄┄┴┄┄┄┄┄┄┄┄┄┄┄┴┄┄┄┄┄┄┄┄┄┄┄┴┄┄┄┄┄┄┄┄┄┄┄┴┄┄┄┄┄┄┄┄┄┄┄┴┄┄┄┄┄┄┄┄┄┄┤
│ [ screenshot rail: pill states ·  TUI home · setup · doctor ]          │  horizontal scroll-snap, native, no JS
├────────────────────────────────────────────────────────────────────────┤
│ [ 01 / 03 ] · INSTALL                                                  │
│ CommandBox tabs: macOS (.pkg) | Linux | From source                    │
├────────────────────────────────────────────────────────────────────────┤
│ [ 02 / 03 ] · WHAT HAPPENS NEXT                                        │
│ NumberedRows 01–05 (from the real guided setup)                        │
├────────────────────────────────────────────────────────────────────────┤
│ [ 03 / 03 ] · VERIFY                                                   │
│ SHA-256 per file · codesign check · release notes                      │
└────────────────────────────────────────────────────────────────────────┘
```

**Data.** Version, file names, sizes and SHA-256 come from the latest GitHub Release via the API at build time (ISR 1 h). If no release exists, the macOS tab shows "Build from source" only, and the hero button becomes "Get the source". Never link a file that isn't published.

**Install tabs.** All copy is from the README. Keep it in sync.

macOS (.pkg):
1. "Download `xflow-{version}.pkg` and open it."
2. "The installer puts `xflow` and `xflowd` in `/usr/local/bin`, starts the background service and opens guided setup."
3. "Setup walks you through: speech service → key → microphone → shortcut → permissions, then asks you to hold Fn and say something."

Linux (beta, GNOME):

```sh
sudo apt install build-essential pkg-config libasound2-dev libdbus-1-dev wl-clipboard
cargo build --release --locked
scripts/install.sh
xflow setup
```

With the caption: "GNOME on Wayland needs the bundled Shell extension: `xflow extension install`, then log out and back in." Link to `/docs/linux`.

From source: `cargo build -p xflow-app --locked` plus `scripts/install.sh`, and a link to `/docs/install#source`.

The `curl … | sh` and Homebrew methods appear **only** once those installers exist (§11, ⁽¹⁵⁾).

**What happens next** (`NumberedRows`):
- `01 Speech service` — "Pick a provider, or Pro."
- `02 Key` — "Paste it once. Hidden input."
- `03 Microphone` — "Choose your input device."
- `04 Shortcut` — "Fn by default. On macOS we'll set the Globe key to Do Nothing so it stops opening emoji."
- `05 Permissions` — "Microphone, Accessibility, Input Monitoring. We open the exact System Settings pane; you flip one switch each."

**Verify:**
- Checksums table: file, SHA-256 (mono-sm, truncated with copy).
- `$ shasum -a 256 xflow-{version}.pkg`
- `$ codesign -dv --verbose=2 /usr/local/bin/xflowd` with the caption "Look for identifier com.xflow.xflowd."

**Requirements** (caption list): macOS {min} on Apple silicon or Intel [verify the deployment target and architectures from the release build]; Linux: GNOME 4x on Wayland, PipeWire/ALSA [verify against platform-setup.md].

**Screenshot rail:** 4 to 6 AVIF captures at 16:10 with fixed `width`/`height`, `scroll-snap-type: x mandatory`, `tabindex="0"` on the scroller and `aria-label="Screenshots"`. Real captures only: pill states from `docs/img/pill-*.png` (re-export at 2×), TUI home, setup and doctor.

### 9.4 `/docs`

**Content tree** (`apps/web/content/docs/`). Port existing repo docs rather than rewriting them. Wherever README or `docs/*.md` say it, the website says the same thing.

```
index.mdx                 Introduction (what xflow is, 60-second tour)
getting-started/
  install.mdx             (from README "Build and run", "macOS", installer)
  setup.mdx               guided setup, `xflow setup --yes` flags
  first-dictation.mdx
using/
  hotkeys.mdx             (docs/hotkeys.md)
  hands-free.mdx
  command-mode.mdx
  history.mdx
  dictionary.mdx          dictionary, replacements, auto-learn
  snippets-and-styles.mdx
  overlay.mdx
providers/
  index.mdx               table generated from providers.json
  {groq,openai,deepgram,assemblyai,elevenlabs,gemini,mistral,together,deepinfra,openrouter}.mdx
  local-and-offline.mdx   custom endpoint, whisper.cpp, privacy.offline
pro/
  sign-in.mdx             device flow from the CLI
  hosted.mdx              what is sent where; limits
  billing.mdx
reference/
  cli.mdx                 GENERATED from `xflow --help` tree at build (script), never hand-edited
  config.mdx              GENERATED from config/default.toml with comments
  events.mdx              `xflow watch --json` schema
troubleshooting/
  permissions.mdx         macOS rows, re-signing, Check-up
  doctor.mdx
linux.mdx                 (docs/platform-setup.md)
privacy-and-security.mdx
```

**Generated references:** `scripts/gen-docs.ts` runs the release `xflow` binary (`--help` for each subcommand) and parses `config/default.toml`, writing MDX. In CI this needs a built binary. Commit the generated files and have CI fail on diff, rather than building Rust on Vercel.

**Docs home** (`/docs`): H1 "Docs", a `CommandBox` ("Install"), then a 3 × 2 `FeatureGrid` of entry points (Install, Setup, Hotkeys, Dictionary, Providers, CLI reference) and a "Troubleshooting" `Callout`.

**MDX components available:** `Callout`, `CodeBlock` (fenced), `Tabs`, `Kbd`, `Pill`, `Terminal`, `Steps` (`NumberedRows` variant), `ProviderTable`.

### 9.5 `/changelog`

- Source: `content/changelog/YYYY-MM-DD-{slug}.mdx` with frontmatter `version`, `date`, `title`, `tags: [macos|linux|cli|tui|pro|web]`. Write a post per release; small fixes are grouped. Not auto-generated from commits, because commit messages aren't release notes.
- Layout per entry, a `3 / 9` split on the grid:

```
├───────────────┬┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┤
│ v0.1.0        ┆ Globe key setup, shared Keychain access   [MACOS] [TUI]  │  h3 + badges
│ 2026-10-28    ┆ prose, images (fixed aspect), code                       │
│ (sticky)      ┆                                                          │
```

  The left column is mono-sm and `position: sticky` under the nav.
- Index header: H1 "Changelog", lede "What shipped, when." and an RSS link (`/changelog/rss.xml`, generated at build).
- Each entry has an anchor (`#v0-1-0`) and a permalink page with OG.

### 9.6 Auth pages (`/sign-in`, `/sign-up`)

Centred on `bg-dots`, inside the frame. The Clerk component sits on a `--bg-raised` panel (radius 10). Above it: the wordmark and "Sign in to xflow" (h2).

Clerk `appearance`:

```ts
const appearance = {
  variables: {
    colorPrimary: '#C6F135', colorTextOnPrimaryBackground: '#0A0A0A',
    colorBackground: 'var(--bg-raised)', colorText: 'var(--fg)',
    colorTextSecondary: 'var(--fg-muted)', colorInputBackground: 'var(--bg-raised)',
    colorInputText: 'var(--fg)', colorDanger: 'var(--rec)', borderRadius: '6px',
    fontFamily: 'var(--font-geist-sans)', fontFamilyButtons: 'var(--font-geist-sans)',
  },
  elements: { card: { boxShadow: 'none', border: '1px solid var(--line)' } },
}
```

[verify] that Clerk accepts CSS variables in `variables` in the installed version. If it doesn't, pass the resolved hex per theme.

### 9.7 `/device` (CLI sign-in approval)

This is the page the CLI opens (`verification_uri` from #69). Auth is required: signed-out users go to `/sign-in?redirect_url=/device?code=…`.

```
┌────────────── bg-dots ──────────────┐
│ [ CLI SIGN-IN ]                      │
│ Connect the xflow CLI               │  h2
│ Check that this code matches the    │  body --fg-muted
│ one in your terminal.               │
│ ┌──────────────────────────┐         │
│ │  KX7M-Q4TR               │         │  CodeInput, prefilled from ?code
│ └──────────────────────────┘         │
│ Signed in as {email}  · Switch       │  caption
│ [ Approve ]   [ Deny ]               │  primary + secondary
└──────────────────────────────────────┘
```

States (each one is a distinct, tested view):

| State | Copy |
|---|---|
| Idle | as above |
| Approved | "✓ Done. Go back to your terminal — xflow is signed in." (h2, `--ok` check). Caption: "You can close this tab." |
| Denied | "Request denied. Nothing was connected." |
| Expired / unknown code | "This code has expired. Run `xflow login` again to get a new one." [verify command] |
| Already used | "This code was already used." |
| Rate limited / server error | "Something went wrong on our side. Try again in a minute." Include a "Retry" button. |

- The Approve button is disabled until the code is 8 valid characters. Submitting goes through a server action, and the button shows a spinner (fixed width).
- Security copy (caption, always visible): "Only approve codes you started yourself. xflow will never ask you for a code by email or chat."

### 9.8 `/dashboard`

Layout: a `3 / 9` split. On the left, a sticky local nav with Plan, Usage, CLI tokens and Account (anchor links; on mobile it becomes a horizontal tab row). On the right, sections as grid cells. Signed-out users are redirected by middleware.

**Plan (`#plan`)**

| State | Display |
|---|---|
| Free | "FREE · BYOK" badge, "You're using your own key." [Get Pro — $5/mo] |
| Pro active | "PRO" badge, "Renews {date}." [Manage billing] (→ Dodo customer portal, new tab, `rel="noopener"`) |
| Pro cancelled, in period | "Pro until {date}. You won't be charged again." [Resume] (portal) |
| Past due | `Callout` Warning: "Your last payment failed. Update your card to keep Pro." [Update payment] |

**Usage (`#usage`):** "This month": requests, audio minutes and cleanup requests, in mono `stat`-small cells. Plus a 30-day bar sparkline as inline SVG, rendered server-side with fixed 120 px height. Fields depend on the `usage_events` schema from #69 [verify]. The empty state says "No hosted usage yet. Free (BYOK) dictation isn't counted — it never touches our servers."

**CLI tokens (`#tokens`):** a table with Name/device, Created, Last used and an action column (Revoke).
- Revoke opens a `<dialog>`: "Revoke this token? The CLI on {device} will be signed out." The destructive action is labelled `Revoke`, and the default focus is Cancel.
- The empty state says "No CLI sign-ins. Run `xflow login` in your terminal."
- Never display token values. They're hashed (#69).

**Account (`#account`):** Clerk `<UserProfile routing="hash"/>` inside a panel, or a link to it, plus "Delete account" (danger) → Clerk flow. Deletion semantics (cancel subscription, purge usage) are **TBD in #69**. Don't show the button until the backend handles it.

### 9.9 `/checkout` and `/checkout/success`

- `/checkout` is a route handler. It requires auth, creates the Dodo checkout session (server-side, per #69) and responds with `303` to the hosted URL. Errors render a page saying "Couldn't start checkout. Nothing was charged." with Retry.
- `/checkout/success`: H2 "Activating Pro…", with a `Pill` in the `processing` state. It polls entitlement every 2 s for up to 30 s (webhooks can lag) ⚡.
  - On success: "✓ Pro is on." A `CommandBox` with `$ xflow login` [verify], and the caption "Already signed in? Your next dictation uses Pro automatically." [verify CLI behaviour] Then [Go to dashboard].
  - On timeout: "Payment received — activation is taking longer than usual. It'll switch on within a few minutes; you don't need to pay again." Then [Go to dashboard].
- A cancel return from Dodo goes to `/pricing?checkout=cancelled` with a toast: "Checkout cancelled. You weren't charged."

### 9.10 `/compare/wispr-flow`

A long-form version of §9.1.9, with the full table plus three short sections: "Price", "Privacy" and "Where Wispr Flow is better" (platforms and mobile, honestly). Every competitor fact carries a footnote with source URL and access date. Page date: "Last checked {date}".

### 9.11 404

```
$ open /some/path
zsh: no such file or directory: /some/path
[ Go home ]  [ Search docs ]
```

The `Terminal` component, static. The missing path is printed from the request.

### 9.12 Legal

`/privacy`, `/terms` and `/security` share the docs prose layout. **The content must come from the owner. Do not generate legal text.** `/security` holds the disclosure contact, a PGP key if any, and a summary of permissions and data flows (it can reuse §9.1.5 and #69 facts).

---

## 10. Accessibility and performance budgets

### 10.1 Accessibility (WCAG 2.2 AA, release-blocking)

- **Contrast:** use the token pairs in §5.1 only. Any new pair must be checked (axe in CI).
- **Keyboard:**
  - Every interactive element is reachable, in visual order, with the visible `--focus` ring (never `outline: none` without a replacement).
  - The first focusable element is a "Skip to content" link.
  - Tabs use arrow-key roving `tabindex` (ARIA tabs pattern).
  - Dialogs trap focus (native `showModal`) and return it on close.
- **Semantics:**
  - One `<h1>` per page, and no skipped levels.
  - Landmarks: `<header>`, `<nav aria-label="Main">`, `<main id="content">`, `<footer>`.
  - Section labels are `aria-hidden`.
  - Decorative textures, crossings and ASCII art are `aria-hidden` / `alt=""`.
- **Motion:** honour `prefers-reduced-motion` (§5.6). No content is conveyed only by animation. The hero's final frame is the content.
- **Targets:** at least 24 × 24 CSS px everywhere (2.5.8), and 40 px for primary controls.
- **Forms:** visible labels (placeholder is not a label), errors in text linked with `aria-describedby`, and autocomplete attributes. The device code input announces its format ("8 characters, like KX7M-Q4TR").
- **Live regions:** copy buttons, toasts and checkout activation use `role="status"`.
- **Zoom and reflow:** usable at 400 % zoom and 320 px width with no horizontal scrolling of page content. Code blocks may scroll, and are focusable.
- **Language:** `<html lang="en">`. Expanded abbreviations: first use of BYOK on a page is `<abbr title="bring your own key">BYOK</abbr>`.
- **Testing:** `@axe-core/playwright` on every route in light and dark, with zero violations. Manual VoiceOver pass on `/`, `/pricing`, `/device` and `/dashboard` before launch.

### 10.2 Performance budgets (CI-enforced with Lighthouse CI, mobile, simulated throttling)

| Metric | Budget | Applies to |
|---|---|---|
| Lighthouse Performance / A11y / Best Practices / SEO | **≥ 95 each** | all public routes |
| CLS | **0.00** (assert `≤ 0.01`) | all routes |
| LCP | ≤ 1.8 s | marketing, docs |
| TBT | ≤ 100 ms | marketing, docs |
| INP (field, Vercel Speed Insights *only if* analytics are approved, else lab proxy) | ≤ 200 ms | all |
| First-load JS (gzip) | ≤ 90 KB marketing; ≤ 140 KB docs (search); ≤ 220 KB dashboard (Clerk) | per route, `next build` output |
| CSS (gzip) | ≤ 30 KB total | — |
| Fonts | 2 files (Geist Sans var and Geist Mono var, latin subset), ≤ 110 KB total, both preloaded by `next/font` | — |
| Images | Above the fold: none. Below the fold: AVIF, ≤ 120 KB each, `loading="lazy"`, explicit `width`/`height` | — |
| HTML (gzip) | ≤ 60 KB for `/` | — |
| Third-party requests | 0 on marketing and docs; Clerk and Dodo only on auth and billing routes | — |

### 10.3 Zero-layout-shift rules (how CLS = 0 is guaranteed, not hoped for)

1. Fonts go through `next/font` only (fallback metrics with `size-adjust`). No `@import` from Google Fonts.
2. Every image, video and iframe has `width`/`height` or `aspect-ratio`. Every visual slot in components declares an `aspect-ratio`.
3. Client islands (auth slot, Pro CTA, stars, copy state) render inside **fixed-size boxes** with server-rendered placeholders of identical dimensions.
4. Animations reveal pre-laid-out content (`opacity`, `transform`, `clip-path`) and never insert nodes into flow.
5. The OS-aware download label swaps before paint (inline script), inside a fixed-width button.
6. No late-loading banners or cookie bars. We set no tracking cookies, so we need no consent banner. Clerk's cookies are strictly necessary [verify with owner/legal].
7. Docs search UI is a `<dialog>` (top layer), so it never pushes content.
8. Web fonts for the ASCII art are irrelevant, because ASCII art ships as SVG.

### 10.4 Caching and delivery

- Marketing and docs are static (SSG) and served from the Vercel edge with `Cache-Control: public, max-age=0, must-revalidate` (Vercel's default for static HTML) plus immutable `_next/static`. Release data uses ISR 1 h.
- `/dashboard`, `/device` and `/checkout/*` are dynamic and `Cache-Control: private, no-store`.
- Security headers via `next.config.ts` `headers()`:
  - CSP, with nonces for the inline theme and OS scripts, `script-src` limited to self plus Clerk on auth routes, and `frame-ancestors 'none'`
  - `Referrer-Policy: strict-origin-when-cross-origin`
  - `Permissions-Policy: camera=(), microphone=(), geolocation=()`. The *site* never needs the mic. Only the app does.
  - HSTS

---

## 11. Claims ledger

Every number or competitor fact on the site must appear here with a source. A row marked **BLOCKED** must not ship until its condition is met. The developer renders footnotes from this table (`content/claims.ts`) so copy and evidence can't drift apart.

| # | Claim | Source | Status |
|---|---|---|---|
| 1 | Idle memory ≈ 11 MB | `PERFORMANCE.md` (11,116 KiB RSS after 5 s warm-up, Linux release, headless) | OK for Linux. Caption must say "Linux". **1ᵐ:** macOS figure BLOCKED until measured on the signed release. |
| 2 | Daemon start ~3 ms (p50 2.6 ms) | `PERFORMANCE.md` startup row (warm cache, 30 launches) | OK, with the "p50, warm" caption. |
| 3 | No telemetry | README "Telemetry is absent" | OK. Re-verify before each release (grep for network calls beyond providers). |
| 4 | 10+ speech providers plus local | README providers section | OK. `providers.json` generated from `xflow providers list --json` [verify flag]. |
| 5 | Audio in memory, never saved or logged by default | README "Audio remains in memory and is never saved or logged by default" | OK. |
| 6 | Offline mode rejects non-loopback endpoints | README `privacy.offline` | OK. Wording: "local whisper.cpp **server**". The model isn't bundled. |
| 7 | Local SQLite history, can be disabled | README `privacy.history = false` | OK. Note that it is *plaintext*. Docs must say so. |
| 8 | Secure fields refused; no multiline auto-paste into terminals | README macOS delivery section | OK. |
| 9 | MIT licensed, public repo | `Cargo.toml` license, GitHub visibility PUBLIC | OK. |
| 10 | Pro audio not stored; only usage metadata kept | #69 design (`usage_events`) | **BLOCKED** until the #69 implementation confirms no audio or text persistence, and the provider's data-retention terms are linked. |
| 11 | Silence returns "Didn't catch anything" | README | OK. |
| 12 | Keys stored in xflow's encrypted local store | User decision (Keychain → own 0600 encrypted store) | **BLOCKED** until that migration ships. Until then: "Stored in your system keychain". |
| 13 | Wispr Flow: Pro $15/mo or $12/mo yearly; Free 2,000 words/week; platforms | wisprflow.ai/pricing, checked 2026-10-29 | OK. Re-check monthly. Other Wispr cells in §9.1.9 are [verify]. |
| 14 | "Five minutes to first words" | — | **BLOCKED** until a timed fresh-install run. |
| 15 | `curl \| sh` installer, Homebrew tap | — | **BLOCKED** until they exist. Omit the tabs. |
| 16 | Accuracy vs Wispr on dev terms | — | **Not claimed anywhere** until a published, reproducible benchmark exists (word error rate on a dev-vocabulary set). No "more accurate" headline before that. |
| 17 | End-to-end latency vs Wispr | — | **Not claimed** until measured (key-up to text-in-field, same audio, same machine). |

---

## 12. Build order and acceptance (for the developer agent)

1. `apps/web` scaffold per #69. Add `globals.css` (§5.8), fonts and the theme script. Build a `/styleguide` route (dev only, `notFound()` in production) that renders every token and component in both themes. **Review gate: screenshots of `/styleguide` light and dark.**
2. Frame primitives: `Frame`, `Section` (rule plus crossings), `Cell`, `SectionLabel` and the textures.
3. Components in §7 order. Each has a Playwright visual snapshot (light and dark, 1440 and 375).
4. Landing → Pricing → Download → Docs (port content and the generators) → Changelog → 404.
5. Auth, Device, Dashboard and Checkout pages, wired to the #69 APIs.
6. SEO, OG images, sitemap and headers.
7. Lighthouse CI and axe in CI, with §10 budgets as hard assertions.

**Definition of done:**
- Every route matches its section spec and copy.
- Budgets pass in CI.
- Zero axe violations.
- Every ledger item that is BLOCKED is not rendered.
- `bun run lint && bun run typecheck && bun run test && bun run build` pass.
- Screenshots of every route in both themes at 1440 and 375 are attached to the PR.

---

## 13. Open questions for the owner

1. **Domain.** All copy uses `{SITE_URL}`. Do we stay on `*.vercel.app` for launch?
2. **Logo.** This spec uses a typographic wordmark (`xflow▍`) and an app icon of `xf▍` on a dark tile with a Volt cursor. Do we need a designed mark before launch?
3. **Refund policy, support email, legal text** (Privacy, Terms) are needed before `/pricing` and `/checkout` go live.
4. **CLI sign-in command name** (`xflow login` is assumed throughout). Confirm it with the platform work in #69.
5. **Hosted provider name on Pro** (`{PROVIDER}` = Groq per #69). Should we name it publicly on the privacy section? (Recommended: yes, it is more honest.)
