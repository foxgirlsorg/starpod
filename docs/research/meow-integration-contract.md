# meow-rs Integration Contract — VERIFIED FIRST-HAND

Everything here was verified against the vendored crate source and the **actual downloaded
release binary**, not inferred. Sources marked `[src]` are file:line in
`~/.cargo/registry/src/index.crates.io-*/meow-*-0.29.0/`; `[bin]` is the real
`meow-v0.29.0-x86_64-pc-windows-msvc.zip` from the v0.29.0 GitHub release, unpacked and run.

---

## 1. Release distribution (VERIFIED)

`v0.29.0` — published 2026-11-05, stable, not a prerelease. 48 assets. Naming convention:

| Channel | Pattern | Example |
|---|---|---|
| Stable | `meow-v<version>-<rust-target-triple>.<ext>` | `meow-v0.29.0-x86_64-pc-windows-msvc.zip` |
| Rolling alpha | `meow-alpha-<commit>-<rust-target-triple>.<ext>` | `meow-alpha-9f9dc68-x86_64-pc-windows-msvc.zip` |

Windows targets shipped: `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`, `i686-pc-windows-msvc`.
Linux: `x86_64/aarch64/armv7/riscv64gc` × `gnu`/`musl`. macOS: `x86_64`/`aarch64` `-apple-darwin`.
Also OpenWrt `.ipk`/`.apk` and LuCI packages — **not relevant to a desktop client.**

**Asset URL (no API needed):**
`https://github.com/meow-rs/meow-rs/releases/download/v0.29.0/meow-v0.29.0-x86_64-pc-windows-msvc.zip`

**Checksum asset** — one `.sha256` per binary, `sha256sum` output format:
```
3ffd602369e2b852eca6348ba10689cc7f1f49f3f5935b8f8810a7d4e88dc275 *meow-v0.29.0-x86_64-pc-windows-msvc.zip
```
Verified: the downloaded zip's `sha256sum` equals this value exactly. **Parsing rule:** split on
whitespace, take field 0 as the hex digest (the `*` prefix on the filename means binary mode).

**Archive contents** (verified with `unzip -l`):
```
meow-v0.29.0-x86_64-pc-windows-msvc/
├── meow.exe                    16,044,032   ← the proxy core
├── meow-cli.exe                1,460,736   ← reference REST client
├── wintun.dll                    427,552   ← TUN support, bundled
├── LICENSE
├── LICENSE.wintun.txt
├── README.md                     27,929
└── skills/meow-cli/SKILL.md      11,763   ← authoritative CLI docs
```
The archive has a **single top-level directory** named after the asset — the extractor must
descend into it, not assume files sit at the archive root. **No geodata files are bundled**:
see §6.

**No signatures.** The project publishes only sha256, fetched from the same release as the
binary. That means the checksum defends against corruption and truncated downloads, but **not**
against a compromised GitHub release. Worth telling the user; it does not block the design.

---

## 2. CLI surface (VERIFIED by running `meow.exe --help`)

`meow.exe` — "A rule-based tunnel in Rust". Commands: `install`, `uninstall`, `status`, `help`.

| Flag | Meaning |
|---|---|
| `-f, --config <CONFIG>` | Path to configuration file. **Default `config.yaml`** |
| `-d, --directory <DIRECTORY>` | Home directory |
| `-t, --test` | **Test configuration and exit** |
| `-m, --geodata-mode` | Set geodata mode |
| `--config-string <CONFIG_STRING>` | **base64-encoded** configuration string |
| `--ext-ui <EXT_UI>` | Override external UI directory |
| `--ext-ctl <EXT_CTL>` | Override external controller address |
| `--ext-ctl-tls <EXT_CTL_TLS>` | Override external controller TLS address |
| `--ext-ctl-unix <EXT_CTL_UNIX>` | Override external controller unix address |
| `--ext-ctl-pipe <EXT_CTL_PIPE>` | Override external controller pipe address |
| `--ext-ctl-routing-mark <EXT_CTL_ROUTING_MARK>` | Override external controller routing mark |
| `--secret <SECRET>` | Override secret for RESTful API |
| `--post-up <POST_UP>` / `--post-down <POST_DOWN>` | post-up/post-down scripts |
| `--age-secret-key <AGE_SECRET_KEY>` | Decrypt configuration (age) |
| `--no-external-plugins` | Disable executable SIP003 plugins |
| `-V, --version` | Print version |

**Key findings for our design:**
- `-t/--test` gives us **config validation without starting the proxy** — exactly the
  "validate before applying" requirement in GOAL.md §3. The app can run
  `meow.exe -t -f <candidate.yaml>` and check the exit code before touching the running process.
- `--ext-ctl` + `--secret` let us pin the controller to loopback with a known secret rather than
  letting the engine pick a port and write it to a controller file. **We should always pass both
  explicitly** — it removes the need to discover the controller file and avoids a race where the
  engine has not yet written it.
- `--config-string` is **base64**, matching the REST API's `payload` field (§4). Note the CLI
  flag is base64 of the *whole config*, while `-f` takes a plain file path — the file path is
  easier to inspect and to write atomically, so prefer `-f`.

### Shutdown
The CLI has no `--shutdown` flag and no stop command. Shutdown is either:
- the OS terminating the process, or
- the REST API (no explicit shutdown endpoint was found in the route table — see §4).

Windows note: `meow.exe` is a console app. `CTRL_BREAK_EVENT` is deliverable to a child only if
the child was created with `CREATE_NEW_PROCESS_GROUP`; `CTRL_C_EVENT` to a process group is
another route. `TerminateProcess` is the blunt fallback. **This needs a decision** — see the
questions in §8.

---

## 3. Controller discovery (VERIFIED)

`meow-common/src/controller_file.rs` — when `external-controller` is bound, meow writes
`<pid>.json` into:
- Windows: `%LOCALAPPDATA%\meow\controllers\`
- macOS: `~/Library/Caches/meow/controllers/`
- other unix: `~/.cache/meow/controllers/`
- root on unix: `/var/run/meow/controllers/`

Overridable with env `MEOW_RUNTIME_DIR`.

File content (`Entry`, serde JSON):
```json
{ "pid": 1234, "controller": "127.0.0.1:9090", "secret": "…", "config": "C:\\…\\config.yaml" }
```
`secret` and `config` are omitted when empty/absent.

**Security properties of the mechanism** (from the module's own docs and tests): the file is
written under a **shared lock** and the lock is held for the process's lifetime; the lock — not
the pid — is the liveness test, so a recycled pid cannot pass for a live meow. On unix the dir is
`0700` and the file `0600`; only a file owned by the current user is accepted, so another user's
file cannot point a client at an attacker's controller. The `Debug` impl redacts the secret as
`<redacted>`.

**Consequence for our app:** we do not need to use this. We will pass `--ext-ctl` and `--secret`
explicitly, so the app already knows the address and the secret. But we *should* still set
`MEOW_RUNTIME_DIR` to a per-app directory so our instance's controller file does not collide
with, or get swept by, any other meow on the machine.

---

## 4. REST controller API (VERIFIED against `meow-api-0.29.0/src/routes.rs`)

Axum router. Full route table, verbatim from `routes.rs:194-289`:

```
GET    /                                    → hello
GET    /version
GET    /proxies
GET    /proxies/{name}
PUT    /proxies/{name}                      → update_proxy
DELETE /proxies/{name}                      → unfix_proxy
GET    /proxies/{name}/delay
GET    /proxies/{name}/chain-health
GET    /group
GET    /group/{name}
GET    /group/{name}/delay
GET    /rules
POST   /rules                               → replace_rules
PUT    /rules/{index}                       → update_rule_at_index
DELETE /rules/{index}                       → delete_rule
POST   /rules/reorder
GET    /connections
GET    /connections/{id}
DELETE /connections/{id}
DELETE /connections                         → close all
GET    /configs
PATCH  /configs                             → update_configs (partial)
PUT    /configs                             → put_configs (full reload)
GET    /metrics
GET    /traffic
GET    /traffic/ledger
DELETE /traffic/ledger
GET    /statistics
GET    /logs
GET    /logbook
DELETE /logbook
GET    /memory
GET    /dns/results
GET    /dns/query
POST   /dns/query
POST   /cache/dns/flush
POST   /cache/fakeip/flush
POST   /api/config/save
GET    /api/subscriptions
POST   /api/subscriptions
DELETE /api/subscriptions/{name}
POST   /api/subscriptions/{name}/refresh
GET    /api/proxy-groups
POST   /api/proxy-groups
PUT    /api/proxy-groups/{name}
DELETE /api/proxy-groups/{name}
PUT    /api/proxy-groups/{name}/select
GET    /providers/proxies
GET    /providers/proxies/{name}
PUT    /providers/proxies/{name}             → refresh_provider
GET    /providers/proxies/{name}/healthcheck
GET    /providers/proxies/{provider_name}/{proxy_name}
GET    /providers/proxies/{provider_name}/{proxy_name}/healthcheck
GET    /providers/rules
GET    /providers/rules/{name}
PUT    /providers/rules/{name}               → refresh_rule_provider
GET    /listeners
GET    /ui, /ui/{*rest}                      → built-in web panel
```

### Auth
The whole router is wrapped in `middleware::from_fn_with_state(..., require_auth_ws)` — a
`Bearer` secret check. `401` when the secret is missing or wrong. The `/ui` web panel is
deliberately **unauthenticated** (matches upstream mihomo) and serves a token prompt.

**Our app must:** generate a strong random secret, pass it via `--secret` when starting meow, and
send `Authorization: Bearer <secret>` on every request. Never log it; never put it in the YAML
file we write (the file may contain node passwords too).

### `PUT /configs` — the hot-reload path (VERIFIED, `routes.rs:3363+`)
Request body (`PutConfigsBody`, `#[derive(Deserialize)]`):
```json
{ "path": "C:\\path\\to\\config.yaml" }
```
or
```json
{ "payload": "<base64 of the YAML>" }
```
Exactly one of `path` / `payload`; neither → `400 "request body must contain 'path' or 'payload'"`.
`payload` must be **standard** base64 (`STANDARD.decode`), and the decoded bytes must be UTF-8.

Query parameter: `?force=true` (any other value is not force).

Behavior:
1. YAML is parsed via `meow_config::parse_raw_yaml` — **syntax errors are always `400`, even
   with `force=true`**.
2. Duplicate `subscriptions:` names → `400 "duplicate subscription name"` (never commit).
3. Under `force=true`, a config whose strict checks / TUN / listeners / DNS fail is still
   applied; otherwise broken proxies/groups/rules are skipped with a warning and the rest applied
   (unless the config sets `strict: true`).
4. `reload` (per SKILL.md) sends the file's text and **closes every open connection**.

**Our design should use `path`**, not `payload`: the app writes the YAML to disk atomically,
then PUTs the path. That gives the user a readable config file, makes the config diffable, and
lets the app validate the same bytes with `meow.exe -t -f <path>` before reloading.

### `PATCH /configs` — partial (VERIFIED, `routes.rs:847+`)
```json
{ "mode": "rule|global|direct", "log-level": "info", "tun": { … } }
```
All fields optional; `tun` keys are `enable, device, mtu, inet4_address, inet6_address,
auto_route, outbound_interface, dns_hijack, udp_timeout` (kebab-case; unknown mihomo keys like
`stack`/`strict-route` are ignored). Fields are **validated before anything is applied**, so a
bad patch never partially applies.

### `GET /configs` response (`ConfigResponse`)
`mode`, `log_level`, `mixed_port`, `socks_port`, `http_port`, `redir_port`, `tproxy_port`,
`external_controller`, `allow_lan`, `bind_address`, `ipv6`, `tun_enable`, `tun`.

### Live state endpoints
- `GET /traffic` — traffic sample (the SKILL.md shows `meow-cli traffic --count 5` polls one
  sample per second).
- `GET /connections` — open connections; `GET /connections/{id}` for one.
- `GET /logs` — log stream (SKILL.md: `meow-cli log --count 50` "follow the log"; so it is
  polling/streaming — exact wire format still to confirm from `log_stream.rs`).
- `GET /logbook` — notable engine events, **survives restarts**, unlike `/logs`. Kinds include
  `config.reload`, `config.change`. Levels filterable.
- `GET /statistics` — per-proxy byte counters (`meow-cli stats` = "bytes each proxy carried").

**There is no shutdown endpoint.** The route table has no `/shutdown` or `/stop`.

---

## 5. What `meow-cli` (the reference client) reveals (VERIFIED by running it + reading SKILL.md)

The release ships `skills/meow-cli/SKILL.md` — a complete, authoritative description of the API's
semantics. It is the best single document about this engine's behavior. Highlights that shape
the app:

- `meow-cli status` → "version, mode, log level, TUN, ports, traffic totals".
- `meow-cli group list` / `group show "<name>"` → groups, members, last delay.
- `meow-cli group select "<group>" "<member>"` → switches a `select` group; **meow remembers the
  pick across restarts**.
- `meow-cli proxy test "<name>"` / `group test` → delay test.
- `meow-cli rule list --filter <word>` → rules in match order with index.
- `meow-cli provider list` → proxy and rule providers, usage and expiry.
- `meow-cli subscription list` → **shows only the subscription's host**; "Subscription URLs carry
  access tokens: … Never ask for or print the full URL."
- `meow-cli logbook --kind config` → config reload/change events. **Our app's Logs screen should
  surface exactly these.**
- `meow-cli reload <path>` → sends the file's text and closes all connections.
- `meow-cli tun on|off` → toggles TUN at runtime.
- `meow-cli mode rule|global|direct` → changes the running mode, kept in meow's in-memory config.
- Errors: `401` = secret missing/wrong; `400`/`404` = meow refused, message says why.

**`meow` has its own first-class subscription support** — `POST /api/subscriptions`,
`POST /api/subscriptions/{name}/refresh`, `GET /api/subscriptions`. The engine can fetch and
refresh subscriptions itself. This is a significant architectural option: our app could either
(a) fetch and parse the subscription itself and generate YAML, or (b) hand the subscription URL
to meow and let the engine own node refresh. GOAL.md §3 asks for *our own* subscription layer with
caching, expiry handling and separation from user preferences, so (a) is the spec-compliant
choice — but (b) is a real alternative worth putting to the user.

---

## 6. Geodata — RESOLVED (meow fetches it itself)

`meow-config/src/geodata.rs` — the defaults are the upstream MetaCubeX datasets:

| Dataset | Default URL |
|---|---|
| Country MMDB (GeoIP) | `https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/country.mmdb` |
| ASN MMDB | `https://github.com/P3TERX/GeoLite.mmdb/releases/latest/download/GeoLite2-ASN.mmdb` |
| Geosite | `https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/geosite.dat` |

Key facts:
- **meow has no `geoip.dat` consumer.** `geox-url.geoip` is accepted and **ignored with a
  warning**; the GeoIP MMDB URL is set with `geox-url.mmdb` instead.
- **Startup fetching is lazy and targeted** (`ensure_geodata` inside `build_config`): it
  "downloads only the databases the config actually references before the rule/parser context is
  built — there is no unconditional boot-time sweep". So a config with no `GEOIP`/`GEOSITE` rules
  downloads **nothing**.
- `geodata.auto-update` (default `false`) turns on a background refresh loop;
  `auto-update-interval` defaults to `24` (hours).
- `geosite-path` overrides the local geosite file path.
- Downloads honor `resolve-geodata-egress`: `DIRECT`/`COMPATIBLE`/`PASS` fetch direct,
  `REJECT`/`REJECT-DROP` do not fetch.

**Consequence for the app: the app does not need to ship or fetch geodata.** The engine does it,
lazily, only for the rule types the generated config actually uses. This means:
- Our config builder may safely emit `GEOIP`/`GEOSITE` rules.
- On first use of a geo rule the engine will download a few MB — the UI must show that as a
  loading state on the first start after such a config is applied, and the Logs screen should
  surface the download.
- If the app wants to avoid the download latency, it can set `geodata.auto-update: true` so the
  refresh happens in the background rather than blocking the first rule compile.

---

## 7. Shutdown — RESOLVED (Ctrl-C is the graceful path on Windows too)

`meow-app/src/main.rs:142-175` — `ShutdownSignal`:

```rust
enum ShutdownSignal {
    Console,
    #[cfg(target_os = "windows")]
    WindowsService(tokio::sync::oneshot::Receiver<()>),
}
```

`ShutdownSignal::wait()` on **Windows** (`#[cfg(not(unix))]` branch) does:
```rust
tokio::signal::ctrl_c().await?;
Ok("Ctrl-C")
```
On unix it additionally waits for `SIGTERM`.

So on Windows the engine's graceful shutdown trigger is a **Ctrl-C console event**, which
`tokio::signal::ctrl_c()` receives as `CTRL_C_EVENT` / `CTRL_BREAK_EVENT` on the console
process group. `main.rs:1663-1672` comments that the signal handler is only installed once
startup is complete ("Only now: a signal before `shutdown.wait()` installs its handler…"), and
then it waits for the shutdown signal.

**How the app must deliver it:** spawn the child with `CREATE_NEW_PROCESS_GROUP` and then send
`CTRL_BREAK_EVENT` to that process group (`GenerateConsoleCtrlEvent` via `windows-sys`).
`CTRL_C_EVENT` cannot be targeted at a specific group (it would also hit the app itself);
`CTRL_BREAK_EVENT` can. `tokio::process::Command` on Windows supports
`creation_flags(CREATE_NEW_PROCESS_GROUP)`.

Fallback if the graceful path does not complete within a timeout: `TerminateProcess` (the child
is killed; the running config is lost but the app holds the source config and can restart).

Also verified: `main.rs` registers the Windows **service** mode (`ShutdownSignal::WindowsService`)
via a oneshot channel, i.e. the engine can also be installed as a Windows service. **Our app
should NOT use the service mode** — it supervises the process directly with `tokio::process`,
which is simpler and keeps the lifetime tied to the GUI.

---

## 8. Rule model — to be pinned down from `meow-rules` + `meow-config/src/rule_parser.rs`

Not yet verified first-hand at the token level. Expected shape (to confirm): `DOMAIN`,
`DOMAIN-SUFFIX`, `DOMAIN-KEYWORD`, `DOMAIN-REGEX`, `IP-CIDR`, `IP-CIDR6`, `IP-SUFFIX`, `IP-ASN`,
`GEOIP`, `GEOSITE`, `SRC-IP-CIDR`, `SRC-PORT`, `DST-PORT`, `PROCESS-NAME`, `PROCESS-PATH`,
`IN-NAME`, `IN-TYPE`, `IN-USER`, `UID`, `NETWORK`, `DSCP`, `AND`/`OR`/`NOT`, `MATCH`.
Targets: `DIRECT`, `REJECT`, `REJECT-DROP`, `PASS`, `COMPATIBLE`, `MATCH`, or a proxy/group name.
Plus `no-resolve` flags.

---

## 9. Decisions this raises for the user

1. **Subscription ownership** — does the app parse the subscription itself and generate YAML
   (spec-compliant per GOAL.md §3), or does it delegate to meow's own subscription support
   (`POST /api/subscriptions`)? The latter is far less code but gives up our control of caching,
   expiry and precedence.
2. **TUN mode** — `wintun.dll` ships in the archive, so TUN is feasible on Windows; but TUN needs
   admin rights and a driver install. Is TUN in scope for v1, or mixed-port only?
3. **Config delivery** — `path` vs base64 `payload` for `PUT /configs` (recommend `path`).

Resolved and no longer open: shutdown mechanism (Ctrl-C via `CREATE_NEW_PROCESS_GROUP`, §7),
geodata (engine fetches lazily, §6).

---

## 10. Reproducing this

```bash
curl -sL -o meow.zip https://github.com/meow-rs/meow-rs/releases/download/v0.29.0/meow-v0.29.0-x86_64-pc-windows-msvc.zip
sha256sum meow.zip   # 3ffd602369e2b852eca6348ba10689cc7f1f49f3f5935b8f8810a7d4e88dc275
unzip -l meow.zip
unzip meow.zip -d dist && ./dist/*/meow.exe --help
```
