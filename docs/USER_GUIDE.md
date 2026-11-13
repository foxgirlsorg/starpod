# Starpod User Guide

This guide covers the desktop app: installing the engine, adding subscriptions, choosing servers, writing rules, and where your data lives. For building the app, see [DEVELOPMENT.md](DEVELOPMENT.md).

Screen and button names may change before the first release. The behaviour described here is what the code does today.

## 1. First run and the engine

Starpod does not bundle the proxy engine. It downloads the engine the first time you install it, from the Settings or Dashboard screen.

- **Source.** `https://github.com/meow-rs/meow-rs/releases/download/v0.29.0/` and the matching `.sha256` file from the same release. The asset is chosen for your platform: `x86_64-pc-windows-msvc` (zip), `aarch64-apple-darwin` (tar.gz), `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu` (tar.gz).
- **Check.** The app computes the SHA-256 of the downloaded archive and compares it with the hash in the `.sha256` file from the same release. A mismatch deletes the download and installs nothing.
- **What this does not check.** The release is not signed, and the app does not verify a signature. The check catches corruption and a mismatched download. It does not catch a compromised release, because the attacker could publish a matching hash too.
- **Where it goes.** `<local data>/starpod/engine/0.29.0/` (on Windows, `%LOCALAPPDATA%\starpod\engine\0.29.0\`). On Windows the archive's `wintun.dll` is copied next to the engine.

The engine runs as your user. It is never installed system-wide.

## 2. Subscriptions

A subscription is a URL your provider gives you. Add one with:

- **Name.** Up to 80 characters. Nodes from this subscription are shown as `<name> / <node name>`, so two providers cannot collide by name.
- **Address.** An `http://` or `https://` address with a host. Include your token in the address if the provider uses one.
- **Authorization header** (optional). The value of an `Authorization` header the provider requires. Leave it empty if the URL alone works.
- **Mode.** App-managed or engine-managed (section 2.1). Choose app-managed unless you have a reason not to.

The app stores the address and the header in the secret store (section 8). The state files keep only a reference to them.

Adding a subscription does not fetch it. Use Refresh on the subscription, or wait for the automatic refresh. App-managed subscriptions refresh automatically when their interval has passed. The default interval is 60 minutes.

### 2.1 App-managed and engine-managed

| | App-managed | Engine-managed |
|---|---|---|
| Who downloads it | The app | The engine, on its own refresh schedule |
| Who parses and merges it | The app. Its nodes and rules take part in the precedence rules in section 4 | The engine. Its nodes and rules are not merged by the app |
| Accepted body formats | Clash/Mihomo YAML, or the same YAML wrapped in base64 | Whatever the engine accepts. The engine requires a `proxies:` key and does not unwrap base64 |
| Traffic quota and expiry | Shown, if the provider sends the `subscription-userinfo` header | Not shown. The engine discards that header. The app never displays a number it does not have |
| Node count | Shown | Not known to the app, shown as zero |
| `Authorization` header | Sent with each fetch | Not sent. The app keeps the value, but the engine's subscription entry has no field for it, so the header does not reach the fetch |
| Rule order | Subscription rules come after your rules, before the `MATCH` tail | The engine's own order. The app does not re-check it after a refresh |
| Name collisions with your nodes | Namespaced as `<name> / <node>`, and your settings take precedence | Not prevented. The engine can replace a node of yours with the same name, and the app does not detect this yet |

Use app-managed unless the provider's payload is one the app cannot parse, or you need the engine to fetch it for a reason specific to your setup.

If a refresh of an app-managed subscription fails, the app keeps the last payload that worked, shows the error on the subscription, and keeps the proxy running on the previous configuration. The subscription's "last updated" time does not change on failure, so you can see how old the working data is.

### 2.2 Removing a subscription

Removing a subscription deletes its cached payload and its stored address and header. Your own nodes, groups and rules are not affected.

## 3. Servers and node selection

The Servers screen lists the nodes from your own list and from app-managed subscriptions.

- **Select a node.** While the proxy is running, the selection is applied live to the engine. While it is stopped, the selection is only saved and applies on the next start.
- **Selection survives refreshes.** The selected node is remembered by its internal id, not its name. If the provider renames a node, your selection stays on it.
- **Latency test.** Tests each node by asking the engine to measure a request to `https://www.gstatic.com/generate_204` with a 5-second timeout. It needs the proxy to be running. Results are kept until the proxy stops. The probe uses the engine's `/proxies/{name}/delay` endpoint, which follows the mihomo convention; it has not been checked against a live engine in this repository.

## 4. Configuration precedence

The running configuration is built from your settings and the subscriptions. When the two disagree, these rules decide:

| # | What | Rule |
|---|---|---|
| P1 | Ports | Your setting, then the subscription's, then the default |
| P1a | Allow-LAN, bind address | Your setting, then the safe default (off, loopback). A subscription is never consulted. |
| P2 | Mode, log level, TUN | Your setting, then the subscription's, then the default |
| P3 | Nodes | The union of subscription nodes and your nodes. On a name clash, yours wins |
| P4 | Proxy groups | Per group name. Your group replaces a subscription group of the same name |
| P5 | Rules | Your rules first, in your order. Then subscription rules. Then the `MATCH` tail |
| P6 | Selected node | Your selection, by node id. Then the subscription's default |

Two guarantees follow from this:

- **A refresh never changes your settings.** A refresh writes only the subscription's own metadata and cached payload. It has no path to your settings file.
- **A failed build changes nothing.** If the configuration does not validate, nothing is applied, the running proxy is untouched, and the error is shown.

Allow-LAN and the bind address are never taken from a subscription. See section 6.

## 5. Rules

Rules decide where each connection goes. The engine checks them from top to bottom and the first match wins. The order is:

1. Your rules, in the order you set.
2. Subscription rules from app-managed subscriptions.
3. `MATCH`, the catch-all. The app always places it last. If you write a `MATCH` rule yourself, the app drops it and writes its own.

### 5.1 Two editors

- **Structured editor.** One rule per row: kind, payload, target, and flags. Changes are validated before they are saved.
- **Raw text.** One engine rule per line, in the engine's own syntax. Blank lines and lines starting with `#` are ignored. If any line is invalid, nothing is saved, and every bad line is reported with its line number.

Raw text is parsed into the same rule model the structured editor edits, so a rule entered either way is stored the same way.

### 5.2 Rule kinds

The app supports the 31 rule tokens of the meow grammar (`crates/starpod-core/src/rules/grammar.rs`):

| Group | Kinds |
|---|---|
| Domain | `DOMAIN`, `DOMAIN-SUFFIX`, `DOMAIN-KEYWORD`, `DOMAIN-REGEX`, `DOMAIN-WILDCARD` |
| Geo | `GEOSITE`, `GEOIP`, `SRC-GEOIP`, `IP-ASN`, `SRC-IP-ASN` |
| IP | `IP-CIDR`, `SRC-IP-CIDR`, `IP-SUFFIX`, `SRC-IP-SUFFIX` |
| Port and network | `SRC-PORT`, `DST-PORT`, `IN-PORT`, `NETWORK`, `DSCP` |
| Process and user | `PROCESS-NAME`, `PROCESS-PATH`, `UID` |
| Inbound | `IN-NAME`, `IN-TYPE`, `IN-USER` |
| Logic and blocks | `AND`, `OR`, `NOT`, `SUB-RULE`, `RULE-SET` |
| Catch-all | `MATCH` (written by the app, not by you) |

Things to know:

- **Targets** are a proxy group or node name, or a built-in outbound: `DIRECT`, `REJECT`, `REJECT-DROP`, `COMPATIBLE`, `PASS`, `PASS-RULE`. A target that names nothing in the candidate configuration fails the build, and the error names the rule.
- **Flags.** `no-resolve` stops the engine resolving a domain to an IP before matching. `src` matches the source address instead of the destination.
- **Logic rules** (`AND`, `OR`, `NOT`) take their inner rules without a target. The enclosing rule owns the target.
- **`RULE-SET` does not work yet.** It refers to a rule provider, and the app currently supplies no providers, so a `RULE-SET` rule fails validation with a message naming the missing provider.
- **Geo rules.** `GEOIP`, `SRC-GEOIP`, `IP-ASN` and `SRC-IP-ASN` make the app show a warning. The engine downloads its geo database the first time it needs it, so the first match can be slow. `GEOSITE` needs no database check; if the database is absent, the rule simply never matches.

## 6. Settings

| Setting | Default | Notes |
|---|---|---|
| Mixed port (HTTP and SOCKS) | 7890 | Used when you leave it unset |
| SOCKS port | 7891 | Used when you leave it unset |
| Port | unset | Optional |
| Allow LAN | Off | See below |
| Bind address | unset | Passed to the engine only when you set it |
| Mode | Rule | Rule, Global or Direct. Global sends everything through the selected node. Direct sends everything directly |
| Log level | Info | Silent, Error, Warning, Info or Debug |
| TUN | Off | See below |

**Allow LAN is off by default, and why.** When it is on, other machines on your network can use the proxy ports. You did not ask for that, so the default is off.

**A subscription cannot turn Allow LAN on.** Whether the proxy is reachable from your network is your decision alone. A subscription's `allow-lan` and `bind-address` values are ignored, and the default stays off until you change it.

**TUN** sends all system traffic through a virtual network interface. It needs administrator rights on Windows, root on macOS, and root or `CAP_NET_ADMIN` on Linux. The app does not start the engine with those rights. Turning TUN on sends the setting to the running engine, which works only if the engine already has the privileges. The elevation helper described in `docs/research/ARCHITECTURE.md` section 4.5 is not implemented. Treat TUN as not yet usable.

## 7. Logs

The Logs screen has two streams:

- **App events.** Actions, refreshes and errors, kept as the last 500 lines.
- **Engine output.** The engine's standard output and error, held in a bounded buffer. The subscription address, the header value and the controller secret are removed before a line is stored.

Secrets never appear in an error message either. The app scrubs every error text against the secrets it has read.

## 8. Tray and window

- **Tray available.** Closing the window hides it to the system tray. The proxy keeps running. Quit in the tray menu stops the engine gracefully and exits. The engine gets a stop signal and 10 seconds to exit before it is killed.
- **Tray not available.** Some Linux desktops have no tray host. Closing the window then stops the engine and exits. The app says so once at startup.

The app does not start itself at login. There is no autostart setting yet.

## 9. Where your data is

The paths follow each platform's own conventions. The table is taken from the `dirs` crate's mapping, which is what `starpod-store` uses.

| | Windows | macOS | Linux |
|---|---|---|---|
| Settings and rules (`user-state.json`) | `%APPDATA%\Starpod` | `~/Library/Application Support/Starpod` | `~/.config/Starpod` |
| Subscription metadata (`subscriptions.json`), generated config (`runtime-config.yaml`), fallback secrets, controller files (`controllers/`) | `%LOCALAPPDATA%\Starpod` | `~/Library/Application Support/Starpod` | `~/.local/share/Starpod` |
| Cached subscription payloads (`subscriptions/`) | `%LOCALAPPDATA%\Starpod` | `~/Library/Caches/Starpod` | `~/.cache/Starpod` |
| Engine binary | `%LOCALAPPDATA%\starpod\engine\0.29.0` | `~/Library/Application Support/starpod/engine/0.29.0` | `~/.local/share/starpod/engine/0.29.0` |

Notes on this table:

- On macOS the settings and the data share one folder. They are still separate files, so a subscription refresh still cannot touch `user-state.json`. The separation is by file, not by folder, on that platform.
- On Windows the cache shares the data folder. Its files are in a `subscriptions` subfolder, which does not collide with `subscriptions.json`.
- The engine path uses lowercase `starpod`, while the other folders use `Starpod`. On Windows and macOS the file system ignores case, so both names are the same folder. On Linux they are different folders.

The generated `runtime-config.yaml` can contain a subscription address with its token, so it is written with owner-only permissions: `0600` on Unix, and an owner-only ACL on Windows.

## 10. Secrets

Subscription addresses and `Authorization` header values are stored in the platform's secret store:

| Platform | Store | Encrypted |
|---|---|---|
| Windows | DPAPI, in `dpapi-secrets.json` in the data folder | Yes, bound to your Windows user |
| macOS | Keychain | Yes |
| Linux | Secret Service (GNOME Keyring, KWallet and similar) | Yes, when a keyring is running |

If the platform store is not available, the app falls back to `secrets.json` in the data folder. **The fallback is not encrypted.** It is written with owner-only permissions, but anyone who can read your files can read the subscription addresses. The app records the fallback reason in its snapshot so the Settings screen can show it. On Linux, a machine with no running keyring uses the fallback.

## 11. Corrupt or unreadable state files

Each state file records a schema version. The app handles a bad file like this:

- **Cannot be parsed (corrupt).** The file is moved to the same folder with `.corrupt-<unix time>` added to its name. It is kept, not deleted. The app starts from defaults and shows a warning that names the moved file. Your data is still on disk; you can restore it by hand.
- **Written by a newer build.** The file is left as it is. The app runs from defaults for that file and does not save over it, so your newer data is not lost when you go back to this version.
- **Written by an older build.** The app migrates it on load. If no migration step can handle it, the file is treated like a newer one: left alone and not overwritten.

Because a corrupt user-settings file falls back to defaults, the first save after the warning writes a fresh file. If you want the old settings back, restore the `.corrupt-` copy before you change anything.
