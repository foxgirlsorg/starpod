# foxgirls.org — Extracted Design System

**Source of truth:** https://github.com/foxgirlsorg/foxgirls.org (`src/app.css`, `src/lib/components/ui/*`)
**Deployed CSS:** https://foxgirls.org/assets/index-G328ult6.css
**Stack:** SvelteKit + Tailwind v4 + shadcn-svelte (`tailwind-variants`)

The site is **dark-only** (`color-scheme: dark`). The token set is literally the shadcn-svelte
token names, retinted. A native Slint desktop port should reuse these names so the translation
is mechanical.

---

## 1. Color tokens (verbatim from `src/app.css` `:root`)

| Token | Value | Slint token name | Notes |
|---|---|---|---|
| `--background` | `#161616` | `background` | App background, near-black neutral |
| `--foreground` | `#ededed` | `foreground` | Primary text |
| `--card` | `#1c1c1c` | `card` | Card / panel surface |
| `--card-foreground` | `#ededed` | `card-foreground` | |
| `--popover` | `#1c1c1c` | `popover` | Same as card |
| `--popover-foreground` | `#ededed` | `popover-foreground` | |
| `--primary` | `#de6161` | `primary` | **Brand red** — the signature accent |
| `--primary-foreground` | `#ffffff` | `primary-foreground` | Text on primary |
| `--secondary` | `#262626` | `secondary` | Filled secondary surface |
| `--secondary-foreground` | `#ededed` | `secondary-foreground` | |
| `--muted` | `#232323` | `muted` | Subtle filled surface |
| `--muted-foreground` | `#a3a3a3` | `muted-foreground` | Secondary text |
| `--accent` | `#262626` | `accent` | Hover surface |
| `--accent-foreground` | `#ededed` | `accent-foreground` | |
| `--destructive` | `#e5484d` | `destructive` | Error / destructive |
| `--border` | `rgb(255 255 255 / 0.08)` | `border` | Hairline, 8% white |
| `--input` | `rgb(255 255 255 / 0.12)` | `input` | 12% white |
| `--ring` | `#de6161` | `ring` | Focus ring = brand red |
| `--brand` | `#de6161` | `brand` | Alias of primary |

Supporting values seen in the built CSS: `#ededed`, `#202020`, `#262626`, `#1c1c1c`, `#232323`,
`#161616`, `#a3a3a3`, `#de6161`, `#e5484d`.

### Derived / semantic tokens to define for the app

The site has no explicit success/warning tokens (shadcn default omits them). A desktop proxy app
needs them; derive them to stay in the same family without breaking the palette:

- `success` — a desaturated green consistent with the muted-neutral scale, e.g. `#4a9e5c` (needs contrast check on `#161616`)
- `warning` — an amber in the same register as the brand red, e.g. `#c99a3f`
- `info` — reuse `muted-foreground` `#a3a3a3` or the brand red family

**Contrast note:** the palette is dark-mode, low-luminance surfaces with a mid-luminance brand red.
`#de6161` on `#161616` is roughly 5.1:1 — passes AA for normal text. `#a3a3a3` on `#161616` is
roughly 7.4:1 — comfortable for secondary text. `#ededed` on `#161616` is roughly 14.6:1.
These are the pairs the UI must keep.

### Status indicator idiom

The site's "connected/active" language is: brand-red filled shape + white text, or a red ring
(focus). For the proxy app, the natural mapping is:

- **Connected / running** → brand red filled dot or pill (the site's most saturated element)
- **Stopped** → `muted-foreground` `#a3a3a3` hollow dot
- **Error** → `destructive` `#e5484d`
- **Syncing / loading** → `muted-foreground` with a reduced-opacity pulse (the site uses
  `tw-animate-css` + `prefers-reduced-motion` guards, so a reduced-motion fallback is expected)

## 2. Typography

- Font stack: `"Inter Variable", "Segoe UI", system-ui, sans-serif`
  - Mono stack: `var(--font-mono)`
- Weights in use: `300` (light), `400` (normal), `500` (medium)
- Base size `1rem` / line-height `1.5`; body is `text-sm` (`0.875rem`) in most components
- Scale (Tailwind v4 `--text-*` tokens, rem, with their line heights):
  - `text-xs` 0.75rem · `text-sm` 0.875rem · `text-base` 1rem · `text-lg` 1.125rem/1.75
  - `text-xl` 1.25rem/1.75 · `text-2xl` 1.5rem/2 · `text-3xl` 1.875rem/1.2
  - `text-4xl` 2.25rem/2.22 · `text-5xl` 3rem/1 · `text-7xl` 4.5rem/1 · `text-8xl` 6rem/1
- Headings use `line-height: 1` at the large end and tighter tracking — display type is set large and tight.

**Desktop translation:** Inter is SIL OFL licensed and freely redistributable. Ship it as a
`FontResource` in the Slint bundle rather than relying on system Inter. Fallback chain
`Segoe UI, system-ui` matches the Windows host. For a numeric/latency readout, use the mono stack.

## 3. Spacing, radii, borders

- Spacing unit: `--spacing: 0.25rem` (4px). Scale is Tailwind's: 1 = 4px, 2 = 8px, 3 = 12px,
  4 = 16px, 6 = 24px, 8 = 32px, 10 = 40px, 12 = 48px, 16 = 64px.
- Radii: `--radius: 0.625rem` (10px)
  - `--radius-sm` = radius − 4px = 6px
  - `--radius-md` = radius − 2px = 8px
  - `--radius-lg` = radius = 10px
  - `--radius-xl` = radius + 4px = 14px
  - `--radius-2xl` = 1rem = 16px
  - `--radius-4xl` = 2rem = 32px
- Borders: hairline `1px` at 8% white. `outline-ring/50` on everything = 50% brand-red focus.
- Cards: `rounded-xl` (14px) + `ring-1 ring-foreground/10` (a 1px inner border equivalent) —
  note the site uses a *ring*, not a border, on cards.
- Buttons: `rounded-lg` (10px), `border border-transparent`.

## 4. Component treatments

### Button (`ui/button/button.svelte`)
Variants: `default` (bg-primary, white text, hover bg-primary/80), `outline` (bg-background +
`border-input` + `hover:bg-muted`), `secondary` (bg-secondary, hover a 5% foreground color-mix),
`ghost` (transparent, hover bg-muted/50), `destructive` (bg-destructive/10, text-destructive,
hover bg-destructive/20, focus ring destructive/20), `link` (text-primary, underline-offset-4).

Sizes (height / padding / text): `xs` h-6 (24px) text-xs · `sm` h-7 (28px) text-[0.8rem] ·
`default` h-8 (32px) text-sm · `lg` h-9 (36px) text-sm · `icon` 32×32.
Icons are 16px (`size-4`) inside default buttons, 12px in `xs`, 14px in `sm`.

Interaction: `focus-visible:ring-3` with `ring-ring/50` (a 3px brand-red halo at 50%);
`active:translate-y-px` (a 1px press-down); `disabled:opacity-50 disabled:pointer-events-none`.
Transitions use `--default-transition-duration: .15s` with
`cubic-bezier(.4, 0, .2, 1)`.

### Card (`ui/card/card.svelte`)
`bg-card` (#1c1c1c), `rounded-xl`, `py`/`gap` = card-spacing (default `--spacing * 4` = 16px;
`size=sm` uses `* 3` = 12px), `text-sm`, 1px ring at 10% foreground. Slots:
`card-header`, `card-title`, `card-description`, `card-action`, `card-content`, `card-footer`
(footer flushes the bottom padding to 0).

### Badge (`ui/badge/badge.svelte`)
Small pill; use for status (e.g. "Connected", "Expired", country tags). Variants follow the
shadcn set — default = `bg-primary text-primary-foreground`, plus `secondary`, `destructive`,
`outline`, `ghost`.

### Other components present in the repo
`avatar` (+ badge/fallback/group), `dropdown-menu` (rich, with checkbox/radio items, groups,
separators, shortcut labels), `separator`, `input`, `label`, `textarea`, `select`, `tabs`,
`tooltip`, `dialog`, `sheet`, `sidebar` (if present), `scroll-area`, `skeleton`, `switch`,
`progress`, `table`, `alert`, `accordion`, `collapsible`, `popover`, `sonner` (toasts).

**Desktop translation:** Slint has no shadcn, so these become hand-written `.slint` components
in a `ui/` directory. The mapping is 1:1 for card / button / badge / input / switch / separator /
dialog. Dropdown-menu and sheet need Slint-specific implementations (popup window vs. overlay).

## 5. Layout & structural idioms

- **Navbar:** top-of-page bar, transparent over content, sticky. Blur is available
  (`--blur-md: 12px`, `--blur-sm: 8px`) — the site blurs the navbar and footer over content.
  Desktop equivalent: a fixed-width left sidebar is better for 5 screens; keep the blur idea for
  a floating/translucent top bar if a top bar is used instead.
- **Sections:** full-width sections separated by generous vertical padding; container widths
  `48rem / 72rem` (3xl / 6xl) with `32rem` / `36rem` (lg / xl) for narrow content.
- **Section heading:** `SectionHeading.svelte` — a consistent eyebrow + title + description pattern.
  Translate to a reusable "panel header" for the desktop app.
- **Hero:** oversized display type at `text-8xl` (6rem) with `line-height: 1`, tight tracking,
  centered, with a blurred video background (`BlurVideo.svelte`) and a slow reveal.
  Desktop equivalent: the Dashboard's primary status readout should use this oversized/tight
  treatment — a large status word rather than a label-and-value form.
- **Entrance motion:** `[data-reveal]` elements start `opacity: 0; translateY(20px)` and rise in
  over 0.6s with `cubic-bezier(0.2, 0.7, 0.2, 1)`, staggered by `--i * 70ms`.
  Guarded by `prefers-reduced-motion`. Desktop: use for the app's first paint (dashboard cards
  staggering in), respecting the reduced-motion setting.
- **Logo:** `Logo.svelte` + `/logo.svg` — a fox-themed mark. Needs a Slint-side recreation as
  vector paths or a bundled SVG-derived image; a text-free geometric fox mark is achievable with
  Slint's `Path`/`PathLayout` but a bundled raster/SVG asset is more faithful.

## 6. Global rules worth porting

- `*` gets `border-border outline-ring/50` — every focusable thing's outline color is the brand red.
- `::selection` = `background: var(--brand); color: #fff` — brand-red text selection.
- `body` is `antialiased` — subpixel-antialiased text.
- `button:not(:disabled) { cursor: pointer }`.
- `scroll-behavior: smooth`; `section[id] { scroll-margin-top: 5rem }`.

## 7. Logo & iconography

### The mark (`src/lib/Logo.svelte`)
A single-color (`fill="currentColor"`) **fox head in profile**, drawn as two SVG paths, viewBox
`0 0 117.29976 203.22969` (tall aspect — head plus ears). One path is the filled silhouette
(ear + muzzle + cheek); the second is a stroked outline (`stroke-width: 1.165`) offset from it,
giving the mark a subtle double-line/ink quality rather than a flat blob. The whole thing is
one color, so it recolors to any `currentColor` — in the navbar it is `text-brand`
(`#de6161`) and rotates `-12deg` on group hover.

Desktop translation: this is a pure vector mark with no gradients or raster. It can be ported to
Slint either as a bundled SVG converted to a `Path`/`svg` image resource, or redrawn with
Slint's `Path`/`PathLayout` primitives using the same two paths. Bundling the converted SVG is
the faithful and cheaper route. Render it in `brand` (#de6161) in the sidebar/app chrome.

### Icon set
The site uses **Lucide** (`@lucide/svelte`) — stroke-based, 24×24 viewBox, default `stroke-width: 2`,
`size-4` (16px) inside default buttons. Named icons actually used: `Menu`, `X`, `Globe`.
Desktop app should use the same Lucide iconography (SIL OFL) so the desktop and web identities
match. Slint has no SVG renderer at runtime, so icons must be converted to image resources
(a build-time SVG→PNG/SVG-texture step) or hand-drawn as `Path` geometry. A small icon set
(~20 glyphs) is enough: dashboard, servers, rules, settings, logs, refresh, connect/disconnect,
chevrons, check, x, alert, globe, search, plus, trash, edit.

## 8. Open items

- **Badge/select/tabs variants** were not fully read; if the app needs them, fetch those files.
