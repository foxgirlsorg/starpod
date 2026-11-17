# Starpod UI spec (v2)

The single source of truth for the UI rewrite. Every measurement here is a requirement, not a hint.
Dark only. Brand red `#de6161`. Component vocabulary borrowed from shadcn/ui (zinc dark theme),
layout of the front page borrowed from Koala Clash (a big connect button, the profile's announcement,
a server picker), nothing else from either.

## 1. Principles

1. **The first screen answers "am I connected, through what?"** One big button, one line of status,
   one picker. Everything else lives on other pages.
2. **Color is for state.** Red `#de6161` marks the primary action and "connected / selected". Green
   only for good latency and the status dot when connected. No other hues except amber/red latency
   and destructive.
3. **Flat surfaces.** Cards are separated from the ground by one tone step plus a 1px border. No
   shadows except on floating things (select popups, dialogs).
4. **Everything aligns to one 4px grid.** Icons 16px in controls, 18px in the sidebar, always
   vertically centered on the text they label.
5. **Plain words.** "Profile" = a subscription. "Server" = a node. "Group" = a proxy group. Never
   "payload", "engine-managed", "P1" on a main surface; technical terms only in Settings and tooltips.

## 2. Tokens (`ui/design.slint`, replace the values)

| Token | Value | Use |
|---|---|---|
| background | `#111113` | window ground |
| sidebar | `#0c0c0e` | sidebar ground |
| card | `#18181b` | cards, list rows |
| card-hover | `#1f1f23` | hovered rows/tiles |
| popover | `#1c1c1f` | select popups, dialogs |
| secondary | `#27272a` | secondary button, segmented track |
| muted | `#1f1f23` | subtle fills |
| accent | `#27272a` | hover in menus |
| border | `#27272a` | card borders, dividers |
| input | `#3f3f46` | input/select borders |
| foreground | `#fafafa` | text |
| muted-foreground | `#a1a1aa` | secondary text |
| subtle-foreground | `#71717a` | tertiary text, placeholders |
| primary | `#de6161` | primary action, selection |
| primary-hover | `#e57a7a` | |
| primary-pressed | `#c95454` | |
| primary-soft | `#de61611f` (12% alpha) | selected tile fill, connected ring |
| primary-foreground | `#ffffff` | |
| success | `#4ade80` | latency < 200 ms, connected dot |
| warning | `#fbbf24` | latency 200–499 ms |
| destructive | `#ef4444` | errors, delete, latency ≥ 500 / timeout |
| ring | `#de6161` | focus ring |
| radius-sm 6px, radius-md 8px, radius-lg 12px, radius-full 9999px | | |
| space 4/8/12/16/20/24/32/48 | `space-1 … space-12` naming is fine; keep existing names too | |
| font-family | `"Inter Variable"` (keep), font-mono `"Consolas"` (logs and raw rules only) | |
| font sizes | 12 / 13 / 14 / 16 / 20 / 28 | xs / sm / base / lg / xl / 2xl |

Line heights are implicit in Slint; give every single-line Text `vertical-alignment: center` inside a
fixed-height parent instead of relying on baselines.

## 3. Primitives (`ui/primitives.slint`, rewrite)

All interactive primitives: `opacity: 0.5` and no hover when disabled; a 2px focus ring
(`Design.ring`, offset 2px) when they have keyboard focus; `mouse-cursor: pointer` on clickables.

- **Button** `variant: primary | secondary | outline | ghost | destructive`, `size: sm (28px, pad 10,
  13px) | md (36px, pad 16, 14px) | lg (44px, pad 24, 15px)`, optional leading `icon` (an `image`
  property holding an SVG; 16px, gap 8px), `busy` (shows a spinning ring instead of the icon),
  `enabled`. Radius 8. Label weight 500, centered both ways. Width = content unless stretched.
  - primary: bg primary → hover primary-hover → pressed primary-pressed, text white.
  - secondary: bg secondary → hover `#323236`, text foreground.
  - outline: transparent, 1px input border → hover bg accent.
  - ghost: transparent → hover bg accent.
  - destructive: bg destructive, white text.
- **IconButton**: square, same sizes (28/36/44), ghost or outline variant, icon 16px centered, a
  `tooltip` string shown after 500 ms hover in a small popover above.
- **Input**: 36px tall, radius 8, 1px `input` border (ring color when focused), bg transparent,
  12px horizontal padding, text 14px vertically centered, placeholder `subtle-foreground`. Optional
  `label` ABOVE (13px, weight 500, foreground, 8px gap) and `error` BELOW (12px destructive, 6px
  gap) and `hint` below (12px muted) when no error. Optional leading icon (search: 16px at x=12, text
  shifted to x=36). `numeric` flag right-aligns nothing — numbers stay left-aligned like shadcn.
- **Select**: looks exactly like Input (same border, height, padding), value text left, chevron-down
  16px vertically centered 12px from the right edge. Click opens a PopupWindow directly under the box
  (4px gap), same width as the box (or `popup-min-width`), bg popover, 1px border, radius 8, padding
  4, drop shadow `0 8px 24px #00000080`. Items 32px, radius 6, 8px left padding, hover bg accent,
  the current item shows a 16px check on the right. **Max popup height 320px; longer lists scroll**
  (use a `ListView` inside the popup). Each item may carry a right-aligned secondary text (used for
  latency) — so the model is `[SelectItem { text, detail, detail-color }]`, plus a plain-string
  convenience wrapper `SimpleSelect` taking `[string]`.
- **Switch**: 36×20 track radius full, thumb 16px white, 2px inset. Off track `input`, on track
  primary. Animated 150 ms.
- **Segmented** (shadcn Tabs list): track bg secondary radius 8 padding 3, items 30px tall, auto
  width (padding 12), selected item bg background (`#111113`) + foreground text, others
  muted-foreground. Used for rules tabs, log filter.
- **Badge**: 20px tall, radius full, padding 8, 12px weight 500. variants: neutral (bg secondary,
  muted-fg text), primary (primary-soft bg, primary text), success, warning, destructive (each its
  color at 15% alpha bg + full color text), outline (1px border).
- **Card**: bg card, 1px border, radius 12, padding 20. Optional header: title 16px weight 600 +
  description 13px muted, 4px apart, 16px above the body. Optional header-right slot for actions.
- **Separator**: 1px `border` color, horizontal or vertical.
- **Progress**: 6px tall, radius full, track secondary, fill primary (or warning ≥ 80%, destructive
  ≥ 95%).
- **Alert** (banner): radius 8, 1px border, padding 12/16, leading 16px icon, title 14px 600 + text
  13px; variants info (border `border`, icon muted), error (border destructive 40%, bg destructive
  10%, icon destructive), success. Optional close X (ghost IconButton sm). Long text wraps; capped at
  4 lines with a "Show more" ghost button that expands it.
- **ConfirmDialog**: modal PopupWindow centered, 420px wide, popover bg, radius 12, padding 24, title
  18px 600, body 14px muted, buttons right-aligned (outline Cancel, destructive/primary confirm).
  Used for delete profile, delete rule, clear cache.
- **EmptyState**: centered column inside its card: 40px icon circle (bg secondary, 20px icon
  muted-fg), title 15px 600, one line 13px muted, optional button (secondary, md). 24px padding.
- **Spinner**: 16px ring, 2px stroke, rotating 800 ms linear infinite.
- **LatencyText**: `-1` → "—" subtle; `0` → "timeout" destructive; `<200` success; `<500` warning;
  else destructive; format "123 ms", tabular.
- **PageHeader**: title 20px weight 600 foreground, description 14px muted below (4px), optional
  actions slot right-aligned and vertically centered on the title block. Bottom margin 24px.

## 4. Shell (`ui/app.slint`)

- Window: min 900×600, preferred 1100×720. Background `background`.
- **Sidebar** 220px, bg `sidebar`, 1px right border `border`. Padding 12.
  - Brand row 52px: fox mark 22px in primary + "Starpod" 16px weight 600, 8px gap, vertically
    centered, 8px left padding.
  - Nav (12px below brand): items 36px, radius 8, padding-left 10, icon 18px + 10px gap + label 14px
    weight 500. Inactive: icon & label muted-foreground, hover bg `#18181b` + foreground. Active: bg
    `#1f1f23`, label foreground, icon primary. No colored fill.
  - Pages in this order: **Home** (house/power icon), **Profiles** (layers/list icon), **Proxies**
    (server icon), **Rules** (route icon), **Logs** (scroll-text icon), then a flexible spacer, then
    **Settings** (settings icon) at the bottom above the status block.
  - **Status block** at the bottom: a card-colored rounded rect (radius 8, padding 10/12): 8px dot
    (success when running, warning while starting/stopping, destructive when failed, subtle when
    stopped) + "Connected" / "Connecting…" / "Disconnecting…" / "Not connected" / "Failed" 13px 500;
    second line 12px muted: uptime when running ("Up 01:23:45"), otherwise the mode ("Rule mode").
- **Content area**: scrolls vertically (one ScrollView per page, not the whole shell). Padding 32px
  left/right, 28px top, 32px bottom. Content column max width 920px, left-aligned in the area (Home
  is the exception: centered). Global error/success banner (Bridge.banner-*) appears as an Alert at
  the top of the content area, above the page, with a close button.
- `Bridge.active-page`: 0 Home, 1 Profiles, 2 Proxies, 3 Rules, 4 Logs, 5 Settings.

## 5. Pages

### 5.1 Home (`screens/home.slint`) — only three things

A centered column, width 440px, vertically centered in the content area when it fits (top-aligned
with 48px top padding when the window is short; never overlapping).

1. **Announcements** (only when `Bridge.announcements` is non-empty): one Alert-like card per
   announcement, full column width, above the button with 32px gap below the last one: leading 16px
   megaphone icon in primary, title row = the profile's title (13px 600), text below (14px
   foreground, wraps, up to 6 lines then "Show more"), and if `support-url` or `web-url` is set a
   row of ghost sm buttons "Support" / "Open website" that call `Bridge.open-url(url)`.
2. **Connect button**: a 168px circle, centered.
   - stopped/failed: bg card, 2px border `input`, power icon 56px muted-foreground; hover border
     primary and icon foreground.
   - starting/stopping: border primary 2px, icon primary, the whole button's icon pulses opacity
     0.4↔1 every 900 ms; a Spinner is not needed.
   - running: bg primary, no border, icon white, plus an outer ring: a 196px circle behind it with
     bg primary-soft.
   - blocked (`connect-blocked-reason != ""` and not running): looks stopped but at 50% opacity and
     not clickable.
   - Click: running → `Bridge.disconnect()`, stopped/failed → `Bridge.connect()`.
   - Below the button (20px): status title 20px weight 600 ("Connected" / "Not connected" /
     "Connecting…" / "Disconnecting…" / "Couldn't connect"), then 6px below a 13px muted line:
     running → "Up {uptime} · {server-address}" (the address is left out when empty); blocked → the blocked reason, plus one primary md
     button 12px below: "Install engine" (`Bridge.install-engine()`, busy while busy) when the engine
     is missing, otherwise "Add a profile" (switches to page 1); failed → the engine error in
     destructive 13px, max 3 lines.
3. **Picker card** (40px below the status, only when `Bridge.groups` is non-empty): Card padding 16,
   two Selects stacked with labels:
   - "Group": every group's name (`Bridge.groups[i].name`); default = `Bridge.primary-group`. The
     chosen group is local UI state (`home-group` property, initialized from `primary-group`).
   - "Server": the chosen group's members, each item text = member name, detail = latency text in
     its latency color. Value = the group's `selected`. Selecting calls
     `Bridge.select-member(group, member)`. Disabled (with hint "Chosen automatically") when the
     group kind is not `select`.
   - Under the selects, a ghost sm button with a gauge icon: "Test latency" (`Bridge.test-latency()`,
     busy while `Bridge.busy`), right-aligned.

Nothing else on Home: no stats, no mode switch, no cards of numbers.

### 5.2 Profiles (`screens/profiles.slint`)

- PageHeader "Profiles" / "Subscriptions that supply your servers and rules." Actions: outline md
  "Update all" (refresh icon) + primary md "Add profile" (plus icon).
- **Add profile** opens an inline Card at the top of the list (not a dialog): Input "Subscription
  URL" (placeholder `https://…`), Input "Name" (placeholder "Optional — taken from the link"), a ghost
  sm toggle "Advanced" that reveals Input "Authorization header" (hint "Sent with every update, stored
  in your system keychain") and a Segmented "Updated by: Starpod | Engine" (hint for engine: "No
  traffic/expiry info, the engine refreshes it itself"). Footer: ghost "Cancel", primary "Add"
  (busy). Errors show under the URL field.
- **List**: one Card per profile, padding 16, 12px gap between cards:
  - Row 1: name 15px 600 (elide) + status Badge (Updated = neutral "Up to date", syncing = primary
    "Updating…", error = destructive "Update failed", never = neutral "Not updated yet"); right side:
    IconButton ghost refresh (tooltip "Update now"), IconButton ghost trash (tooltip "Remove",
    opens ConfirmDialog "Remove “{name}”?").
  - Row 2 (6px below), 13px muted: "{node-count} servers · Updated {last-updated}" (+ " · Updated by
    the engine" when engine-managed).
  - Row 3 (only with quota): Progress full width, then a 12px muted line: "{quota-text}" left,
    "Expires {expires}" right.
  - Error (if any): 13px destructive text, wraps, max 3 lines.
- Empty: EmptyState "No profiles yet" / "Add your subscription link to get servers." / "Add
  profile".

### 5.3 Proxies (`screens/proxies.slint`)

- PageHeader "Proxies" / "Choose which server each group uses." Actions: Input with search icon,
  220px, placeholder "Filter servers" (bound to `Bridge.node-filter`), outline md "Test latency"
  (gauge icon, busy).
- One collapsible section per group (`Bridge.groups`), 16px apart: header row 44px, card bg, radius
  12 (radius only top when expanded): chevron-right/down 16px, group name 15px 600, kind Badge
  (neutral: "Select" / "Auto" for url-test / "Fallback" / "Load balance" / "Relay"), member count
  13px muted, and right-aligned the currently selected member name 13px foreground (elide, max
  280px). Clicking the header toggles. First group starts expanded, others collapsed.
- Expanded body: a grid of server tiles, columns = floor((width + 8) / 228) (min 1), 8px gaps, 12px
  padding. Tile 60px tall, radius 8, bg `muted`, 1px transparent border; hover bg card-hover.
  Content: name 14px 500 foreground elide (top line), protocol 12px muted left + LatencyText 12px
  right (bottom line), 10px padding. Selected tile: border primary, bg primary-soft. Clicking a tile
  (select groups only) calls `select-member(group, member)`.
- The filter hides tiles whose name doesn't contain the filter (case-insensitive), done in Slint.
- Empty (no groups): EmptyState "No servers yet" / "Add a profile to get servers." / "Go to
  Profiles".

### 5.4 Rules (`screens/rules.slint`)

- PageHeader "Rules" / "Decide where traffic goes. Your rules run before the profile's rules."
- Segmented: "Your rules ({n})" | "From profiles ({m})" | "Edit as text".
- **Your rules** tab:
  - Add row in a Card (padding 12): Select Kind (160px; items = `rule-kinds`), Input Value (fills,
    placeholder from kind, e.g. `example.com`), Select Target (180px; items = `rule-targets`),
    primary md "Add". All on one row, 8px gaps, 36px tall, aligned. Validation error below the row.
  - List in a Card with no padding: rows 44px, 1px dividers between rows, padding 0/16: Switch
    (enabled) · kind Badge (outline, monospace not needed) · value 14px (elide) · arrow → · target
    13px muted · right side IconButtons ghost sm: up, down, pencil (edit inline: the row becomes the
    add-row layout with Save/Cancel), trash.
  - Empty: EmptyState "No rules of your own" / "Profile rules still apply. Add one to override
    them."
- **From profiles** tab: read-only `ListView` (virtualized; there can be thousands) in a Card,
  rows 36px: kind Badge · value · → target. Header line above: "{m} rules from your profiles,
  applied after yours." Error rows show a destructive dot and the error as tooltip text below.
- **Edit as text** tab: a monospace TextEdit, 13px, min height 360px, bg `muted`, radius 8; under
  it the error (destructive) and buttons ghost "Revert" + primary "Apply".

### 5.5 Logs (`screens/logs.slint`)

- PageHeader "Logs" / "What Starpod and the engine are doing." Actions: IconButton outline copy
  (tooltip "Copy all"), IconButton outline trash (tooltip "Clear").
- Segmented filter: All | Info | Warnings | Errors (bound to `log-level-filter`: all/info/warn/error).
- Card with no padding containing a `ListView`, fills the remaining height (min 400px). Rows: time
  12px subtle (monospace, 72px), level 12px 600 colored (INFO muted, WARN warning, ERROR
  destructive, DEBUG subtle; 52px), source 12px muted (90px, elide), message 13px foreground
  (monospace, wraps). 6px vertical row padding.
- Empty: EmptyState "No logs yet".

### 5.6 Settings (`screens/settings.slint`)

- PageHeader "Settings". Sections as Cards with a header title, 16px apart. Inside a card, rows
  separated by Separators; each row min 60px, padding 12/0: left block (label 14px 500 + 13px muted
  description, 2px gap, wraps) and right control, fixed 220px wide, vertically centered on the row.
  Switches are right-aligned in that 220px box.
- **Connection**: Routing (Select: "Rule-based" rule / "Global" global / "Direct" direct; desc "Rule
  -based follows your rules; Global sends everything through the selected server; Direct uses no
  proxy."), Mixed port (Input, placeholder 7890), Allow LAN (Switch), Bind address (Input,
  placeholder "127.0.0.1"; only enabled when Allow LAN is on).
- **Advanced ports**: SOCKS port (placeholder "Off"), HTTP port (placeholder "Off").
- **TUN**: Enable TUN (Switch; desc "Routes all system traffic. Needs administrator rights."),
  Device (placeholder "Automatic"), MTU (placeholder 9000, hint "1280–9000").
- **Profiles**: Auto-update every (Select: Never / 30 min / 1 hour / 6 hours / 12 hours / 24 hours →
  0/30/60/360/720/1440; -1 shows as "1 hour (default)").
- **Engine**: Version text (or "Not installed") + outline "Install" / "Reinstall" button; Log level
  (Select: silent/error/warning/info/debug).
- **Storage**: Secrets stored in (text: backend, + warning text if fallback reason), Data folder
  (text, elide middle), Clear cached profiles (destructive outline "Clear", ConfirmDialog).
- A sticky save bar at the bottom of the content area appears only when the form differs from
  `Bridge.settings`: card bg, top border, 56px, text "You have unsaved changes" + ghost "Discard"
  + primary "Save". `settings-error` shows in the bar in destructive.

## 6. Bridge additions (owned by the backend agent)

```slint
export struct MemberItem { name: string, protocol: string, latency-ms: int }
export struct GroupItem {
    name: string,
    // "select" | "url-test" | "fallback" | "load-balance" | "relay"
    kind: string,
    // the member the group currently uses ("" when unknown)
    selected: string,
    members: [MemberItem],
}
export struct AnnouncementItem {
    subscription-id: string,
    title: string,        // profile-title header, or the profile's label
    text: string,
    support-url: string,
    web-url: string,
}
// in Bridge:
in-out property <[GroupItem]> groups: [];
in-out property <string> primary-group: "";
in-out property <[AnnouncementItem]> announcements: [];
callback select-member(string /* group */, string /* member */);
callback open-url(string);
```

`SubscriptionItem` gains `title: string` (profile-title or label). Page indices change (§4).

## 7. Demo data for screenshots

`STARPOD_UI_DEMO=1` (debug builds and the shot tool) makes `ui/runtime.rs` push a fixed, realistic
data set into the Bridge after load and ignore later snapshots: engine running, uptime "01:12:09",
listen "127.0.0.1:7890", one profile "Foxgirls VPN" with quota 34.2 GB of 200 GB, expires
"12 Dec 2026", 24 servers; an announcement ("Maintenance on Nov 20, 02:00–04:00 UTC. Servers in
Frankfurt will restart once." with a support URL); groups "🚀 Proxy" (select, 12 members with
mixed latencies incl. -1 and 0), "⚡ Auto" (url-test), "🚫 Blocked sites" (select, members DIRECT +
"🚀 Proxy"); 6 user rules, 300 profile rules, 40 log lines of all levels. `STARPOD_UI_PAGE=0..5`
still selects the page. `tools/ui-shot.sh` sets both and its page names become
`home profiles proxies rules logs settings`.
