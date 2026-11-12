# Starpod — Architecture Dossier

**Status:** implementation-ready. Every claim below is verified first-hand against the vendored
`meow-*-0.29.0` crate source, the real downloaded release binary, or the upstream
`foxgirlsorg/foxgirls.org` repo. Sources: `[src]` = file:line in
`~/.cargo/registry/src/index.crates.io-*/meow-*-0.29.0/`; `[bin]` = the real
`meow-v0.29.0-*.zip/.tar.gz` from the v0.29.0 GitHub release, unpacked and executed.

**Target platforms (user-pinned):** Linux, Windows, macOS-on-ARM. x86_64 macOS is out of scope
for v1; the macOS artifact is `aarch64-apple-darwin` only.

---

## 0. Decisions already taken (by the user, in conversation)

| Decision | Choice | Consequence for this document |
|---|---|---|
| meow core | **Do not build from source.** App downloads the prebuilt release binary. | Subprocess supervisor + REST client, not a linked library. |
| Platform scope | **Linux, Windows, macOS-ARM.** | Release-asset mapping, shutdown, data dirs, secret storage are all platform-conditional. |
| Subagents | **None.** | All research and implementation done inline in this session. |
| Visual reference | foxgirls.org | Design tokens in `foxgirls-design.md`, reused verbatim here. |
| Subscription ownership | **Both, per subscription** — a per-subscription toggle chooses app-managed or engine-managed. | Two code paths; §3.2 specifies how they coexist without conflicting. |
| TUN mode | **In v1**, with an elevation prompt. | The *child* must run elevated, not the GUI; §4.5. |
| Window close | **Hide to the system tray**; the proxy keeps running. | Two exit paths (close→hide, quit→stop); §6.4. |
| Rule editing | **Structured editor + a raw-text advanced mode.** | Two editors, one grammar; the raw mode is validated by the same parser; §3.3. |
| Subscription auth | **URL token + an optional `Authorization` header.** | One extra field on the model, one header on the request. |
| Rule order vs. engine refresh | **The app re-asserts the order after any engine refresh.** | A supervision loop watches `last_updated`; §4.6. |
| CI | **Linux x64, Windows x64, macOS ARM64 — all three, every PR.** | §8.3. |
| Persistence | **JSON** for state, YAML for the generated config and the cache. | §3.1, §5. |

---

## 1. Verified facts (the foundation)

### 1.1 Release distribution

Stable channel naming: `meow-v<version>-<rust-target-triple>.<ext>`. The **exact** assets that
exist for our target set (fetched from the GitHub releases API for `v0.29.0`):

```
meow-v0.29.0-aarch64-apple-darwin.tar.gz              ← macOS ARM  (our macOS target)
meow-v0.29.0-x86_64-apple-darwin.tar.gz                (not shipped by us)
meow-v0.29.0-aarch64-pc-windows-msvc.zip               (Windows ARM)
meow-v0.29.0-x86_64-pc-windows-msvc.zip                ← Windows x64 (our Windows target)
meow-v0.29.0-i686-pc-windows-msvc.zip                  (not shipped by us)
meow-v0.29.0-x86_64-unknown-linux-gnu.tar.gz            ← Linux x64 (our Linux target)
meow-v0.29.0-aarch64-unknown-linux-gnu.tar.gz          ← Linux ARM64
meow-v0.29.0-x86_64-unknown-linux-musl.tar.gz          ← Linux x64, static (recommended)
meow-v0.29.0-aarch64-unknown-linux-musl.tar.gz
+ armv7 / riscv64gc variants (not shipped by us)
```

Each binary asset has a sibling `.sha256` file in `sha256sum` format:

```
3ffd602369e2b852eca6348ba10689cc7f1f49f3f5935b8f8810a7d4e88dc275 *meow-v0.29.0-x86_64-pc-windows-msvc.zip
```

**Parse rule:** split on whitespace, take field 0 as the hex digest.

**Archive layout (verified by unpacking):** the archive contains a **single top-level directory**
named after the asset. The extractor must descend into it — a naive "read files at archive root"
extractor gets nothing.

```
meow-v0.29.0-x86_64-pc-windows-msvc/
├── meow.exe              16,044,032   ← the proxy core
├── meow-cli.exe           1,460,736   ← reference REST client (docs only)
├── wintun.dll              427,552   ← TUN support, bundled
├── LICENSE
├── LICENSE.wintun.txt
├── README.md
└── skills/meow-cli/SKILL.md             ← authoritative CLI semantics doc
```

**No geodata files are bundled** — see §1.6.

**Trust limitation (must be stated to the user):** the project publishes **only sha256**, fetched
from the same release as the binary. This defends against corruption and truncated downloads but
**not** against a compromised GitHub release. There is no signature to anchor trust. Not a
blocker, but the app must not claim more assurance than it has.

### 1.2 CLI surface

| Flag | Meaning |
|---|---|
| `-f, --config <CONFIG>` | Config file path. Default `config.yaml`. |
| `-t, --test` | **Validate config and exit** — never starts the proxy. |
| `--config-string <CONFIG_STRING>` | base64-encoded config (CLI counterpart of REST `payload`). |
| `--ext-ctl <ADDR>` | Override external controller address. |
| `--ext-ctl-unix`, `--ext-ctl-pipe` | Unix-socket / named-pipe controller variants. |
| `--ext-ctl-tls`, `--ext-ctl-routing-mark` | TLS controller / routing mark. |
| `--secret <SECRET>` | REST API bearer secret. |
| `--ext-ui`, `--ext-ui-name`, `--ext-ui-url` | External web panel. |
| `--geodata-mode`, `--no-external-plugins`, `--post-up/--post-down`, `--age-secret-key` | Misc. |
| `-d, --directory`, `-m, --geodata-mode`, `-V, --version` | Misc. |

Subcommands: `install`, `uninstall`, `status`, `help`.

**Key design consequence:** the app always passes `--ext-ctl` (loopback) and `--secret` explicitly
so it knows the controller address and secret without reading the controller file, and so the app
is immune to the "engine hasn't written its controller file yet" race.

### 1.3 REST controller API

Axum router, `Bearer` secret auth on the whole router (`401` when missing/wrong). `/ui` is
deliberately **unauthenticated** (matches upstream mihomo) — the app must never expose it.

Full route table (from `meow-api-0.29.0/src/routes.rs:194-289`) is reproduced in
`meow-integration-contract.md` §4. The routes the app actually uses:

| Route | Purpose |
|---|---|
| `GET /configs` | `ConfigResponse` — mode, log_level, mixed_port, tun_enable, tun, … |
| `PUT /configs` | **Full reload.** Body `{path}` or `{payload}` (base64). `?force=true`. |
| `PATCH /configs` | Partial. Body `{mode, log-level, tun}`. |
| `GET /proxies`, `GET /proxies/{name}`, `GET /group`, `GET /group/{name}` | Node/group state. |
| `PUT /proxies/{name}` | Select a member in a `select` group. |
| `GET /proxies/{name}/delay`, `GET /group/{name}/delay` | Latency probe. |
| `GET /rules`, `POST /rules`, `PUT /rules/{index}`, `DELETE /rules/{index}`, `POST /rules/reorder` | Live rule list mutation. |
| `GET /connections`, `DELETE /connections`, `DELETE /connections/{id}` | Connection list / close. |
| `GET /traffic`, `GET /statistics`, `GET /logs`, `GET /logbook` | Telemetry for the UI. |
| `GET /memory`, `GET /metrics`, `GET /version` | Diagnostics. |
| `GET /dns/results`, `POST /dns/query` | DNS introspection. |
| `GET /providers/proxies`, `GET /providers/rules` | Provider state (subscription freshness). |

**`PUT /configs` semantics (verified):**
1. Body must contain exactly one of `path` / `payload`; neither → `400 "request body must contain
   'path' or 'payload'"`.
2. `payload` must be **standard** base64 of UTF-8 bytes.
3. **Syntax errors are always `400`, even with `force=true`** — a malformed YAML never reaches the
   running engine.
4. Duplicate `subscriptions:` names → `400 "duplicate subscription name"`, never committed.
5. Without `force=true`, broken proxies/groups/rules are skipped with a warning and the rest is
   applied (unless the config sets `strict: true`). With `force=true` the whole config applies.
6. A successful reload **closes all open connections**.

**The app must use `path`, not `payload`** — it writes the YAML to disk atomically, then PUTs the
path. That gives the user a readable, diffable config file, and lets the app validate *the exact
same bytes* with `meow.exe -t -f <path>` before reloading.

### 1.4 Config top-level keys accepted by meow (`RawConfig`, `meow-config-0.29.0/src/raw.rs:184+`)

```
port, socks_port, mixed_port, allow_lan, bind_address, mode, log_level,
external_controller, external_ui, external_ui_name, external_ui_url, secret,
dns, proxies, proxy_groups, proxy_providers, rules, rule_providers, sub_rules,
subscriptions, strict, tproxy_port, tproxy_sni, routing_mark, tcp_connect_timeout,
connection_notes, logbook, traffic_ledger, hosts, sniffer, listeners,
authentication, skip_auth_prefixes, geodata, geox_url, tun, max_connections,
tcp_keepalive(+idle/interval/probes), tcp_idle_timeout, firewall, udp
```

Note `secret` is a config key — the app must **not** put the controller secret in the generated
YAML (it is passed via `--secret` on the command line instead, and the YAML is written to a
user-readable location).

### 1.5 Rule grammar (verified token-level, `meow-rules-0.29.0/src/parser.rs:128-240`)

A rule line is `TYPE,payload,target[,flags...]` — **at least 3 comma-separated parts** required
(`"rule needs at least 3 parts"`).

**Rule types meow actually supports** (from `meow-common-0.29.0/src/rule.rs` `RuleType::as_str`,
cross-checked against the parser's match arms — these two agree exactly):

```
DOMAIN  DOMAIN-SUFFIX  DOMAIN-KEYWORD  DOMAIN-REGEX  DOMAIN-WILDCARD
GEOIP  SRC-GEOIP  GEOSITE
IP-CIDR  IP-CIDR6  SRC-IP-CIDR  IP-SUFFIX  SRC-IP-SUFFIX  IP-ASN  SRC-IP-ASN
SRC-PORT  DST-PPORT->DST-PORT  IN-PORT  NETWORK  DSCP
PROCESS-NAME  PROCESS-PATH  UID
IN-NAME  IN-TYPE  IN-USER
RULE-SET  SUB-RULE
AND  OR  NOT  MATCH
```

**Flags** (4th+ part, comma-separated within that field): `no-resolve`, `src`. `src` implies
`no-resolve` (verified: `flags.no_resolve |= flags.is_src`).
`IP-CIDR`/`IP-SUFFIX` accept `src` as a suffix; `SRC-*` variants hard-code it.

**Targets:** `DIRECT`, `REJECT`, `REJECT-DROP`, `COMPATIBLE`, `PASS`, `PASS-RULE` (built-in
outbounds, registered by the engine — `meow-config-0.29.0/src/lib.rs:2332-2411`), plus the name
of any proxy or proxy-group.

**Logic rules:** `AND,((RULE1),(RULE2)),TARGET` — parenthesised sub-rules, nested to a bounded
depth (`MAX_LOGIC_DEPTH`).

**`RULE-SET` payload** is `provider_name,behavior` where behavior is `domain` | `ipcidr` (aliases
`ip-cidr`) | `classical`. The rule is `RULE-SET,<provider>,<behavior>,<target>`.

**`GEOSITE` payload** may carry an attribute: `microsoft@cn`. The category is the part before `@`.

**Important:** `GEOIP` requires a GeoIP database and `IP-ASN` requires an ASN MMDB; if absent the
rule is a hard parse error (`"GEOIP rule requires a GeoIP database, but none is configured"`).
This means **rule validation must be aware of whether geodata is present** — see §4.4.

### 1.6 Geodata — resolved, the app ships none

`meow-config-0.29.0/src/geodata.rs`:

| Dataset | Default URL |
|---|---|
| Country MMDB | `https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/country.mmdb` |
| ASN MMDB | `https://github.com/P3TERX/GeoLite.mmdb/releases/latest/download/GeoLite2-ASN.mmdb` |
| Geosite | `https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/geosite.dat` |

- **meow has no `geoip.dat` consumer** — `geox-url.geoip` is accepted and **ignored with a
  warning**. The GeoIP MMDB URL is set via `geox-url.mmdb`.
- **Downloads are lazy and targeted** — the engine fetches only the databases the config
  actually references, before the rule context is built. No unconditional boot-time sweep.
- `geodata.auto-update` (default `false`) + `auto-update-interval` (default `24` hours).
- `resolve-geodata-egress`: `DIRECT`/`COMPATIBLE`/`PASS` fetch direct; `REJECT`/`REJECT-DROP` do
  not fetch.

**Consequences:** the app ships no data files and performs no geodata fetching. It may emit
`GEOIP`/`GEOSITE` rules; the engine resolves them. The UI must show a *loading* state on the
first apply of a geo-rule config (the engine downloads a few MB then), and the Logs screen should
surface that download. `geodata.auto-update: true` moves it off the critical path.

### 1.7 Shutdown — resolved, platform-conditional

`meow-app-0.29.0/src/main.rs:142-175` — `ShutdownSignal::wait()`:
- **Windows**: `tokio::signal::ctrl_c().await` → the engine's graceful trigger is a console
  control event.
- **Unix**: additionally waits for `SIGTERM` (and `SIGINT`).

The app must therefore deliver:

| Platform | Graceful signal | Fallback |
|---|---|---|
| Windows | `CTRL_BREAK_EVENT` to the child's process group. Spawn the child with `CREATE_NEW_PROCESS_GROUP` (`windows-sys` `CREATE_NEW_PROCESS_GROUP = 0x200`). `CTRL_C_EVENT` is unusable — it cannot be targeted at one group and would also hit the app. | `TerminateProcess` after a timeout. |
| Linux/macOS | `SIGTERM` to the child pid (`tokio::process::Child::kill` is SIGKILL; use `nix::sys::signal::kill(pid, SIGTERM)` or `libc::kill`). | `SIGKILL` after a timeout. |

The engine installs its signal handler **only after startup completes**, so the app must wait for
the controller to answer before it ever attempts a graceful stop (else the signal is lost).

**The app must not use the engine's Windows-service mode** — it supervises the process directly.

### 1.8 Controller discovery (the app won't need it, but must isolate from it)

`meow-common-0.29.0/src/controller_file.rs` — when `external-controller` is bound, meow writes
`<pid>.json` into:

| Platform | Directory |
|---|---|
| Windows | `%LOCALAPPDATA%\meow\controllers\` |
| macOS | `~/Library/Caches/meow/controllers\` |
| Linux | `~/.cache/meow/controllers\` |
| Linux (root) | `/var/run/meow/controllers\` |

Overridable with env `MEOW_RUNTIME_DIR`. The file's `Debug` impl **redacts the secret**, and the
directory is `0700`/file `0600` on unix.

**The app sets `MEOW_RUNTIME_DIR` to its own per-app directory**, so its instance never collides
with another meow on the machine and never gets swept up by one.

### 1.9 Subscription support — both an engine feature and a format contract

**The engine has first-class subscription support:** `GET /api/subscriptions`,
`POST /api/subscriptions`, `DELETE /api/subscriptions/{name}`,
`POST /api/subscriptions/{name}/refresh`. Config key `subscriptions:` (`RawSubscription`,
`raw.rs:965+`).

**Subscription payload formats meow accepts** (`meow-config-0.29.0/src/subscription.rs`):
`parse_subscription_yaml(text, strict)` → `SubscriptionData { proxies, proxy_groups, rules }`.
So a subscription body is **YAML in Clash/Mihomo config shape** (not a URI list) — `proxies:`
entries are `HashMap<String, serde_json::Value>`, i.e. arbitrary maps that are validated later by
`proxy_parser.rs`.

**Proxy URI schemes** meow's `proxy_parser.rs` dispatches on (protocol dispatch at :329-530):

```
ss  trojan  vless  vmess  hysteria2  anytls  snell  http  socks5
```

each with a feature-gated fallback that errors when the protocol is compiled out.

**Expiry / quota header:** `meow-config-0.29.0/src/subscription_userinfo.rs` — the
`subscription-userinfo` HTTP header, parsed as `;`-separated `key=value` pairs:

```
upload=1234; download=5678; total=1073741824; expire=1700000000
```

Keys: `upload`, `download`, `total`, `expire`. `expire` is a **unix timestamp**; amounts accept
integers or floats. Any field may be absent.

**This is exactly the metadata the app must surface** (traffic used / total / expiry) — and it is
the reason the app should fetch subscriptions *itself* rather than delegating to meow: the app
needs the header, the cache, and the expiry, and none of those are exposed by meow's REST
subscription routes.

### 1.10 Slint 1.18.1 — the API is the classic one

Verified against the vendored `slint-1.18.1` / `i-slint-core-1.18.1` source:
- **There is no `slint::init()` and no `AppHandle`** in 1.18.1. The classic `ComponentHandle`
  model (`slint::ComponentHandle`, `i-slint-core/api.rs:1073`) is current.
- `slint::invoke_from_event_loop` (`i-slint-core/api.rs:1286`) — schedule work on the UI thread.
- `slint::spawn_local` (`slint/lib.rs:355`) — spawn onto the Slint event loop.
- `VecModel` / `ModelRc` (`i-slint-core/model.rs:506-660`).
- **tokio interop:** tokio futures need an `async_compat::Compat` wrapper when driven from the
  Slint thread; `#[tokio::main]` is discouraged in favour of `tokio::task::block_in_place(slint::run_event_loop)`.
- `slint::BackendSelector` for backend choice.

### 1.11 foxgirls design tokens

Full extraction in `foxgirls-design.md`. The values the app will use verbatim:

```
--background #161616   --foreground #ededed   --card #1c1c1c   --popover #1c1c1c
--primary    #de6161   --primary-foreground #ffffff   --secondary #262626
--muted      #232323   --muted-foreground #a3a3a3      --accent #262626
--destructive #e5484d --border rgb(255 255 255 / 0.08)   --input rgb(255 255 255 / 0.12)
--ring       #de6161   --brand #de6161        --radius 0.625rem
font: "Inter Variable", "Segoe UI", system-ui, sans-serif
```

Contrast: `#de6161` on `#161616` ≈ 5.1:1 (AA); `#a3a3a3` ≈ 7.4:1; `#ededed` ≈ 14.6:1.

Derived semantic tokens the app adds (the site has none, a proxy client needs them):
`success #4a9e5c`, `warning #c99a3f`, `info` = `--muted-foreground`.

---

## 2. Architecture overview

```
┌───────────────────────────────────────────────────────────────────────────┐
│                              starpod-ui (Slint)                            │
│  Dashboard · Servers · Rules · Settings · Logs                            │
│  ui/*.slint — presentation only. No business logic.                        │
└───────────────────────────────┬───────────────────────────────────────────┘
                                │ commands / state updates
┌───────────────────────────────▼───────────────────────────────────────────┐
│                          starpod-app (orchestration)                       │
│  AppState (single source of truth) · task supervision · event bus          │
└──────┬─────────────┬───────────────┬──────────────┬─────────────────────┘
       │             │               │              │
┌──────▼──────┐ ┌────▼─────────┐ ┌───▼──────────┐ ┌▼──────────────────┐
│ subscription │ │ config-builder│ │    rules    │ │  engine-supervisor│
│ (providers)  │ │  (the core)  │ │  (model)    │ │  (process + REST) │
└──────┬──────┘ └────┬─────────┘ └───┬──────────┘ └┬──────────────────┘
       │             │               │              │
       │             └───────┬───────┘              │
       │                     │                      │
       │            ┌────────▼────────┐    ┌────────▼─────────┐
       │            │  meow-config   │    │  meow-client     │
       │            │ (YAML emitter) │    │  (REST wrapper)  │
       │            └────────────────┘    └────────┬─────────┘
       │                                            │
       │                                   ┌────────▼─────────┐
       └───────────────────────────────────▶│   meow process   │
                          validation       │  (downloaded     │
                          meow -t -f       │   release binary)│
                                           └──────────────────┘
                    ┌──────────────┐
                    │  starpod-    │  preferences · rules · subscription
                    │  store       │  metadata + cache · selected node
                    └──────────────┘
```

**Crates** — a Cargo workspace, one crate per concern, so the config pipeline is testable
without the GUI or the engine (GOAL.md §3 requires exactly that):

| Crate | Contains | Depends on GUI? | Depends on engine? |
|---|---|---|---|
| `starpod-core` | domain types, rule model, config pipeline, validation | **no** | **no** |
| `starpod-subscription` | provider abstraction, fetchers, parsers, cache | no | no |
| `starpod-store` | persistence, migrations, secret storage | no | no |
| `starpod-engine` | release download/verify, supervisor, REST client, YAML emitter | no | no (talks to a *process*) |
| `starpod-ui` | Slint components + view models | — | — |
| `starpod-app` | binary entry point; wires the above to the UI | yes | yes |

`starpod-core` is the "most important part" per GOAL.md §3 and depends on nothing but `serde` +
a YAML library. It is exercised by its own test suite with zero I/O.

---

## 3. Data model

### 3.1 Persisted state (`starpod-store`, on-disk format: **JSON**, one file per concern)

| File | Contents | Secret? |
|---|---|---|
| `preferences.json` | mode, ports, allow_lan, bind address, log level, language, theme, autostart, tun toggle | no |
| `subscriptions.json` | per-subscription metadata: id, name, url, last-fetch time, expiry, traffic used/total, node count, enabled, group assignment | **yes — the URL** |
| `rules.json` | user-defined rules (the app's own model, not meow's raw format) | no |
| `state.json` | selected proxy/group, selected subscription, UI state | no |
| `cache/<subscription-id>.yaml` | last valid subscription payload | maybe (node passwords) |
| `secrets.json` | controller secret, subscription URLs | **yes** |

**Secret storage — platform-conditional:**

| Platform | Mechanism |
|---|---|
| Windows | **DPAPI** (`windows` crate `CryptProtectData` / `CryptUnprotectData`, `CRYPTPROTECT_UI_FORBIDDEN`) — bound to the user account. |
| macOS | **Keychain** (`security-framework` crate, `SecItemAdd`/`SecItemCopyMatching` with `kSecClassGenericPassword`, service = app id). |
| Linux | **Secret Service / D-Bus** via `secret-service` crate if a keyring is present; **fallback: file with `0600` perms** in the app's own data dir, with the UI stating plainly that the secret is stored unencrypted at rest. |

The app never stores the controller secret in the generated meow YAML; it is passed as `--secret`
on the child's command line and kept in memory only.

**Corruption / migration handling:**
- Every file is read with a schema version field (`"schema": 1`).
- Parse failure → the file is renamed to `<name>.corrupt-<timestamp>` and the app starts with
  defaults, surfacing a warning on the Logs screen. Never silently discard; never crash.
- Unknown/newer `schema` → the file is left untouched and the app runs with defaults.
- Older `schema` → a pure migration function upgrades it (`fn migrate(v: u32) -> Value`).
- The subscription cache is **kept** even if the metadata file is corrupt — the cache is the last
  known-good payload, and it is the app's fallback when the network is down.

### 3.2 Subscription model

```rust
struct SubscriptionMeta {
    id: SubscriptionId,           // stable uuid
    name: String,                  // user-facing label
    url: SecretString,             // credential-bearing, never logged
    /// Optional extra request header, e.g. `Authorization: Bearer …`.
    /// Secret-bearing, so never logged. App-managed only (§3.2.1).
    header: Option<SecretString>,
    mode: SubscriptionMode,        // AppManaged (default) | EngineManaged
    enabled: bool,
    added_at: Timestamp,
    last_success: Option<Timestamp>,
    last_error: Option<String>,    // redacted
    expiry: Option<Timestamp>,     // from subscription-userinfo `expire`
    traffic: Option<TrafficUsage>, // upload/download/total
    node_count: usize,
    group_count: usize,
    cache_path: PathBuf,
    /// The engine's own bookkeeping for engine-managed subscriptions:
    /// the `last_updated` stamp the app watches to detect a refresh (§4.6).
    engine_last_updated: Option<i64>,
}

struct SubscriptionPayload {        // parsed, validated, normalized
    nodes: Vec<NodeSpec>,
    groups: Vec<GroupSpec>,
    rules: Vec<String>,
}
```

**`expiry` and `traffic` are `None` for engine-managed subscriptions** — not zero, absent. The
engine's fetcher discards the `subscription-userinfo` header, so the app has no value to show and
the Servers screen renders "managed by the engine" in place of the quota bar (§3.2.1). A `None`
here must never be rendered as `0`/`0 GB`; that would be a fabricated number.


`NodeSpec` is the app's own normalized node shape — **decoupled from meow's raw YAML**:

```rust
struct NodeSpec {
    id: String,             // app-local stable id
    name: String,           // display name
    protocol: Protocol,     // ss | trojan | vless | vmess | hysteria2 | anytls | snell | http | socks5
    server: String,
    port: u16,
    credentials: SecretString,  // password / uuid / private key — never logged
    params: BTreeMap<String, String>,  // protocol-specific extras (tls, sni, flow, …)
}
```

**Provider abstraction** (GOAL.md §3: design it "without pretending that a real integration has
been completed"):

```rust
#[async_trait]
trait SubscriptionProvider {
    async fn fetch(&self, req: &FetchRequest) -> Result<FetchResponse, ProviderError>;
    fn kind(&self) -> ProviderKind;
}

struct FetchRequest { url: SecretString, timeout: Duration }
struct FetchResponse {
    body: Vec<u8>,
    status: u16,
    headers: HeaderMap,     // for subscription-userinfo
}
```

Implementations:
- `HttpSubscriptionProvider` — plain HTTPS GET, follows redirects (bounded), honours the
  `subscription-userinfo` header, applies size limits and a timeout.
- `LocalFileProvider` — reads a YAML file from disk (for testing and for users with a local
  profile).

**The provider is the seam where a real vendor API would plug in later.** The app never claims
compatibility with any specific panel; it handles the *de-facto* Clash/Mihomo subscription format
that panels emit.

### 3.2.1 Two subscription modes (user decision: both, per subscription)

Each subscription carries a `mode` field:

```rust
enum SubscriptionMode {
    /// The app fetches, parses, validates, caches and merges the payload.
    AppManaged,
    /// The engine owns the fetch and the refresh; the app supervises.
    EngineManaged,
}
```

**The two modes are genuinely different, and the difference is not cosmetic** - it was verified
against the engine source:

| | App-managed | Engine-managed |
|---|---|---|
| Who fetches | the app (`reqwest`) | meow (`internal_http::fetch`) |
| Who parses | the app (format sniffing + validation) | meow (`parse_subscription_yaml`) |
| Payload format | Clash/Mihomo YAML **or** base64-wrapped YAML | **Clash/Mihomo YAML only** - meow's parser requires a `proxies:` key and will not unwrap base64 |
| Quota / expiry | **available** (`subscription-userinfo` header) | **not available** - the engine's fetcher discards the header; `GET /api/subscriptions` returns only `name, url, interval, last_updated, proxy` + contribution counts |
| Auth | URL token **and** optional `Authorization` header | URL only - meow's `AddSubscriptionRequest` has no header field, and its fetcher passes `&[]` headers |
| Cache | app-owned, last-valid payload retained | engine-owned |
| Rule order | user rules first (P5) | engine prepends subscription rules - corrected by the supervision loop in §4.6 |
| Node-name collisions | app namespaces them | engine replaces the local node of the same name, and **does not restore it** when the remote drops it |

**The last row is the sharp edge.** meow's `apply_subscription` warns explicitly:

> "subscription '{name}': remote node '{n}' replaces a local proxy of the same name - the local
> definition is not restored when the remote drops it"

So in engine-managed mode a subscription *can* silently destroy a user-defined node. The app
mitigates this by **namespacing every subscription-derived node name** before the engine ever
sees it (`<subscription label> / <node name>`), which makes a collision with a user node
impossible rather than merely unlikely.

**Why the mode exists at all:** engine-managed mode means the *engine* keeps the nodes fresh even
while the app is closed - the tray keeps the app alive, but a crash or a reboot would leave the
engine's subscription stale until the app returns. It also offloads the fetch to the engine's
proxy-aware downloader, which can reach a subscription host that is itself only reachable through
a node (`subscription.proxy:` - a dialer for the download).

**The Servers screen must show the mode**, and must show quota/expiry **only** for app-managed
subscriptions (an engine-managed row shows "managed by the engine" instead of a quota bar). This
is honest UI: it does not display a number the app does not actually have.

**Switching modes** re-registers the subscription with the other subsystem and moves the
subscription's nodes across - app-managed to engine-managed hands the URL to
`POST /api/subscriptions`; engine-managed to app-managed removes it via
`DELETE /api/subscriptions/{name}` and takes over fetching. Both directions run through the full
validate-then-apply path (§4), so a failed switch leaves the previous state running.

### 3.2.2 Subscription body parsing pipeline (app-managed mode)


```
bytes → sniff format → YAML parse → shape check → per-entry validation → SubscriptionPayload
```

- **Sniff:** if the body parses as YAML with a `proxies:` or `proxy-groups:` key → YAML path.
  Otherwise if it base64-decodes to something that then parses as YAML → base64-wrapped YAML
  (a very common panel behaviour). Otherwise → reject with a clear error.
- **Validation** (untrusted input, GOAL.md §6): every node is checked for a known protocol, a
  non-empty server, a port in 1..=65535, a name that does not collide with another node, and
  required credential fields present. Invalid nodes are **skipped and counted**, not fatal — but
  if *zero* nodes survive, the whole subscription is rejected and the previous cache is kept.
- Node **names are namespaced** before being emitted into meow's YAML, to prevent a subscription
  from hijacking a user-defined group or rule target.

### 3.3 Rule model (the app's own, translated to meow)

The user-facing model is **not** meow's raw rule string. It is structured:

```rust
enum RuleKind {
    Domain, DomainSuffix, DomainKeyword, DomainRegex, DomainWildcard,
    GeoSite, GeoIp, SrcGeoIp,
    IpCidr, SrcIpCidr, IpSuffix, SrcIpSuffix, IpAsn, SrcIpAsn,
    SrcPort, DstPort, InPort, Network, Dscp,
    ProcessName, ProcessPath, Uid,
    InName, InType, InUser,
    RuleSet, SubRule,
    And, Or, Not,
    Match,
}

struct RuleSpec {
    id: RuleId,
    kind: RuleKind,
    payload: String,       // domain, CIDR, category, provider name, …
    target: RuleTarget,    // see below
    flags: RuleFlags { no_resolve: bool, is_src: bool },
    enabled: bool,
    order: u32,            // explicit ordering; first match wins
}

enum RuleTarget {
    Direct, Reject, RejectDrop, Compatible, Pass,
    Node(String),      // by node id → resolved to name at emit time
    Group(String),     // by group id → resolved to name at emit time
}
```

**Translation layer** (`starpod-core/src/rules/translate.rs`): `RuleSpec` → meow rule string.
- `RuleKind` → the exact token from §1.5. The mapping table is **derived from meow's
  `RuleType::as_str`**, so an unsupported kind cannot be constructed.
- `RuleTarget::Direct` → `"DIRECT"`, etc.; `Node`/`Group` → the resolved display name.
- Flags → `,no-resolve` / `,src`, appended only when set, and `src` implies `no-resolve`.
- `And`/`Or`/`Not` → `AND,((R1),(R2)),TARGET` with nested parenthesisation.

**Round-trip guarantee:** the translator emits only strings meow's parser accepts, and the app
**validates every emitted rule by parsing it with meow's own grammar** — but meow is a subprocess,
so the app validates by running `meow.exe -t -f <candidate.yaml>` (§4.4). At the *unit* level the
app has its own mirror of the token table and asserts the translation is total.

### 3.3.1 Two rule editors (user decision: structured + raw text)

The Rules screen offers two editors over the same underlying `RuleSpec` list.

**Structured editor** (the default): dropdowns for `RuleKind` and `RuleTarget`, a validated text
field for the payload, and checkboxes for the flags. The target dropdown lists only names that
actually resolve — user groups, then subscription node groups, then the built-in outbounds
(`DIRECT`, `REJECT`, `REJECT-DROP`, `COMPATIBLE`, `PASS`). Payload validation is kind-specific: a
CIDR field rejects a non-CIDR, a port field rejects a non-port, a domain field rejects a wildcard.

**Raw-text mode** (the "advanced" toggle): a line-oriented editor where each line is a complete
meow rule string. Lines are parsed by the app's own parser — **the same grammar table the
translator uses, mirrored from meow's `RuleType::as_str`** — and each parsed line is converted
*into* a `RuleSpec` and then back out through the translator. Therefore:

- A raw line is never stored as an opaque string; it is stored as a `RuleSpec`, so it round-trips
  through the same pipeline, the same validation, and the same YAML emission as a structured rule.
- A raw line that does not parse is shown inline as an error on that line, with the position. The
  invalid line is **not** silently dropped and **not** silently kept — the editor refuses to commit
  while an invalid line exists.
- Because raw mode parses into `RuleSpec` first, it cannot introduce a target the structured
  editor would forbid (e.g. a target that does not resolve) — the same validation runs.
- `MATCH` is reserved: the app owns the terminal `MATCH` rule and rejects raw lines that try to
  place one mid-list.

**Two-way sync:** switching from raw mode back to structured mode parses the raw lines into
`RuleSpec`s, so nothing is lost. The structured editor cannot represent every raw line the
grammar allows (the logic forms `AND`/`OR`/`NOT` take a nested sub-rule tree); those appear in
the structured editor as a `Logic` kind with a nested sub-editor, so the mapping is total in both
directions.

**The grammar mirror is a maintenance risk** and is treated as such: the token table lives in one
file (`starpod-core/src/rules/grammar.rs`) with a comment pointing at meow's `rule.rs`, and a test
asserts every token the app can emit is one meow's parser accepts — verified end-to-end by
`meow -t` on every candidate config.


### 3.4 The runtime configuration pipeline — the heart of the app

GOAL.md §3: `Subscription + User Preferences → Normalization → Merging → Validation → Runtime Configuration → Proxy Core`.

```
        ┌──────────────────┐        ┌──────────────────┐
        │ Subscription(s)  │        │ User preferences │
        │  (nodes/groups/  │        │  (ports, mode,   │
        │   rules)         │        │   tun, groups)   │
        └────────┬─────────┘        └────────┬─────────┘
                 │                           │
                 └─────────────┬─────────────┘
                               │
                    ① NORMALIZATION
                    - rename nodes to unique, stable names
                    - drop invalid entries (counted)
                    - resolve rule targets to names
                               │
                    ② MERGING  ← deterministic precedence
                               │
                    ③ VALIDATION
                    - structural checks (app-side)
                    - meow -t -f candidate.yaml  (engine-side)
                               │
                    ④ RUNTIME CONFIG (YAML)
                               │
                    ⑤ PROXY CORE
```

#### ① Normalization

Input: the raw persisted state (subscriptions + preferences + rules). Output: a
`NormalizedConfig` — every node uniquely named, every group referencing existing members, every
rule target resolvable.

- Node names are made unique by suffixing ` (2)`, ` (3)`, … Deterministic, so re-running
  normalization on the same input yields the same names (essential for stable diffs and for
  preserving a user's group selection across a subscription refresh).
- A node's name is namespaced with its subscription label so two subscriptions cannot collide
  visually or semantically.

**Which subscriptions contribute to the app's own pipeline:**

- **App-managed subscriptions** contribute their parsed payload directly — the app fetches,
  parses, normalizes and merges them. They are the input to ①–③.
- **Engine-managed subscriptions** do **not** contribute to the app's pipeline. The engine fetches
  them itself and injects the nodes into the running config directly, bypassing ①–③ entirely.
  The app's pipeline therefore sees only the app-managed subscriptions and the user's own
  configuration.

This is why the two modes need the correction in §4.6: an engine-managed subscription's rules and
nodes arrive at the engine without passing through the app's merge, so the app must re-assert the
order afterwards.

**The YAML the app writes therefore contains two kinds of content:**

| Content | Written by | In the app's YAML? |
|---|---|---|
| User preferences, groups, rules | the app | **yes** |
| App-managed subscription nodes/groups/rules | the app (merged) | **yes** |
| Engine-managed subscription *declarations* | the app (a `subscriptions:` entry) | **yes** — as a declaration, so the engine knows to fetch it |
| Engine-managed subscription *nodes/rules* | the engine | **no** — injected by the engine at fetch time |

The `subscriptions:` declaration for an engine-managed entry is:

```yaml
subscriptions:
  - name: my-provider
    url: https://example.invalid/sub?token=…
    interval: 3600
    proxy: DIRECT        # optional: the dialer used for the download
```

**The URL is in the app's YAML file** — and that file is user-readable. This is a secret-handling
consequence that must be handled: the app writes the generated config with the URL **redacted or
omitted** where the engine does not need it, and where it does need it, the file's permissions are
restricted (`0600` on unix, an ACL on Windows). See §5 for the redaction rules.

#### ② Merging — the precedence rules (this is the contract)

**Rule: user-defined configuration always wins over subscription-derived configuration, and a
subscription refresh never overwrites a user-defined value.** Enforced in three named rules:

| # | Field | Precedence | Notes |
|---|---|---|---|
| P1 | Ports (`mixed_port`, `socks_port`, `port`), `allow_lan`, `bind_address` | **user > subscription > default** | A subscription that ships `mixed-port: 7890` never changes a port the user set. |
| P2 | `mode`, `log_level`, `tun`, `dns` | **user > subscription > default** | Same rule. |
| P3 | `proxies` (nodes) | **union** — subscription nodes + user-pinned nodes; **user wins on name collision** | A user-added node is never dropped by a refresh. |
| P4 | `proxy_groups` | **user > subscription, per group name** | A user-defined group of the same name replaces the subscription's; a subscription group whose name matches a user group is dropped. |
| P5 | `rules` | **user rules first (by `order`), then subscription rules, then a mandatory `MATCH` tail** | User rules take precedence because first-match-wins. The `MATCH` rule is always last and always present — the app appends it if absent, and it is not user-editable. |
| P6 | `selected node` | **user > subscription default** | Preserved across refreshes by node **id**, not name — so a rename in the subscription does not lose the selection. |

**The one exception, and it is explicit:** P3 and P5 govern the content the app merges. An
**engine-managed** subscription bypasses the merge entirely (its nodes and rules arrive at the
engine directly), so for that subscription's content the engine's own rules apply — the engine
prepends the subscription's rules and replaces same-named local nodes. The app corrects the rule
order after each engine refresh (§4.6) and detects node shadowing (§4.6, R9/R12). **The app
does not pretend the merge covers engine-managed content** — the Servers screen labels those rows
so the user knows which precedence applies to them.

**The merge is a pure function:** `merge(subscriptions, preferences, rules) -> NormalizedConfig`.
No I/O, no clock, no randomness → trivially unit-testable, exactly as GOAL.md §3 demands
("must be independently testable, not coupled to GUI or engine").

**"Never silently overwrite user settings"** is enforced structurally: user preferences are stored
in a separate file from subscription-derived data, and the merge reads them separately. There is no
code path where a subscription write can touch a preference file.

#### ③ Validation — two layers

**Layer A (app-side, fast, in-process):**
- Every rule's target resolves to an existing node or group (or is a built-in outbound).
- No duplicate node/group names.
- No duplicate rule `order` values (ties broken by rule id, deterministically).
- Port values in range; `mixed_port`/`socks_port`/`port` not colliding.
- Every group has at least one member and no self-reference; no cycles.
- `RULE-SET` references an existing rule provider.
- **Geodata awareness:** if the config contains `GEOIP`/`IP-ASN` rules, the app warns that the
  engine will download a database on first use — a *warning*, not an error, because the engine
  resolves it (§1.6).

**Layer B (engine-side, authoritative):**
Write the candidate YAML to a temp file, run `meow.exe -t -f <candidate.yaml>`, and require exit
code 0. This is the real check: it uses meow's own parser, so a rule we thought was valid but
meow rejects is caught **before** the running config is touched.

**Both layers must pass before anything is applied.** A failure at either layer leaves the running
configuration untouched and returns a structured error to the UI.

#### ④ Runtime configuration → ⑤ proxy core

The validated YAML is written **atomically** (write to `<name>.tmp`, fsync, rename) to the app's
runtime config path, then applied:

- **First start:** spawn `meow` with `-f <path> --ext-ctl 127.0.0.1:<port> --secret <secret>`.
  If TUN is enabled, the spawn goes through the elevated helper instead (§4.5).
- **Config change:** `PUT /configs {"path": "<path>"}` — a hot reload. On success the engine closes
  all connections; the app shows that on the Dashboard.
- **Restart:** stop the child (graceful, §1.7), then start it again.
- **TUN toggle:** `PATCH /configs {"tun": {...}}` - live, no restart (§4.5).

**Failure isolation:** the app always keeps the *previous* validated YAML and the previous
subscription cache. If a refresh produces an invalid config, the app keeps running the old one
and reports the error — it never tears down a working proxy because a remote subscription went
bad. This is GOAL.md §3's "safe recovery from failed updates".

---

## 4. The proxy core abstraction

GOAL.md §2 wants an abstraction around the core "so GUI/business logic don't depend on its
internals", while also warning "avoid unnecessary abstractions". The resolution: **one trait with
seven methods**, implemented by a `MeowEngine` that talks to a subprocess.

```rust
#[async_trait]
pub trait ProxyEngine: Send + Sync {
    async fn start(&self, cfg: &RuntimeConfig) -> Result<EngineHandle, EngineError>;
    async fn stop(&self) -> Result<(), EngineError>;
    async fn reload(&self, cfg: &RuntimeConfig) -> Result<(), EngineError>;  // restart/reload on config change
    async fn status(&self) -> Result<EngineStatus, EngineError>;           // operational state
    async fn validate(&self, cfg: &RuntimeConfig) -> Result<ValidationReport, EngineError>; // validate before applying
    async fn set_tun(&self, tun: &TunSettings) -> Result<(), EngineError>; // TUN on/off (v1)
    fn logs(&self) -> LogStream;                                          // diagnostics
}
```

`EngineStatus` = `{ phase: Stopped|Starting|Running|Stopping|Failed, mode, uptime,
version, last_error, controller_ready: bool, elevated: bool }`.

**`validate` returns a report, not a bare `Result`.** The engine-side check (`meow -t`) is
authoritative, but it is not the whole story: the app-side layer (§3.4 ③) also produces findings,
including *warnings* that must not block the apply — notably "this config uses `GEOIP`/`IP-ASN`
rules, so the engine will download a database on first use" (§1.6). A bare `Result<(), E>` would
force the app to either lose those warnings or treat them as failures.

```rust
struct ValidationReport {
    ok: bool,                  // true only if the engine accepted it
    engine_error: Option<String>,   // the engine's own message, verbatim
    warnings: Vec<String>,     // non-blocking findings from the app-side layer
}
```

**`set_tun` exists because TUN is in v1** and is a *runtime* operation on the engine
(`PATCH /configs {"tun": {...}}`), not a config-file edit — the app toggles it live and warns
that it closes existing connections (§4.5). The method is on the trait because the GUI must not
know that TUN happens to be a `PATCH` rather than a restart.

**The trait is the *only* thing the GUI and the business logic see.** `starpod-ui` and the
orchestration layer never import a meow type, never name a REST route, and never know whether the
engine is a subprocess or something else. That is the property GOAL.md §2 asks for, and it is
enforced by the crate boundary: `starpod-engine` is the only crate that imports `reqwest`,
`tokio::process`, or knows the word "meow".

### 4.1 Release download & verification

```
1. Resolve asset name from (version, target triple)  → §1.1 table
2. GET https://github.com/meow-rs/meow-rs/releases/download/v{ver}/{asset}
3. GET .../{asset}.sha256  → parse field 0 as hex digest
4. Stream the asset to a temp file, computing sha256 as it goes
5. Compare; mismatch → delete temp, fail closed
6. Extract (zip on Windows, tar.gz on Unix) into a versioned dir under the app's data dir
7. Locate the binary inside the single top-level directory (§1.1)
8. chmod +x on Unix
```

- **Never** download over an existing install without verifying first.
- The install dir is `<data_dir>/engine/meow-v{version}-{triple}/`; a version bump is a new dir,
  and the old one is removed after a successful swap.
- The sha256 of the installed binary is recorded in a local manifest so the app can detect
  tampering after install (not a substitute for a signature — see §1.1).

### 4.2 Supervision

**Two supervision topologies, one interface.** The `ProxyEngine` trait (§4) hides the difference:

| Topology | When | How |
|---|---|---|
| **Direct** | TUN off (the default) | The app spawns meow itself with `tokio::process::Command`. It holds the handle, so it can poll the exit status and send signals directly. |
| **Elevated helper** | TUN on | The app spawns a *helper* (the same binary with `--elevated-supervisor`) elevated; the helper spawns meow and owns the process group. The app talks to the helper over the IPC directory (§4.5). |

Both present the same `EngineHandle` to the rest of the app: a liveness signal, a stop request, an
exit code, and the log stream. The direct topology is the common case and is what the tests
exercise most heavily.

**Direct topology details:**

- Spawn via `tokio::process::Command`, with `--ext-ctl 127.0.0.1:{port}` and `--secret {secret}`.
  The port is chosen by binding `:0`, releasing, and using the number — with a bind-retry loop, and
  a fallback to letting meow pick the port and reading its controller file if the race is lost.
- **Windows:** `creation_flags(CREATE_NEW_PROCESS_GROUP)` so `CTRL_BREAK_EVENT` can be targeted.
- **Stdout/stderr are piped** and drained into the app's log ring buffer. The engine's own log
  level is set from user preference.
- **Health:** the app polls `GET /version` (or `/configs`) until it answers, with a startup
  timeout. Until then the UI shows `Starting`.
- **Crash:** the child's exit is observed; the app marks the engine `Failed`, surfaces the exit
  code and the tail of the engine log, and offers restart.
- **Env:** `MEOW_RUNTIME_DIR` is set to the app's own runtime dir so the engine's controller file
  doesn't collide with other meow instances.

**The secret necessarily reaches the engine on the command line** — `--secret` is the only way to
set it, and there is no config-file or environment equivalent. On unix that makes it visible in the
process table to other users of the same machine. The app's mitigation is the runtime dir's
permissions: the controller binds to **loopback only** (`--ext-ctl 127.0.0.1:{port}`), so the
secret is only useful to a process on the same host that can already read the process table or the
app's `0700` runtime dir. This is a limitation of the engine's interface, not something the app
can fix, and it is recorded here rather than papered over.

### 4.3 The REST client

- `reqwest` with `bearer_auth(secret)` on every request; **the secret is never logged** — the
  client's request logger redacts the `Authorization` header, and errors are mapped to
  user-facing messages that never embed the secret or the full subscription URL.
- Timeouts on every request (short for status polls, longer for reload).
- A typed error enum: `Unauthorized`, `NotFound`, `BadRequest(String)`, `EngineDown`, `Timeout`,
  `Transport(String)` — each with a user-facing message.

### 4.4 Validation via `meow -t`

`meow.exe -t -f <candidate.yaml>` exits 0 on success and non-zero with a message on failure.
This is the app's **authoritative** pre-apply validation and it uses the engine's own parser, so
there is no second grammar to keep in sync. The app surfaces the engine's stderr to the Logs
screen and a summarised form in the UI.

**Cost:** one process spawn per validation. For an interactive edit that is noticeable, so the app
validates on a debounce (~400 ms) rather than on every keystroke, and the UI shows an inline
"checking…" state on the rule editor.

### 4.5 TUN mode (user decision: in v1, with elevation)

TUN is the whole-device transparent proxy. It is **in v1**, but it changes *who* runs elevated —
a constraint that is easy to get wrong.

**The elevation requirement is on the meow process, not on the GUI.** Verified from meow's own
documentation (`meow-listener-0.29.0/src/tun/mod.rs:64-65`):

> "On Windows the device is a Wintun adapter. `wintun.dll` is resolved next to the executable
> (official Windows zips ship it there), then the working directory; if neither exists the
> official signed DLL embedded in the binary is written out. **The process must run elevated.**
> On Linux/macOS creating the device requires root (CAP_NET_ADMIN)."

So there are two workable shapes, and the app supports both:

| Shape | How | Trade-off |
|---|---|---|
| **A. GUI stays unelevated; the child is relaunched elevated** | The app spawns meow normally. When TUN is enabled, it re-spawns the child elevated via the platform's elevation mechanism: `ShellExecuteW` with the `runas` verb on Windows, `osascript`/`sudo` on macOS and Linux. | The child is elevated, so **the controller secret and the config path must cross the elevation boundary** — the elevated child inherits them via the command line and environment, never via a shared file the user could edit. |
| **B. The whole app relaunches itself elevated** | On enabling TUN, the app re-executes itself with an elevation flag, and the elevated instance owns the engine. | Simpler privilege story, but the *whole* GUI runs elevated, including the browser-ish parts — a worse security posture, and it conflicts with the tray design. |

**The app uses shape A.** The GUI stays unprivileged; only the proxy process is elevated.

**Shape A has a Windows subtlety that changes the supervisor design.** `ShellExecuteExW` with the
`runas` verb returns **no process handle** — the elevated process is started by the shell, and the
caller cannot wait on it, poll it, or signal it directly. That breaks the supervisor model
(crash detection, graceful stop) if it is done naively. The app therefore uses a **supervisor
helper** on Windows:

```
unelevated app
   │  writes <ipc-dir>/child.json  { meow_argv, config_path, secret, ctl_port }   (ACL: user only)
   │  ShellExecuteExW(runas) → this same binary with --elevated-supervisor
   ▼
elevated helper (same exe, --elevated-supervisor)
   │  spawns meow with CREATE_NEW_PROCESS_GROUP, inheriting the console
   │  writes <ipc-dir>/child.pid  { helper_pid, meow_pid, group_id }
   │  waits for the stop request, then sends CTRL_BREAK_EVENT to meow's group
   │  writes <ipc-dir>/child.exit { code }
   ▼
meow.exe
```

This works because **the elevated helper owns the console** and created the process group, so
*it* is the process that can call `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group_id)` — the
unelevated parent could not, because it is not attached to that console. The parent still gets
everything it needs (liveness, exit code, and a stop request) through the IPC directory.

**On Linux and macOS the same helper pattern is used** (`sudo`/`osascript` → the helper), so the
supervision model is uniform across platforms and the graceful-stop path is identical in shape
everywhere: *the helper that spawned meow is the one that signals it.*

**The IPC directory is the security boundary.** It contains the controller secret and the config
path, so it must be:
- inside the app's own runtime dir, created with `0700` on unix and a user-only ACL on Windows;
- written atomically, with the secret in the file (never on the command line, which is visible in
  the process table on unix);
- deleted as soon as the child is confirmed started.

**The `--elevated-supervisor` mode is a separate, minimal code path** — it must do nothing except
spawn the child, service the stop request, and report the exit status. It deliberately contains
no UI, no network code, and no subscription handling, so the elevated surface is as small as
possible.

**`wintun.dll` must sit next to `meow.exe`.** The engine resolves the DLL in this order: next to
the executable, then the working directory, then it writes out the signed copy embedded in the
binary (`wintun.rs:32-34` — `include_bytes!` of the official signed DLL). The DLL search path and
`PATH` are deliberately **not** consulted, to avoid a LoadLibrary hijack. **Consequence: the app's
extractor must preserve the archive layout** — `wintun.dll` and `meow.exe` in the same directory.
Since the release archive ships both side by side, extracting the whole top-level directory
verbatim satisfies this; the app must not flatten or cherry-pick files.

**TUN configuration** (`RawTun`, `raw.rs:403-443`): `enable`, `device`, `mtu`, `auto_route`,
`outbound_interface`, `dns_hijack`, `udp_timeout`, `stack`, `strict_route`,
`auto_detect_interface`, `auto_redirect`, `endpoint_independent_nat`, `route_address`,
`route_exclude_address`. The REST `PATCH /configs` accepts `tun` with the mihomo-compatible keys
`enable, device, mtu, inet4_address, inet6_address, auto_route, outbound_interface, dns_hijack,
udp_timeout`; unknown mihomo keys like `stack`/`strict-route` are ignored by that endpoint.

**TUN enable/disable is a runtime operation** — `PATCH /configs {"tun": {...}}` — and the engine
can also toggle it via `meow-cli tun on|off`. Toggling TUN changes the device's routing, which
**closes existing connections**; the app warns before enabling it.

**Failure handling.** If TUN cannot be enabled (no elevation granted, driver install refused,
device busy), the app must:
1. Leave the mixed-port proxy running — TUN failure must not take down the working proxy.
2. Report the engine's error text verbatim (the engine knows the real reason: "the process must
   run elevated", driver errors, etc.).
3. Offer "retry elevated" — re-spawn the child elevated.

**Shutdown.** An elevated child must still be stopped by the graceful path (§1.7). `CTRL_BREAK_EVENT`
and `SIGTERM` work regardless of the child's privilege level, because the parent holds the handle
and the signal is delivered by the OS, not by the child's token.

### 4.6 Re-asserting rule order after an engine refresh (user decision)

This is the mechanism that makes engine-managed subscriptions coexist with our precedence rules.

**The problem.** meow's `apply_subscription` (`meow-config-0.29.0/src/subscription.rs:62-181`)
does two things that invert our rules:

1. **Rules:** it *prepends* the subscription's rules ahead of the local ones:
   ```rust
   let mut merged = sub.applied_rules.clone();
   merged.extend(rules);          // ← subscription rules first
   raw.rules = Some(merged);
   ```
   So after an engine refresh, subscription rules sit **above** user rules. Our P5 says the
   opposite.

2. **Nodes:** a remote node whose name matches a local one **replaces** it, and when the remote
   later drops that name, the local definition is *not* restored — meow warns about exactly this.

**The fix.** After detecting an engine refresh, the app re-reads the live rules and re-applies the
full merged rule list with user rules first, via `POST /rules`:

```
POST /rules {"rules": [ ...user rules..., ...subscription rules..., "MATCH,<target>" ]}
```

**Why this is safe.** `replace_rules` (`routes.rs:2540-2555`) sets `raw.rules` and then calls
`meow_config::subscription::reconcile_contribution_claims(&mut raw)`. That function clamps each
subscription's `applied_rules` set to the rules still physically present, counting duplicates
correctly. So re-ordering the rules does **not** orphan the engine's contribution bookkeeping — the
sets are re-derived from what is actually in the config. The same function clamps `applied_proxies`
and `applied_groups` to the nodes present. This is the mechanism that makes the correction
consistent rather than a hack.

**How the app detects a refresh.** The engine's refresh loop (`meow-app-0.29.0/src/subscription_refresh.rs`)
refreshes a subscription when `now - last_updated >= interval`, and stamps `last_updated`. The app
polls `GET /api/subscriptions` and watches each engine-managed subscription's `last_updated`; when
one changes, the app knows a refresh happened.

**What the app does on detection:**
1. Re-read `GET /rules` to learn the engine's current rule list (which includes the subscription's
   freshly-applied rules).
2. Rebuild the desired order: the app's own user rules first, then the rules it did not author
   (i.e. the subscription's), then the terminal `MATCH`.
3. `POST /rules` with that order.
4. Verify with `GET /rules`; if the order did not take, log it and surface it on the Logs screen.

**Frequency.** The poll is cheap (`GET /api/subscriptions` is a small JSON list) but must not be
frequent. A 30-second poll is appropriate; the engine's own default refresh interval is measured
in hours, so a 30 s poll cannot meaningfully miss one. The poll is skipped entirely while the
engine is not `Running`.

**Node-name collisions in engine-managed mode** cannot be prevented by namespacing, because the
engine does the fetch and the app never sees the raw payload. The mitigation is therefore
detection rather than prevention, and it must be stated precisely:

- The app **compares** the engine's live proxy list against the app's known user-defined node
  names; if an engine-managed subscription's node shadows a user node, the app **warns on the
  Servers screen** and offers to convert that subscription to app-managed mode (where namespacing
  is possible).
- The app **snapshots** the user's node names before the engine starts and after each refresh, so
  a vanished user node is detected and reported rather than silently lost.

This is the honest limit of engine-managed mode, and it is why app-managed is the default.


---

## 5. Persistence

See §3.1 for the file layout. Design decisions:

- **Format: JSON** for state, **YAML** for the generated meow config and the subscription cache.
  JSON because the state is small, human-inspectable, and `serde_json` is already in the tree.
- **Atomic writes everywhere:** write temp → fsync → rename. A crash mid-write can never leave a
  half-written state file. On Windows, `rename` over an existing file needs care — write to
  `foo.tmp`, then `RemoveFile`+`MoveFile` via the `windows` crate, or use `fs::rename` with
  `ReplaceFileW`.
- **Secrets are separate from state.** `secrets.json` (or the OS keyring) holds the controller
  secret and subscription URLs; `subscriptions.json` holds only a **reference** to the keyring
  entry plus non-secret metadata. If keyring is unavailable, the fallback file is `0600` on unix
  and ACL-restricted on Windows, and the Settings screen states plainly that it is not encrypted.
- **No sensitive data in logs.** A redaction layer wraps all logging: URLs are printed as
  `https://host/path?token=***`, node credentials never appear, `Authorization` headers are
  stripped, and the meow engine's own stdout is passed through a filter that masks the secret.

### 5.1 The generated config file is a secret-adjacent artifact

The generated YAML that the app writes for meow contains, unavoidably:

- **node credentials** (passwords, UUIDs, private keys) for every app-managed subscription node;
- **subscription URLs** for every engine-managed subscription (the engine needs them).

So the generated file is treated as sensitive material even though it is "just a config file":

| Platform | Protection |
|---|---|
| Linux / macOS | Written with mode `0600` in the app's runtime dir (`0700`). The directory is created with `0700` on first use, and the app verifies the mode on every write. |
| Windows | Written into the app's runtime dir under `%LOCALAPPDATA%`, with an ACL restricting it to the current user. The `windows` crate can set an explicit DACL; if that is not done, the file inherits `%LOCALAPPDATA%`'s per-user ACL, which is already user-scoped. |

**Redaction in the UI.** The Settings screen shows the *path* of the generated config and a
redacted preview, never the raw file. A "reveal" action opens it in the OS editor, which is an
explicit user action rather than something the app does on its own.

**The `secret` key is never written to the YAML at all** — the controller secret is passed on the
child's command line (`--secret`), so the generated file does not carry it.

**Redaction is a filter, not a formatting convention.** A single `redact()` helper is applied at
the logging boundary and to anything displayed, so a new call site cannot accidentally leak a
token: it takes a `SecretString` and returns only the host and a masked path. Node credentials
are stored in `SecretString` for the same reason — the type makes the leak path harder to write
by accident.

---

## 6. UI

### 6.1 Design system

A hand-written Slint component library mirroring the foxgirls tokens 1:1:

```
ui/theme.slint      — all colour/typography/spacing tokens as global singletons
ui/primitives.slint — AppButton, AppInput, AppSelect, AppSwitch, AppBadge,
                      AppCard, AppSeparator, AppIcon, AppStatusDot, AppSpinner
ui/icons/           — ~20 Lucide-derived glyphs as vector Path geometry
ui/layout.slint     — AppShell (sidebar + content), SectionHeader, EmptyState,
                      LoadingState, ErrorState
```

- Buttons: variants `default` (brand red), `outline`, `secondary`, `ghost`, `destructive`,
  `link`; sizes `xs` (24px), `sm` (28px), `default` (32px), `lg` (36px), `icon` (32×32).
  Focus = 3px ring at 50% brand red; press = 1px translate-y; disabled = 50% opacity.
- Cards: `#1c1c1c`, 14px radius, 1px ring at 10% foreground, 16px padding (12px for `size=sm`).
- Status dot: running = brand red filled; stopped = hollow `#a3a3a3`; error = `#e5484d`;
  syncing = `#a3a3a3` with a reduced-opacity pulse that respects reduced-motion.
- Type: Inter Variable (bundled as a `FontResource`), fallback `Segoe UI, system-ui`.
  Body `text-sm` (14px), labels `text-xs` (12px), headings up to `text-2xl`.
- Spacing unit 4px; radii 6/8/10/14/16px.
- **Icons:** Slint has no runtime SVG renderer, so Lucide glyphs are converted at build time into
  Slint `Path` geometry (or `svg` image resources). ~20 icons suffice:
  dashboard, servers, rules, settings, logs, refresh, power, chevrons, check, x, alert,
  globe, search, plus, trash, edit, lock.
- **The logo:** the foxgirls fox-head mark is a pure vector (two SVG paths, single colour). It is
  recreated as Slint `Path`/`PathLayout` geometry, rendered in `#de6161`, so it recolours with the
  theme.

### 6.2 The five screens

| Screen | Content |
|---|---|
| **Dashboard** | Giant status readout (the site's hero idiom: a large status *word*, not a label/value pair) — `CONNECTED` / `STOPPED` / `STARTING` / `FAILED` in brand red or muted. Below: a mode segmented control (Rule / Global / Direct), live traffic chart (from `GET /traffic`), up/down counters, current node, latency, open-connection count, and quick actions (Connect / Disconnect / Restart). Cards stagger in on first paint, respecting reduced-motion. |
| **Servers** | Subscription list (name, status dot, **mode badge** — "app-managed" / "engine-managed", expiry badge and traffic-used bar **for app-managed rows only**, last fetch, error). Add/remove/edit subscription (URL field is a password-type input; an optional `Authorization` header field, also password-type). A per-subscription toggle switches management mode. Node list grouped by the user's groups, with a search box, per-node latency test, and a select action. Group selection is a `select`-group picker (persisted as a node **id**, so it survives renames). |
| **Rules** | Ordered, drag-reorderable rule list. Two editors over the same model: a **structured editor** (kind / payload / target / flags) and a **raw-text advanced mode** where each line is a full meow rule, parsed by the app's grammar and stored as a `RuleSpec` (§3.3.1). Invalid raw lines are shown inline and block the commit. Live preview of the effective first-match order. |
| **Settings** | Ports (mixed/socks/http), allow-LAN, bind address, log level, **TUN section** (enable, device name, MTU, DNS hijack — with an elevation prompt on first enable, §4.5), autostart, language, theme (dark only, per the reference), engine version + "check for updates", data-directory path, and a clear "clear cached data" action. |
| **Logs** | A live, filterable stream: app logs (redacted) + engine logs (piped stdout/stderr) + the engine's own `GET /logbook` config events (reload/change — exactly what `meow-cli logbook --kind config` shows). Levels filterable; copy/export. |

**Cross-cutting UI requirements (GOAL.md §5):**
- Every screen has explicit **loading / success / failure / empty** states — no blank panels.
- **No dead controls.** Every button is wired; if a control is unavailable in the current state it
  is visibly disabled with a reason, not silently inert.
- **The UI never blocks.** All network/disk/engine work runs on the tokio runtime; results are
  pushed back via `slint::invoke_from_event_loop`. The Slint event loop stays responsive during a
  subscription fetch or an engine restart.
- **Resizable.** A fixed-width left sidebar (the site's navbar idiom, adapted to a desktop app)
  with a content area that reflows; the window has a sensible minimum size, and the layout is
  usable down to ~900×600.

### 6.3 Business logic outside `.slint`

`.slint` files declare properties and callbacks only. Every handler is a Rust function in
`starpod-app` that mutates `AppState` and then pushes the new state to the UI. There is no logic
in a `.slint` file beyond trivial display expressions.

### 6.4 Application lifecycle — system tray (user decision)

**Closing the window hides the app to the system tray; the proxy keeps running.** There are two
distinct exits, and both must stop the child gracefully (§1.7).

| Action | Window | Engine | Tray |
|---|---|---|---|
| Close button / `Esc` | hidden | **keeps running** | shows the running state |
| Hide from tray menu | hidden | keeps running | — |
| **Quit** from the tray menu | closed | **stopped gracefully** | removed |
| App crash / kill | closed | **orphaned — must be reaped** | — |

**The orphaned-engine case is the one that needs care.** If the app is killed (crash, `kill -9`,
power loss), the meow child survives as an orphan. The next app start would then find a meow
process it does not own. Two mitigations, both implemented:

1. **Ownership marker.** The app writes its PID into `MEOW_RUNTIME_DIR` (its own runtime dir) and
   passes `--ext-ctl`/`--secret` from a per-app secret. On start, the app probes the controller
   address it would use; if something answers with the app's secret, the app knows a previous
   instance's engine is still alive and either **adopts** it (re-reading its status) or **stops**
   it before starting a new one. Adopting is the better default: it keeps the proxy up across an
   app restart, which is what a tray-resident proxy client should do.
2. **Crash handler.** A panic hook that stops the child before the process exits, so the common
   case (a Rust panic) does not orphan anything.

**Tray implementation.** The `tray-icon` crate, with a foxgirls-styled icon in brand red
(`#de6161`) that visually distinguishes "running" (filled) from "stopped" (hollow/muted) — the
same status-dot idiom as the UI (§6.1). The tray menu carries: Show/Hide, Connect/Disconnect,
the current mode, the current node, and Quit.

**The tray is not available on every environment** — a Linux desktop without a StatusNotifier host
has no tray. The app must degrade gracefully: if the tray cannot be created, closing the window
**stops the engine and exits** (the old behaviour), and the app says so once, at startup. This is
a real cross-platform hazard, not a hypothetical one.

**Autostart at login** is *not* in v1 (it was not among the user's decisions), but the Settings
screen reserves a row for it and the tray makes it the natural next step.

---

## 7. Dependencies (verified to exist and to be maintained)

| Crate | Purpose | Note |
|---|---|---|
| `slint` 1.18.1 (+ `slint-build`) | UI | `slint-build` must be a **build-dependency**; the template needs `slint::include_modules!()` |
| `tokio` | async runtime | `#[tokio::main]` discouraged; use `block_in_place(slint::run_event_loop)` |
| `async-compat` | tokio↔Slint interop | required to drive tokio futures from the Slint thread |
| `reqwest` | HTTP client | `rustls-tls` — avoid OpenSSL; `default-features = false` |
| `serde` + `serde_json` | state persistence | |
| `serde-saphyr` (2.1.1) | YAML | **what meow itself uses** (meow pins `=2.0.0`). Chosen over the deprecated `serde_yaml`, the unmaintained `serde_yml` shim, and `serde_norway`/`noyalib`. Needs a version-compat check against meow's pin before lockfile unification. |
| `sha2` | release checksum | |
| `zip` | Windows archive extraction | |
| `flate2` + `tar` | Unix archive extraction | |
| `thiserror` | error enums | |
| `tracing` + `tracing-subscriber` | logging | with a redaction layer |
| `uuid` | stable ids | `v4` |
| `time` or `chrono` | timestamps | |
| `windows` / `windows-sys` | DPAPI, console control events | `windows-sys` 0.61.2 has `GenerateConsoleCtrlEvent` |
| `security-framework` | macOS Keychain | |
| `secret-service` | Linux Secret Service | with a file fallback |
| `libc` / `nix` | Unix signals | `nix` for `kill(pid, SIGTERM)` |
| `dirs` | platform data/config dirs | |
| `tray-icon` | system tray (v1) | Linux needs a StatusNotifier host; the app degrades gracefully when absent (§6.4) |
| `elevation` / `sudo` | elevating the engine child for TUN | Windows `ShellExecuteW` + `runas`; macOS/Linux via `osascript`/`sudo`. Alternatively hand-rolled per platform — see the note below. |
| `wait-timeout` | child-stop timeout | the graceful stop has a deadline before the force kill |
| `url` | URL parsing/validation | |
| `base64` | base64 subscription bodies | |
| `rand` | controller secret generation | CSPRNG |

**On elevation:** there is no single well-maintained cross-platform crate that covers
Windows-`runas` + macOS-`osascript` + Linux-`sudo` with the same API, so the app implements a
small platform module (`starpod-engine/src/elevate.rs`) rather than taking a dependency on a
thin wrapper. It is roughly 80 lines and it is the one place where a bad implementation is a
security problem, so it is written explicitly rather than delegated.

**Deliberately absent:** no `openssl`/`native-tls` (use `rustls`), no `serde_yaml` (deprecated),
no `serde_yml` (unmaintained shim), no `tray-icon` before the tray decision (now added).

---

## 8. Testing strategy

GOAL.md §7. Every layer is testable without network or engine.

| Layer | What | How |
|---|---|---|
| `starpod-core` | **config pipeline**: normalization, merge precedence (P1–P6), validation, YAML emission | pure unit tests, **no I/O, no clock** — the pipeline takes a `now` and an `id` generator as parameters |
| `starpod-core` | **rule translation**: every `RuleKind` → correct token; flags; logic nesting; target resolution | table-driven tests over the token table mirrored from meow |
| `starpod-core` | **raw-text rule parsing** | every grammar form parses to a `RuleSpec` and re-emits identically (round-trip); invalid lines are rejected with a position; `MATCH` mid-list is rejected; the logic forms round-trip through the structured editor |
| `starpod-core` | **precedence invariants**: a subscription refresh never changes a user-set port/mode/selection | property-style test: random preferences + random subscription → assert the user-set fields are byte-identical after merge |
| `starpod-subscription` | **parsing/validation** | fixtures: valid YAML, base64-wrapped YAML, malformed YAML, wrong shape, empty, oversized, duplicate names, invalid port, unknown protocol |
| `starpod-subscription` | **network failure & expiry** | a mock provider (trait impl returning canned responses) — 401, 404, 500, timeout, connection reset, missing `subscription-userinfo`, expired `expire` |
| `starpod-subscription` | **both modes** | app-managed: the quota header is surfaced; engine-managed: the quota column is absent, not zero — an assertion that the UI model has no quota field for that row |
| `starpod-subscription` | **cache behaviour** | last-valid cache kept on failure; expired subscription detected from `expire` |
| `starpod-store` | **persistence & migrations** | round-trip, corrupt file → warning + default, missing file, schema upgrade path, secret separation |
| `starpod-engine` | **release download/verify** | a local fixture server; **sha256 mismatch → fail closed**; archive-with-top-level-dir layout; **`wintun.dll` ends up beside `meow.exe`** (the TUN prerequisite) |
| `starpod-engine` | **REST client** | a mock HTTP server (e.g. `wiremock`) for 401/400/404/500 and the `PUT /configs` body shapes |
| `starpod-engine` | **supervisor lifecycle** | start → healthy → reload → stop; crash detection; graceful-stop timeout → force kill; Windows path exercised on Windows CI |
| `starpod-engine` | **rule-order re-assertion** | a mock engine whose `POST /rules` records the order; assert that after a simulated engine refresh the user rules are first and `MATCH` is last |
| `starpod-engine` | **elevation** | the command line built for the elevated child is asserted to carry the secret and config path; the unelevated path is asserted to be the default |
| **integration** | **the central path** | subscription → config → runtime YAML → engine → traffic. GOAL.md §11 requires this to be *verified*, not assumed. |

**The integration test is the one that matters.** It must assert the whole chain:
mock provider returns a subscription → the pipeline produces YAML → the YAML passes `meow -t` →
the engine starts and reports `Running` → a rule from the subscription is visible in `GET /rules`
→ a user preference survives a simulated refresh.

**A live end-to-end check against a real `meow.exe` on the dev machine** is a separate,
opt-in test (`#[ignore]`d, run manually) — it needs the downloaded binary present.

### 8.3 CI matrix (user decision: all three platforms, every PR)

| Platform | Runner | meow asset under test | What it uniquely exercises |
|---|---|---|---|
| **Linux x64** | `ubuntu-latest` | `x86_64-unknown-linux-gnu` + `musl` | SIGTERM shutdown, `~/.cache` + XDG dirs, Secret Service keyring (and its file fallback), `.tar.gz` extraction, `/dev/net/tun` privilege error path |
| **Windows x64** | `windows-latest` | `x86_64-pc-windows-msvc` | `CTRL_BREAK_EVENT` shutdown, `CREATE_NEW_PROCESS_GROUP`, `.zip` extraction, `%LOCALAPPDATA%` dirs, **DPAPI secret storage**, `ShellExecuteW`+`runas` elevation, `wintun.dll` sidecar resolution |
| **macOS ARM64** | `macos-14` (arm64) | `aarch64-apple-darwin` | SIGTERM shutdown, `~/Library/Application Support` + `~/Library/Caches` dirs, **Keychain secret storage**, `.tar.gz` extraction, `osascript` elevation |

**Every PR runs the full suite on all three.** The platform-conditional code — asset name, archive
type, signal type, data dir, secret store, elevation, tray — is exactly where cross-platform bugs
hide, and a matrix that skips it on two of three targets would not catch them.

**What the matrix deliberately does *not* do:** it does not require an elevated runner. TUN's
privileged paths are therefore tested at the *error* branch (the app reports "the process must run
elevated" and keeps the mixed-port proxy running), with the privileged path covered by unit tests
of the command construction and by manual verification. This is the honest limit of CI.

**A release job** builds signed binaries for the three targets; the macOS artifact is arm64-only
(the user's platform scope).


---

## 9. Risks & unknowns

| # | Risk | Impact | Mitigation |
|---|---|---|---|
| R1 | **The subscription server's actual API is unknown.** Panels differ in URL format, auth header, and body format. | High — this is the core data source. | The provider trait + a format-sniffing parser handle the *de-facto* Clash/Mihomo format, which is what nearly every panel emits. The trait is the seam for vendor-specific APIs later. **App-managed is the default mode; engine-managed is the escape hatch.** |
| R2 | **The YAML library's compatibility with meow's** (`serde-saphyr` 2.1.1 vs meow's `=2.0.0`). | Medium — a mismatch in YAML quirks could produce YAML meow rejects. | Pin the same major/minor line meow uses; the app only *emits* simple YAML (mappings, sequences, scalars) and `meow -t` validates every candidate — so a quirk is caught before apply, not at runtime. |
| R3 | **No signature on release binaries.** | Medium | sha256 verification against the release's own `.sha256` catches corruption, not compromise. The app states this limitation in the UI. |
| R4 | **`GET /logs` wire format not yet confirmed** (polling vs SSE) and `GET /traffic` sampling shape. | Low–medium — affects the Logs/Dashboard screens only. | Confirm from `meow-api/src/log_stream.rs` / `traffic` handlers before building those widgets; design the UI to consume a normalized `LogStream` trait so the transport is swappable. |
| R5 | **The engine's `strict` mode and `-t` exit-code contract** for partially-invalid configs. | Medium — affects what "valid" means. | Verified: `-t` returns non-zero on parse failure; the app treats any non-zero as "reject the candidate" and surfaces stderr. |
| R6 | **TUN needs the child elevated, and CI runners are not elevated.** | Medium — the privileged path cannot be exercised automatically. | The error branch is fully tested (the app must keep the mixed-port proxy running and report the engine's message). The privileged path is covered by unit tests of the command construction and by manual verification on each platform. **This is the honest limit of CI and it is called out in §8.3.** |
| R7 | **First-run geo-rule latency** — the engine downloads MMDBs on first use of `GEOIP`/`GEOSITE`. | Low | `geodata.auto-update: true` moves it off the critical path; the UI shows a loading state. |
| R8 | **Slint's icon/logo asset pipeline** — no runtime SVG renderer. | Low | Convert Lucide SVG + the foxgirls mark to Slint `Path` geometry at build time; verify visually early. |
| R9 | **Subscription name collisions with user groups** could let a subscription hijack a user's rule target. | Medium — a genuine security-ish concern | In app-managed mode: namespacing + the merge precedence rules (P4) + validating that every rule target resolves within the merged config. In engine-managed mode: **not preventable** (the engine fetches the payload, so the app cannot rewrite names) — detected by comparing the live proxy list against the user's node names, and reported on the Servers screen. See §3.2.1 and §4.6. |
| R10 | **Windows rename-over-existing** for atomic writes and for the engine binary swap. | Low | Use `ReplaceFileW` via the `windows` crate; or write-new-then-`MoveFileExW(MOVEFILE_REPLACE_EXISTING)`. |
| R11 | **Engine-managed subscriptions lose the quota/expiry header.** | Medium — a visible gap in the Servers screen. | The UI shows "managed by the engine" in place of the quota bar rather than a zero or a guess. The Servers screen never displays a number the app does not have. |
| R12 | **Engine-managed subscriptions can shadow a user node, and meow does not restore it.** | Medium — silent data loss | Detected by snapshotting the user's node names across refreshes (§4.6), reported on the Servers screen, with an offer to convert the subscription to app-managed mode. |
| R13 | **Two subscription modes means two code paths that can drift.** | Medium | Both modes funnel into the *same* `SubscriptionPayload` → `NormalizedConfig` → YAML pipeline; only the *fetch* differs. The difference in observable behaviour (quota present/absent, rule order, payload format) is confined to one module and documented in §3.2.1's table, which is used as the test matrix. |
| R14 | **The tray may be unavailable** (Linux without a StatusNotifier host). | Low–medium | The app degrades gracefully: closing the window then stops the engine and exits, and the app says so once at startup. §6.4. |
| R15 | **The app can be killed and orphan the engine.** | Medium — a leaked proxy process after a crash | Ownership marker + adoption on next start, plus a panic hook that stops the child before exit. §6.4. |

---

## 10. Decision record (GOAL.md §8 checkpoint — resolved)

GOAL.md §8 required a single grouped checkpoint for the decisions that technical research could
not settle. The checkpoint was put to the owner; the answers are recorded here together with the
architectural consequences. **No open architectural questions remain.**

| # | Question | Decision | Architectural consequence |
|---|---|---|---|
| **Q1** | Who owns fetching and parsing the subscription? | **Both, per subscription** — a per-subscription toggle. App-managed is the default; engine-managed is opt-in. | Two fetch paths, one shared pipeline. App-managed adds a format sniffer, a validator and a cache; engine-managed adds a registration call and a supervision loop. The observable differences are tabulated in §3.2.1 and are the test matrix. |
| **Q2** | How does the app authenticate to the subscription server? | **URL token + an optional `Authorization` header.** | One extra field on the subscription model (`header`), one extra header on the provider request. Engine-managed mode cannot carry the header (meow's `AddSubscriptionRequest` has no header field) — the UI marks such subscriptions. |
| **Q3** | Is TUN mode in scope for v1? | **Yes, with an elevation prompt.** | The *child* is elevated, not the GUI (§4.5). The extractor must preserve `wintun.dll` beside `meow.exe`. TUN failure must not take down the mixed-port proxy. |
| **Q4** | What happens when the window is closed? | **Hide to the system tray; the proxy keeps running.** | Two exits, both graceful (§1.7, §6.4). Needs the `tray-icon` crate, an ownership marker, adoption-on-restart, and a graceful degradation path where no tray exists. |
| **Q5** | How much of the subscription-vs-preference split does the server control? | **The app is authoritative.** User settings always win; a refresh never overwrites them. | The precedence table P1–P6 (§3.4) is the contract. For engine-managed subscriptions, the engine's own rule order is corrected by the re-assertion loop (§4.6). |
| **Q6** | How are rules edited in v1? | **Structured editor + a raw-text advanced mode.** | Both editors operate on the same `RuleSpec` list; raw lines are *parsed into* `RuleSpec`s, so they cannot bypass the target-resolution validation (§3.3.1). |
| **Q7** | What persistence format? | **JSON** for state, YAML for the generated config and the subscription cache. | The store sits behind a trait, so this is cheap to revisit. JSON matches the rest of the stack. |
| **Q8** | Which platforms are built and tested in CI for v1? | **Linux x64, Windows x64, macOS ARM64 — all three, every PR.** | §8.3's matrix, with the TUN privilege limitation stated rather than hidden (R6). |
| **Q9** | How is the rule order reconciled when the engine refreshes a subscription on its own? | **The app re-asserts the order after the refresh.** | A 30-second poll of `GET /api/subscriptions` watching `last_updated`, then `GET /rules` → reorder → `POST /rules`. Safe because `replace_rules` calls `reconcile_contribution_claims`, which re-derives the engine's contribution bookkeeping from what is actually in the config (§4.6). |

**Decisions taken earlier in the conversation** (also recorded in §0): the meow core is the
prebuilt release binary driven as a subprocess (not built from source); the platform scope is
Linux, Windows and macOS-on-ARM; no subagents; the visual reference is foxgirls.org.

**Assumptions still standing, flagged for the record:**
- The subscription format is the *de-facto* Clash/Mihomo YAML. No specific vendor panel is
  claimed as supported (R1).
- The release checksum is trusted from the same GitHub release as the binary (R3).
- macOS is arm64-only; x86_64 macOS is not shipped.

---

## 11. Implementation plan

Ordered by dependency. Each step names the files it owns, so parallel work cannot conflict.

**Phase 3 — Scaffolding**
1. Cargo workspace: `starpod-core`, `starpod-subscription`, `starpod-store`, `starpod-engine`,
   `starpod-ui`, `starpod-app`. `rust-toolchain.toml` pinning 1.89 (meow's MSRV).
2. `starpod-app`: `main.rs` with the tokio+Slint bootstrap (§1.10), `AppState`, the event loop.
   → **files:** `Cargo.toml` (workspace), `crates/*/Cargo.toml`, `crates/starpod-app/src/main.rs`

**Phase 4 — Core logic**
3. `starpod-core`: domain types (§3.1–3.3), the rule grammar table (§1.5), the translator, the
   raw-text parser (§3.3.1), the merge implementation with P1–P6, the validators, the YAML emitter.
   → **files:** `crates/starpod-core/src/**`
4. `starpod-subscription`: provider trait, HTTP provider (URL token + optional header), format
   sniffer, validators, the cache, **and the app-managed/engine-managed mode split** (§3.2.1).
   → **files:** `crates/starpod-subscription/src/**`
5. `starpod-store`: JSON files, atomic write, migrations, keyring (DPAPI / Keychain / Secret
   Service) + fallbacks.
   → **files:** `crates/starpod-store/src/**`
6. `starpod-engine`: asset mapping, download+verify+extract (preserving `wintun.dll` beside
   `meow.exe`), supervisor, elevation module (§4.5), REST client, `meow -t` validation, signal
   handling per platform, **and the rule-order re-assertion supervisor** (§4.6).
   → **files:** `crates/starpod-engine/src/**`

**Phase 5 — UI**
7. Design tokens + primitives + the five screens, including the dual rule editor (§3.3.1) and
   the TUN settings section (§4.5).
   → **files:** `crates/starpod-ui/ui/**`, `crates/starpod-ui/src/**`
8. Wire the screens to `AppState`; the app's Rust handlers. **System tray** (§6.4) and the
   window-close-to-hide rule.
   → **files:** `crates/starpod-app/src/**`, `crates/starpod-ui/src/tray.rs`

**Phase 6 — Hardening**
9. Test suites per §8, including the central integration test.
10. CI matrix per §8.3 — three platforms, every PR.
11. `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test --workspace`.
12. Security review pass against GOAL.md §6.

**Phase 7 — Documentation**
13. `README.md`, a user guide, and a build/release doc.

---

## 12. File-ownership map (so parallel work never collides)

| Path | Owner step |
|---|---|
| `Cargo.toml` (workspace), `rust-toolchain.toml` | 1 |
| `crates/starpod-core/**` | 3 |
| `crates/starpod-subscription/**` | 4 |
| `crates/starpod-store/**` | 5 |
| `crates/starpod-engine/**` | 6 |
| `crates/starpod-ui/ui/**` (`.slint`) | 7 |
| `crates/starpod-ui/src/**` | 7–8 |
| `crates/starpod-app/src/**` | 1, 8 |
| `.github/workflows/**` | 10 |
| `docs/research/**` | research (done) |
