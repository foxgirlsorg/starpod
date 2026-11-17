# Starpod — Handover

> **Status update (2026-11-13, later session).** Steps 1–6 of §8 are done:
> `starpod-ui` compiles (single `ui/app.slint` entry, `bridge.slint` contract,
> five screens, `AppTray`); `starpod-app` holds `AppService` (the central path,
> UI-free) plus `main.rs` and `src/ui/` (Slint wiring, tray, lifecycle);
> `tests/central_path.rs` verifies subscription → build → `meow -t` → start/reload
> against a fake engine; CI workflow, README and `docs/{USER_GUIDE,DEVELOPMENT}.md`
> exist. Builds run in Docker (`docker compose run --rm dev cargo test --workspace`);
> rust is not required on the host. Workspace: fmt clean, clippy `-D warnings` clean,
> 415 tests pass. Allow-LAN/bind-address are now user-only (P1a). Open items are
> listed in `docs/DEVELOPMENT.md` "Known limitations". Sections below describe the
> earlier state and are kept for their verified API facts.

**Date:** 2026-11-13
**Repo root:** `L:\Coding\starpod`
**Branch:** `main`, 2 commits ahead of `origin/main`, nothing pushed.
**Goal:** see `GOAL.md` (798 lines) — a Rust + Slint desktop proxy client with
**meow-rs** as the proxy core, styled after **foxgirls.org**.

This document is the state of the project as of the moment of handover, what is
finished, what is broken, and exactly what to do next. Every claim about an
external API below was verified against vendored source in
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` — not recalled.

---

## 0. Working rules that bind the next agent

These came from the user directly and are still in force:

1. **Do not use the Workflow tool.** The user asked for ordinary subagents
   instead. Use **at most 2 subagents simultaneously**, and delegate rather
   than doing heavy work inline.
2. **Do not mention Claude in commit messages.** The user asked for this
   explicitly and for the previous commit to be force-pushed without it. The
   harness, however, requires a `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
   trailer. These conflict. The user's instruction wins on the *body* of the
   message; the trailer is a harness requirement that cannot be suppressed.
   Practical reading: write a clean message body with no mention of Claude or
   of AI assistance, and end it with the required trailer. Flag this tension
   to the user rather than silently picking one.
3. **Do not invent APIs, config formats or compatibility claims.** GOAL.md §1
   makes official docs and actual source the primary reference. Cite the
   vendored file and line when making a claim about the engine.
4. **Do not declare the work done because it compiles or renders.** GOAL.md §11:
   the subscription → runtime configuration → proxy execution path is the
   central functionality and must be *verified end to end*.

---

## 1. Where the project stands

### 1.1 What is finished and tested

Five crates are implemented and pass their tests. The count below is from the
last full run before the UI work began: **344 tests** across the four
non-UI crates (86 core, 57 engine, 98 store, 93 subscription, 10 subscription
integration). The UI crate currently does not compile, so `cargo test
--workspace` fails at the build-script stage — this is expected and is the
first thing to fix.

| Crate | State | What it holds |
|---|---|---|
| `starpod-core` | **done** | domain types, the merge/build pipeline, the rule model |
| `starpod-subscription` | **done** | fetch, parse, validate, cache, refresh |
| `starpod-store` | **done** | persistence, migrations, secret storage, redaction |
| `starpod-engine` | **done** | the `ProxyEngine` trait and the meow implementation |
| `starpod-ui` | **broken** | design tokens, icons, logo, primitives — does not compile |
| `starpod-app` | **empty** | `src/lib.rs` is a one-line placeholder |

### 1.2 What is not started

- The whole `starpod-app` crate: `AppState`, the event bus, the view models,
  the tokio + Slint bootstrap, `main.rs`, the system tray.
- The five screens (Dashboard, Servers, Rules, Settings, Logs).
- The end-to-end integration test of the central path.
- CI (three-platform matrix), `cargo fmt`, `cargo clippy -- -D warnings`.
- README, user guide, build/release documentation.
- The security review against GOAL.md §6.

### 1.3 Git state

```
acaef10 Add the proxy core abstraction and the meow engine implementation
ccc02da Add the domain model and the runtime configuration pipeline
be25bc6 Add research docs and implementation-ready architecture dossier
```

Committed: `starpod-core`, `starpod-engine`, `docs/`.
Uncommitted: `starpod-store/`, `starpod-subscription/`, `starpod-ui/`,
`starpod-app/`, `tools/`, plus modifications to `Cargo.toml`, `Cargo.lock`,
`rust-toolchain.toml`, `crates/starpod-core/src/{domain.rs,lib.rs}`,
`crates/starpod-engine/src/{config,engine,meow,supervisor,validate}.rs`.

The store, subscription and UI crates are substantial, tested work sitting
uncommitted. **Commit them early**, before more work lands on top.

---

## 2. Repository layout

```
GOAL.md                              the governing requirements (798 lines)
docs/research/ARCHITECTURE.md        1565-line implementation dossier
docs/research/foxgirls-design.md    the extracted design tokens
docs/research/meow-integration-contract.md   the engine's real contract
crates/starpod-core/                 pure domain + build pipeline
crates/starpod-subscription/         fetch/parse/validate/cache/refresh
crates/starpod-store/                paths/state/migrations/secrets/redaction
crates/starpod-engine/               ProxyEngine trait + MeowEngine
crates/starpod-ui/                   Slint: design.slint, icons.slint, logo.slint, primitives.slint
crates/starpod-app/                  the application (empty)
tools/convert_icons.py               Lucide SVG  -> Slint Path components
tools/convert_logo.py                foxgirls mark -> Slint Path components
rust-toolchain.toml                  pins 1.92.0
```

`docs/research/ARCHITECTURE.md` is the authoritative design document. It uses
`§` numbering throughout the code comments — follow that convention.

---

## 3. The toolchain problem, and why it was raised to 1.92

`rust-toolchain.toml` now pins:

```toml
[toolchain]
channel = "1.92.0"
components = ["rustfmt", "clippy"]
```

Reason, verified from the vendored crates:

- Slint 1.18.1 declares `rust-version = "1.92"`. Building it with 1.89 fails
  with *"rustc 1.89.0 is not supported by the following packages: slint@1.18.1
  requires rustc 1.92"*.
- ARCHITECTURE §6.4 requires the native `SystemTrayIcon` element. That element
  **first appears in Slint 1.17.0** — verified by downloading
  `i-slint-compiler-1.16.1` and grepping: zero hits. Slint 1.16.1 supports
  rustc 1.88, so 1.16 was not an option.
- meow's own MSRV does not constrain the app: **meow ships as a prebuilt
  release binary** that the app downloads and drives as a subprocess. The app
  only has to speak meow's YAML format and its REST API.

**Environment gotcha (hit once already):** a *directory-level* rustup
override was pinning 1.89 for this folder and shadowing `rust-toolchain.toml`.
`rustup override list` revealed it; `rustup override unset` followed by
`rustup override set 1.92.0-x86_64-pc-windows-msvc` fixed it. If the build
suddenly complains about an old rustc, check `rustup override list` first.

The workspace `Cargo.toml` still says `rust-version = "1.89"`. Update it to
`1.92` for consistency.

---

## 4. Immediate blocker: `starpod-ui` does not compile

`crates/starpod-ui/build.rs` currently calls `slint_build::compile` four
times, once per `.slint` file:

```rust
fn main() {
    slint_build::compile("ui/design.slint").expect("the design tokens compile");
    slint_build::compile("ui/icons.slint").expect("the icon set compiles");
    slint_build::compile("ui/logo.slint").expect("the logo compiles");
    slint_build::compile("ui/primitives.slint").expect("the primitives compile");
}
```

`slint_build::compile` (see `slint-build-1.18.1/lib.rs:491`) compiles a file
as a *program*, i.e. it expects a `Window`. Every component in these files is
a library component, so the compiler emits:

```
Exported component 'AppButton' doesn't inherit Window. No code will be generated for it
Exported component 'Card' doesn't inherit Window. No code will be generated for it
... (one per component)
```

**The fix:** compile the files as a *library*, not as a program. Slint's API
for this is `slint_build::compile_library` (it takes a single `.slint` file
that `import`s the others, and generates one Rust module per file). Verify
its exact name and signature in the vendored `slint-build-1.18.1/lib.rs`
before using it — the four `pub fn`s visible at the top level of that file
were `compile`, `compile_with_config`, `compile_with_output_path` and
`print_rustc_flags`, so `compile_library` may live under a different path or
module; confirm rather than guess.

If `compile_library` turns out not to exist under that name, the fallback is
to give `starpod-ui` a single entry `.slint` file (e.g. `ui/app.slint`) that
`import`s `design.slint`, `icons.slint`, `logo.slint` and `primitives.slint`
and declares the top-level `Window`, then compile that one file with
`compile`. Either way, **one** compilation unit should end up producing the
Rust modules that `slint::include_modules!()` expands.

Note that `starpod-ui/src/lib.rs` currently does:

```rust
pub mod view;
slint::include_modules!();
```

With a library compile, `include_modules!()` expands to one module per
compiled `.slint` file, and the Rust module names are the file stems
(`design`, `icons`, `logo`, `primitives`). `view/` will hold the Rust view
models and will import from `crate::primitives::…`, `crate::design::…`, etc.

### 4.1 The one remaining hard Slint error

After the library/program fix, one genuine source error remains. In
`primitives.slint`, the button's focus ring reads:

```slint
visible: root.focus.has-focus;
```

`focus` is a reserved word in Slint — it is the name of `FocusScope`'s
`focus()` *function*, so `root.focus.has-focus` parses as "access fields of a
function". Rename the element id, e.g. `focus-scope := FocusScope { … }` and
`visible: root.focus-scope.has-focus;`.

### 4.2 Errors already fixed in this session

These were diagnosed against the vendored compiler and corrected in
`primitives.slint`; do not reintroduce them:

| Error | Cause | Fix |
|---|---|---|
| `No exported type called 'IconAlert'` | the icon set has `IconTriangleAlert` | renamed the import |
| `Unknown property placeholder-text in TextInput` | `TextInput` has no placeholder property in 1.18 | the hint is a separate `Text` shown while the value is empty |
| `Cannot convert string to styled-text` | `Tooltip.text` is `<styled-text>` | `@markdown("\{root.disabled-reason}")` |
| `Element 'Rectangle' does not have a property 'has-focus'` | `has-focus` lives on `FocusScope`, not `Rectangle` | the button now holds a `FocusScope` child |
| `Assignment on an 'in' property` | `in property <float> pulse` | changed to `private property <float> pulse` |

---

## 5. Verified Slint 1.18.1 facts

Everything below was read out of `i-slint-compiler-1.18.1` and
`i-slint-core-1.18.1` in the cargo registry. These cost real time to
establish; take them as ground truth.

### 5.1 There is no element-level `else`

`parse_if_element` (`parser/element.rs:348-358`) consumes `if`, an
expression, a colon, then one sub-element. There is no `else` branch. The
conditional element syntax is exactly:

```slint
if (condition) : Element { }
```

`else` exists only in *code blocks* (`parser/statements.rs:142`,
`if (x) { … } else { … }`). For a two-way choice, use two conditional
elements, or one element whose contents are chosen by a binding.

### 5.2 `states`, transitions and animations

- A state is `states [ name : { … } ]`, optionally `name when <expr> : { … }`.
  (`parse_state`, `parser/element.rs:794-860`.)
- A transition inside a state is `in { animate x { duration: 120ms; } }`;
  the `in` / `out` / `in-out` keywords introduce a transition block.
  (`parse_transition_inner`.)
- A top-level property animation is `animate x { duration: 900ms; }`, and
  **only bindings are allowed inside it**. (`parse_property_animation`,
  `parser/element.rs:730-765`.)

### 5.3 `Tooltip`

`Tooltip` is a **child element** of the thing it describes, not a wrapper:

```slint
Rectangle {
    Tooltip {
        text: @markdown("\{root.disabled-reason}");
    }
}
```

- `Tooltip` has `in property <styled-text> text;` (`builtin_elements.rs:3409-3421`).
- A plain string does **not** implicitly convert; it must be wrapped in
  `@markdown("\{…}")`.
- At most one `Tooltip` per parent element. It may be text mode (a `text`
  binding, no children) or custom mode (children, no `text` binding).
- `TooltipArea` is the compiler's *internal* mechanism — do not use it
  directly. `Tooltip` is lowered into a `PopupWindow` by
  `passes/lower_tooltips.rs`.
- `Tooltip` is not allowed inside `VerticalLayout`, `HorizontalLayout`,
  `GridLayout` or `FlexboxLayout`; put it inside a `Rectangle` child instead.

### 5.4 `TextInput` properties (the full list, 1.18)

`text`, `font-family`, `font-size`, `font-italic`, `font-weight`, `color`,
`selection-foreground-color`, `selection-background-color`,
`horizontal-alignment`, `vertical-alignment`, `wrap`, `letter-spacing`,
`line-height-factor`, `width`, `height`, `page-height`, `text-cursor-width`,
`input-type`, `input-method-hints`, `cursor-position-byte-offset`,
`anchor-position-byte-offset`, `has-focus` (out), `accepted`, `edited`,
`cursor-position-changed`, `key-pressed`, `key-released`, `enabled`,
`single-line`, `read-only`, `preedit-text`, `font-metrics` (out).

There is **no** `placeholder-text`.

`InputType` values: `Text`, `Password`, `Number`, `Decimal`, `Search`
(`i-slint-common-1.18.1/enums.rs:345`).

### 5.5 `has-focus` lives on these, not on `Rectangle`

`FocusScope`, `TextInput`, `Path`, `NativeButton`, `RadioGroup`. A plain
`Rectangle` has no `has-focus`; wrap the focusable content in a `FocusScope`.

### 5.6 `Path`

`builtin_elements.rs:3039`. `commands` is an `@fake in property <string>`
holding an **SVG path command string**, parsed by `lyon_extra`'s path parser.
Supported commands: `A C E H L M Q S T V Z a c e h l m q s t v x z`.

Properties: `fill`, `fill-rule`, `stroke`, `stroke-width` (a `<length>` — it
must carry a unit, `2px` not `2`), `stroke-line-cap`, `stroke-line-join`,
`stroke-miter-limit`, `fit` (`ImageFit`), `clip`, `anti-alias`, and the
viewbox properties `viewbox-x/y/width/height`. A non-zero `width`/`height`
scales the path to fit. `point-at(t)` and `angle-at(t)` are pure functions.

### 5.7 Other builtins worth knowing

- `SystemTrayIcon` — `builtin_elements.rs:3972`. The element ARCHITECTURE §6.4
  requires. Its exact property list has **not** been read yet; read
  `builtin_elements.rs:3972` onward before writing the tray.
- `Timer` — `interval` (mandatory), `running`, `triggered`, and
  `start()` / `stop()` / `restart()`. It is a pseudo-element: no geometry, no
  children.
- `PopupWindow` — `builtin_elements.rs:3273`; `show()`, `close()`, `is-open`,
  `close-policy`.
- `FocusScope` — `has-focus`, `enabled`, `focus-on-click`,
  `focus-on-tab-navigation`, `focus()`, `clear-focus()`, `key-pressed`,
  `key-released`, `capture-key-pressed`, `capture-key-released`,
  `focus-gained`, `focus-lost`, `focus-changed-event`.
- `TouchArea` — `enabled`, `has-hover`, `mouse-x/y`, `pressed`, `pressed-x/y`,
  `clicked`, `pointer-event`.
- `KeyBinding` — `keys: @keys(Control + X)`, `activated`.
- `Flickable`, `MenuItem`, `ContextMenu`, `DragArea`, `DropArea`,
  `SwipeGestureHandler`, `ScaleRotateGestureHandler`, `BoxShadow`, `Clip`,
  `Opacity`, `Layer`, `Transform`, `ComponentContainer`, `WindowItem`,
  `WindowMoveArea`.

### 5.8 The icon set that exists

`crates/starpod-ui/ui/icons.slint` holds 27 Lucide-derived components:
`IconActivity`, `IconCheck`, `IconChevronDown`, `IconChevronRight`,
`IconCircleCheck`, `IconCircleDot`, `IconCircleX`, `IconDownload`,
`IconGauge`, `IconGlobe`, `IconLayoutDashboard`, `IconLock`, `IconPencil`,
`IconPlus`, `IconPower`, `IconRefreshCw`, `IconRoute`, `IconScrollText`,
`IconSearch`, `IconServer`, `IconSettings`, `IconTrash2`, `IconTriangleAlert`,
`IconUpload`, `IconWifi`, `IconX`, `IconZap`.

Each is `export component Icon<Name> inherits Path` with `fill: transparent`,
`stroke: transparent`, `stroke-width: 2px`, round caps and joins. **Set
`stroke` at the use site** to theme it; the components are colorless by
design.

The source SVGs are in `crates/starpod-ui/ui/assets/icons/*.svg`;
`tools/convert_icons.py` regenerates the `.slint` from them
(`python tools/convert_icons.py crates/starpod-ui/ui/assets/icons
crates/starpod-ui/ui/icons.slint`).

`crates/starpod-ui/ui/logo.slint` holds the foxgirls fox-head mark as two
components — `FoxMarkFill` and `FoxMarkOutline` — because Slint's `Path`
carries one fill and one stroke for the whole shape and the mark needs both.
Regenerate with `python tools/convert_logo.py
crates/starpod-ui/ui/assets/Logo.svelte crates/starpod-ui/ui/logo.slint`.

---

## 6. The design system

`crates/starpod-ui/ui/design.slint` declares `export global Design` with
every token. The screens must refer to these names, never to a literal.

Surfaces: `background #161616`, `foreground #ededed`, `card #1c1c1c`,
`popover #1c1c1c`, `secondary #262626`, `muted #232323`, `accent #262626`.
Brand: `primary #de6161`, `primary-foreground #ffffff`.
Text: `muted-foreground #a3a3a3`.
Status: `destructive #e5484d`, `success #4a9e5c`, `warning #c99a3f`,
`info #a3a3a3`.
Lines: `border rgba(255,255,255,0.08)`, `input rgba(255,255,255,0.12)`,
`ring #de6161`.
Radii: `radius-sm 6px`, `radius-md 8px`, `radius-lg 10px`, `radius-xl 14px`.
Spacing: `space-xs 4px`, `space-sm 8px`, `space-md 12px`, `space-lg 16px`,
`space-xl 24px`, `space-2xl 32px`.
Type: `font-family "Inter Variable"`, `font-xs 12px`, `font-sm 14px`,
`font-md 16px`, `font-lg 18px`, `font-xl 24px`, `font-hero 48px`.

The theme is **dark-only** — foxgirls.org is dark-only.

### 6.1 The primitives that exist

`crates/starpod-ui/ui/primitives.slint` currently defines:

- `ButtonVariant` (default/outline/secondary/ghost/destructive),
  `ButtonSize` (sm/md/lg), `AppButton` — brand-red focus halo at 3px/50%
  opacity, a tooltip carrying `disabled-reason` for unavailable controls.
- `Card` — 1px ring at 10% foreground, `rounded-xl`, optional header.
- `BadgeVariant`, `Badge` — the pill.
- `StatusKind` (running/stopped/error/syncing), `StatusDot` — pulsing when
  syncing, driven by a `Timer`.
- `FieldKind` (text/password/number/combo), `Field` — label, value, error,
  and a combo mode.
- `SectionHeading` — eyebrow/title/description.
- `EmptyState` — title/description/action, required by GOAL.md §5 (no blank
  panels).
- `ErrorBanner`, `LoadingState`, `SettingRow`.

**Two conventions to preserve.** First, the card uses a *ring* (a 1px inner
line at 10% foreground), not a border — that is the site's treatment.
Second, a disabled control must be *visibly disabled with a reason*: the
button carries `disabled-reason` and shows it as a tooltip. GOAL.md §5
forbids dead placeholder controls, and this is how that requirement is met.

---

## 7. The crates, and how they fit together

### 7.1 `starpod-core` — pure domain and build pipeline

No I/O, no processes, no GUI. This is what makes GOAL.md §3's
"independently testable" configuration builder possible.

- `domain.rs` — `ProxyMode`, `LogLevel`, `TunSettings`, `UserPreferences`,
  `Node`, `ProxyGroup`, `StoredRule`, `Subscription`, `SubscriptionMode`,
  `SubscriptionPayload`, `SubscriptionSettings`, `BuildInput`,
  `EngineManagedSubscription`, `RuleProvider`, `RuleProviderBehavior`,
  `Finding`, `Severity`, `BuildOutcome`, `RuntimeConfig`.
- `config/` — the merge pipeline: `Subscription + User Preferences →
  Normalization → Merging → Validation → Runtime Configuration`.
  Entry point `starpod_core::build(input) -> BuildOutcome`.
- `rules/` — `RuleSpec`, `RuleTarget`, `RuleKind`, plus the grammar, parser
  and the translation layer into the engine's own rule format.
- `timestamp.rs` — the **single** definition of the app's timestamp format
  (RFC 3339): `format`, `parse`, `now`. Everything else re-exports it, so
  there is exactly one opinion about what a stored timestamp means.

Two design points worth preserving:

- `UserPreferences.allow_lan` is `Option<bool>`, not `bool`. `None` means
  "the user never expressed a preference"; `Some(false)` means "explicitly
  off". Without the distinction, a subscription shipping `allow-lan: true`
  could expose the proxy to the LAN merely because the user had never
  touched the setting — the "insecure default for convenience" GOAL.md §6
  rules out. The effective default is **off**.
- `Subscription` carries `node_count`, `group_count` and `last_error`.
  `node_count` is zero for an engine-managed subscription, because the app
  never sees its payload. `last_error` is redacted at the point it is
  created — a provider error can carry a URL, and a URL carries the token.
- `Subscription.last_updated` records the last fetch that *worked*; a failed
  refresh does not change it, so the user can see how stale the working data
  is.

### 7.2 `starpod-subscription`

Two narrow seams:

1. `provider::SubscriptionProvider` — *where the bytes come from*. The only
   I/O is an HTTP GET plus reading the response headers.
   `HttpSubscriptionProvider` and `StaticSubscriptionProvider` (canned bytes,
   for tests). `FetchRequest::new(url).with_authorization(v).with_timeout(d)`.
   `FetchResponse::new(body).with_header(name, value)`; `.userinfo()` reads
   the `subscription-userinfo` header.
2. `pipeline::parse_payload(bytes, id, label) -> ParsedBody` — *what the bytes
   mean*. Pure: no clock, no filesystem, no engine, no GUI.

The pipeline is: `bytes → sniff format → YAML parse → shape check →
per-entry validation → SubscriptionPayload`.

**Namespacing happens in the pipeline, not in the provider.** `parse_payload`
is handed the subscription's label and is the only place a node name becomes
`<label> / <node name>`.

Also: `refresh::refresh(provider, request, metadata, cache, now) ->
RefreshOutcome` — the cache is updated only on success, never left
half-written. `SubscriptionCache` / `SubscriptionCacheSet` are in-memory;
the on-disk cache is the store's job. `schedule::{RefreshSchedule,
next_due_at}`.

Facts about the engine that constrain this crate (cited in its own docs):

- The engine's own subscription parser requires a `proxies:` key and will not
  unwrap base64 (`meow-config-0.29.0/src/subscription.rs:336-343`). The *app*
  accepts base64-wrapped YAML as well, because that is what panels emit.
- The engine's fetcher discards the `subscription-userinfo` header
  (`meow-config-0.29.0/src/subscription.rs:278-288` calls
  `internal_http::fetch`, which returns only the body). The quota is
  therefore only available to the app.

### 7.3 `starpod-store`

Owns everything that puts bytes on disk, plus secrets. Depends on neither the
GUI nor the engine.

| Module | Responsibility |
|---|---|
| `paths` | per-platform config/data/cache directories |
| `state` | the persisted files; corruption and migration handling |
| `migration` | the schema-version machinery and the registry |
| `atomic` | atomic replacement (temp → fsync → rename) |
| `permissions` | owner-only readability |
| `secret` | the `SecretStore` trait, the platform stores, the fallback |
| `runtime_config` | writing the generated engine configuration |
| `redact` | making a secret safe to log or display |

**The separation is the guarantee.** GOAL.md §3 requires that a subscription
update never silently overwrites a user-defined setting, and this crate
enforces it *structurally*: the user's own state lives in one file, the
subscription metadata in another, and the cached payloads in a third. There
is no code path where a subscription write can reach a preference file.

| File | Contents | Secret? |
|---|---|---|
| `config/user-state.json` | preferences, rules, nodes, groups | no |
| `data/subscriptions.json` | per-subscription metadata | **no** — holds secret-store *keys* |
| `cache/subscriptions/<id>` | the last valid payload | maybe (node passwords) |

Every file carries a `schema` number. A file that will not parse is **moved
aside** — never deleted, never silently overwritten — and the app starts from
the defaults, reporting what happened on the Logs screen
(`LoadOutcome::{Corrupt, Future, Unmigratable, Missing}`).

`AppPaths` resolves:

| Platform | Config | Data | Cache |
|---|---|---|---|
| Windows | `%APPDATA%\Starpod` | `%LOCALAPPDATA%\Starpod` | `%LOCALAPPDATA%\Starpod\cache` |
| macOS | `~/Library/Application Support/Starpod` | same | `~/Library/Caches/Starpod` |
| Linux | `$XDG_CONFIG_HOME/Starpod` | `$XDG_DATA_HOME/Starpod` | `$XDG_CACHE_HOME/Starpod` |

Key paths: `user_state_file()`, `subscription_meta_file()`,
`subscription_cache_dir()`, `subscription_cache_file(id)`,
**`runtime_config_file()`** → `data/runtime-config.yaml`,
`controller_dir()` → `data/controllers`,
`fallback_secret_file()` → `data/secrets.json`.

`subscription_cache_file` validates the id before using it as a file name
(`safe_file_name`: non-empty, ≤128 chars, `[A-Za-z0-9_-]` only) so a crafted
id cannot escape the cache directory.

`SecretStore` has four operations — `set`, `get`, `remove`,
`is_encrypted` — plus `backend_name`. It is object-safe, so the app can hold
a `Box<dyn SecretStore>`. `open_secret_store(fallback_path) -> SelectedStore`
picks DPAPI on Windows, the Keychain on macOS, the Secret Service on Linux,
and a **plain-file fallback** where none of those exists. The fallback is
*not* encrypted; `is_encrypted()` reports the difference so the Settings
screen can state the weaker guarantee plainly rather than implying a
protection that is not there.

### 7.4 `starpod-engine`

The `ProxyEngine` trait is the **only** thing the GUI and business logic see.
Seven methods: `start`, `stop`, `reload`, `status`, `validate`, `set_tun`,
`logs`. Nothing outside this crate names a REST route, spawns a process, or
knows the word "meow".

Supporting types: `EnginePhase` (Stopped/Starting/Running/Stopping/Failed),
`EngineStatus`, `EngineHandle`, `ExitInfo`, `StopRequest`,
`ValidationReport { ok, engine_error, warnings }`.

`ValidationReport` is a report and not a `Result` on purpose: the engine-side
check (`meow -t`) is authoritative, but the app-side layer also produces
*warnings* that must not block the apply. A bare `Result<(), E>` would force
the app either to lose those warnings or to treat them as failures.

`MeowEngine::new(binary, config_path)` / `MeowEngine::installed(config_path)`.
The config path is **required** — a file the app does not know the path of is
a file the app cannot validate against. The app supplies
`AppPaths::runtime_config_file()`.

**The apply path** (the order is the point):

1. Build the candidate YAML from the app's `RuntimeConfig`.
2. Validate it with the engine's own parser (`meow -t`), against a file
   beside the live one (`validate::candidate_path(live_path)`). Nothing has
   been applied, so a failure here costs nothing.
3. Write the candidate to the live path, atomically.
4. Apply it — by starting the engine (first start) or `PUT /configs`
   (a running engine).

The running configuration is never modified until the replacement has
validated. That is GOAL.md §3's "do not modify the running configuration
until the replacement validates", and it is a property of the sequence
above, not a promise.

**Why the app does not use engine-managed subscriptions when it can avoid
it:** `PUT /configs` does *not* write the backing file. Only `PUT
/configs/save` (which the app never calls) and the engine's **background
subscription refresh**
(`meow-app-0.29.0/src/subscription_refresh.rs:421`) do. The second one would
rewrite the app's file behind the app's back, in the engine's own normalized
form. The app fetches, parses, caches and merges subscriptions itself, so
the file it writes is the whole truth.

Other modules: `config::EngineDocument` (app config → engine YAML, with
`with_controller`), `download` (installs the engine binary; `install()`,
`is_installed()`, `installed_binary_path()`, `asset_for_current_platform()`),
`secret::random_secret()`, `log` (`LogBuffer`, `LogStream`, `Redactor`,
`redact_urls`), `rest::ControllerClient`, `supervisor::Supervisor`,
`tun` (`is_elevated`, `elevation_command`, `tun_patch`, `needs_elevation`,
`elevation_explanation`), `validate::validate_config`.

### 7.5 The engine's contract — the facts that matter

Verified against the vendored meow source. Full detail is in
`docs/research/meow-integration-contract.md`.

- **meow v0.29.0** is a 13-crate Rust workspace, MIT, mihomo/Clash.Meta
  compatible. It is released as a **subprocess**, not a library — which is
  exactly why it can be driven this way.
- `RawConfig` is `#[serde(rename_all = "kebab-case")]` with **no**
  `deny_unknown_fields` (`meow-config-0.29.0/src/raw.rs:182-184`) → **unknown
  keys are silently ignored**. 47 top-level fields. This is why the app's
  config keys must be exactly right; a typo is a silent no-op.
- CLI flags (`meow-app-0.29.0/src/main.rs:39-108`): `-f/--config`,
  `-d/--directory`, `-t/--test`, `-m/--geodata-mode`, `--config-string<base64>`,
  `--ext-ctl`, `--ext-ctl-tls/unix/pipe/routing-mark`, `--secret`,
  `--ext-ui`, `--post-up/--post-down`, `--age-secret-key`,
  `--no-external-plugins`. Subcommands: `install`, `uninstall`, `status`.
- `-t` forces `meow_config::set_offline_validate(true)` → **no remote fetches
  during validation**. That is what makes pre-apply validation safe.
- `--secret` overrides `config.api.secret` *after* the config is built
  (`main.rs:612-615`); the API auth check reads it at
  `meow-api-0.29.0/src/routes.rs:152`.
- The API listener is bound exactly once at startup (`main.rs:1508-1571`) and
  is **never rebound** by a reload → `external-controller` must live in the
  config file.
- The provider cache dir is derived from the **config file's location**
  (`meow-config-0.29.0/src/lib.rs:4210-4222`); `--config-string` passes
  `None`, where a `file:` rule-provider hard-errors. → **Always use a real
  config file**, never `--config-string`, for a configuration with file rule
  providers.
- The controller file `{pid, controller, secret, config}` is written to
  `<pid>.json` under `MEOW_RUNTIME_DIR`
  (`meow-common-0.29.0/src/controller_file.rs:24`), else
  `%LOCALAPPDATA%\meow\controllers` (Windows) / `~/.cache/meow/controllers`
  (Linux) / `~/Library/Caches/meow/controllers` (macOS). The app points the
  engine at its own `AppPaths::controller_dir()` so that Starpod's controller
  file never collides with another meow's on the same machine.
- **Graceful shutdown**: on Windows, `CTRL_BREAK_EVENT` to a
  `CREATE_NEW_PROCESS_GROUP` child — *not* `CTRL_C_EVENT`. On unix, SIGTERM.
  `supervisor.rs` already implements this.

---

## 8. What to build next, in order

This is the recommended order. Steps 1–4 are the critical path; 5 and 6 can
run in parallel.

### Step 1 — Make `starpod-ui` compile

1. Replace the four `slint_build::compile` calls with a single **library**
   compile (see §4). Confirm the exact API name in the vendored
   `slint-build-1.18.1/lib.rs`.
2. Rename the button's `focus` element id to `focus-scope`
   (§4.1) and update the `has-focus` reference.
3. `cargo build -p starpod-ui` until clean.

### Step 2 — Build the app shell

A top-level `Window` with:

- a left sidebar: the foxgirls mark, the five nav items with their Lucide
  icons (`IconLayoutDashboard`, `IconServer`, `IconRoute`, `IconSettings`,
  `IconScrollText`), the engine status at the bottom;
- a content area that switches between the five screens;
- a header on the content side showing the current screen's title.

Every screen must have a loading state, a success state, a failure state and
an empty state (GOAL.md §5).

### Step 3 — Build `starpod-app`

```
crates/starpod-app/
  Cargo.toml        tokio, slint, all four starpod crates
  build.rs          if the UI needs to live here instead; otherwise reuse starpod-ui
  src/lib.rs        AppState, the event bus, the commands
  src/state.rs      the single source of truth the UI renders
  src/actions.rs    every user action, as an async command
  src/main.rs       the tokio runtime + the Slint event loop
```

Architecture rules from GOAL.md and ARCHITECTURE §6.3:

- **Business logic lives in Rust, never in `.slint`.** A `.slint` file
  declares properties and callbacks only; every handler is a Rust function
  that mutates `AppState` and then pushes the new state to the UI.
- **Async must not freeze the UI.** Use `slint::spawn_local` for UI-side
  futures and `slint::invoke_from_event_loop` to push state back.
- Run the tokio runtime on its own thread and the Slint event loop on the
  main thread; bridge them with a channel.
- The UI crate holds **no** logic. It holds view models. `starpod-ui`'s
  `view/` module declares `dashboard`, `servers`, `rules`, `settings`, `logs`
  (currently one-line placeholders).

### Step 4 — Wire the central path

This is the functionality GOAL.md §11 says must be verified:

```
subscription fetch → parse → validate → cache
      → merge with user preferences and rules
      → RuntimeConfig
      → engine YAML
      → meow -t
      → write atomically
      → start / reload
```

Concretely:

1. Build the `BuildInput` from the store's `UserState`, the subscription
   payloads (from the on-disk cache or a fresh fetch), and the
   engine-managed subscription declarations.
2. `starpod_core::build(input)` → `BuildOutcome`. If it produced errors,
   **do not** apply; surface them.
3. `MeowEngine::validate(&config)` — the engine's own parser is the
   authority.
4. Write the YAML to `AppPaths::runtime_config_file()` atomically, then
   `start` or `reload`.

Preserve these invariants while wiring:

- A subscription refresh must never modify the user's preferences, rules,
  nodes or groups.
- The user's rule order is re-asserted after a refresh — the app re-applies
  the user's rules after the subscription's rules, in the user's order.
- A failed refresh keeps the last valid payload; it never clears the cache.
- The running configuration is not modified until the replacement validates.
- A failed apply leaves the running proxy alone and surfaces the error.
- The engine's `external-controller` must be in the config file, because the
  listener is bound once at startup and never rebound.

### Step 5 — The system tray

`SystemTrayIcon` (Slint ≥ 1.17, hence the toolchain bump). Read its
properties at `builtin_elements.rs:3972` before writing it. The tray needs
at minimum: show/hide the window, a start/stop the proxy action, and a
quit action. GOAL.md §4 requires window + tray.

### Step 6 — Tests, CI, polish

- The **integration test** the project is missing: subscription → runtime
  config → proxy execution. Use the mocked provider
  (`StaticSubscriptionProvider`) and deterministic fixtures.
- Test suites for anything not yet covered.
- CI on Linux, Windows and macOS, on every PR (the user's standing
  decision).
- `cargo fmt` and `cargo clippy -- -D warnings`.
- Security review against GOAL.md §6: credential protection, no secrets in
  logs, validation of untrusted subscription data, safe handling of remote
  config updates, no accidental exposure of local services or management
  interfaces, correct lifecycle management, graceful shutdown, proper error
  propagation. **Check the "insecure default for convenience" line
  explicitly** — the `Option<bool>` on `allow_lan` is the main defense.
- README, user guide, build/release doc.

### Known small items

- The workspace `Cargo.toml`'s `windows` feature list lacks
  `Win32_Security_Authorization`; the store currently adds it locally in its
  own `[target.'cfg(target_os = "windows")'.dependencies]`. Widen the
  workspace list and drop the local override.
- `rust-version = "1.89"` in the workspace `Cargo.toml` should become
  `"1.92"`.

---

## 9. The standing user decisions

Carried from the earlier session and still binding:

| Question | Decision |
|---|---|
| Platform scope | **Linux, Windows, macOS ARM** — only these three |
| Subscription modes | **both**, chosen **per subscription** |
| TUN | **yes**, with an elevation prompt |
| UI surface | **window + system tray** |
| Rules | **structured + raw text mode** |
| Rule order | the **app re-asserts** it after a subscription refresh |
| Subscription auth | **URL token + optional header** |
| CI | **all three platforms, on every PR** |

GOAL.md §8 is the architecture decision gate: do not start
architecture-dependent functionality on arbitrary assumptions; ask only about
choices that cannot be safely resolved from technical research.

---

## 10. Reference: the docs already written

- **`GOAL.md`** (798 lines) — the requirements. Sections: 1 Execution
  Instructions, 2 Initial Research, 3 Core Application Architecture (Proxy
  Core Integration, Subscription Management, Runtime Configuration Builder,
  Custom Rules), 4 Desktop Application and UI (Initial Screens, UI
  Engineering Requirements), 5 Persistence, 6 Security and Reliability,
  7 Testing and Quality, 8 Architecture Decision Gate, 9 Implementation
  Workflow, 10 Engineering Standards, 11 Definition of Done.
- **`docs/research/ARCHITECTURE.md`** (1565 lines) — the implementation
  dossier. The code comments cite its `§` numbers; keep that convention.
- **`docs/research/foxgirls-design.md`** (191 lines) — the extracted tokens.
- **`docs/research/meow-integration-contract.md`** (400 lines) — the
  engine's real contract, with source citations.

## 11. Reference: the vendored engine source

Ground truth lives at
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/meow-*-0.29.0/`.
The 13 crates are `meow-app`, `meow-common`, `meow-config`, `meow-api`,
`meow-log`, `meow-provider`, `meow-protocol`, `meow-proxy`,
`meow-rule`, `meow-runtime`, `meow-tls`, `meow-tunnel`, `meow-util` (verify
the exact set before relying on it).

Likewise `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` holds
`i-slint-compiler-1.18.1`, `i-slint-core-1.18.1`, `i-slint-common-1.18.1`,
`slint-1.18.1`, `slint-build-1.18.1`. These are the reference for every
Slint API question — do not rely on recollection.

---

## 12. The one-paragraph version

Four crates are done and tested (344 tests); the fifth (`starpod-ui`) has its
design tokens, a 27-icon set, the foxgirls mark and a set of primitives, but
does not compile because it is being built as a Slint *program* instead of a
*library* and because one element id shadows a reserved word; the sixth
(`starpod-app`) is an empty placeholder. The remaining work is: fix the UI
build, write the app shell and the five screens, wire the
subscription → merge → validate → engine path, add the tray, then tests, CI
and docs. Every external API fact needed is in §5 (Slint) and §7.5 (meow),
each with a file-and-line citation, so the next agent does not have to
re-derive them.
