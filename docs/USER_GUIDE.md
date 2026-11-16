# Starpod User Guide

This guide covers the desktop app: installing the engine, adding subscriptions, choosing servers, writing rules, and where your data lives. For building the app, see [DEVELOPMENT.md](DEVELOPMENT.md).

Screen and button names may change before the first release. The behavior described here is what the code does today.

## 1. First run and the engine

Starpod does not bundle the proxy engine. It downloads the engine the first time you install it, from the Settings or Dashboard screen.

- **Source.** `https://github.com/meow-rs/meow-rs/releases/download/v0.29.0/` and the matching `.sha256` file from the same release. The asset is chosen for your platform: `x86_64-pc-windows-msvc` (zip), `aarch64-apple-darwin` (tar.gz), `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu` (tar.gz).
- **Check.** The app computes the SHA-256 of the downloaded archive and compares it with the hash in the `.sha256` file from the same release. A mismatch deletes the download and installs nothing.
- **What this does not check.** The release is not signed, and the app does not verify a signature. The check catches corruption and a mismatched download. It does not catch a compromised release, because the attacker could publish a matching hash too.
- **Where it goes.** `<data folder>/engine/0.29.0/`, where the data folder is the one in section 9 (on Windows, `%LOCALAPPDATA%\Starpod\engine\0.29.0\`). On Windows the archive's `wintun.dll` is copied next to the engine. An engine installed by an earlier build under a lowercase `starpod` folder is not found; install it again.

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

Removing a subscription deletes its cached payload and its stored address and header. Your own nodes, groups and rules are not affected. If the removal leaves no subscription with servers while the proxy is running, the proxy is stopped (section 2.3).

### 2.3 Connecting needs a subscription with servers

The proxy starts only when at least one subscription supplies servers. Without one, the generated configuration has no servers and every connection would go direct, so the app refuses to connect. A subscription counts this way:

- **Engine-managed:** it counts. The engine holds its servers.
- **App-managed:** it counts once a refresh has succeeded and the payload has at least one server. A subscription that has never fetched successfully does not count.
- **Your own nodes:** they do not count on their own. You still need a subscription.

While the rule blocks Connect, the Dashboard says why and offers a way to fix it:

- "Add a subscription first", when there are no subscriptions. The card's button opens the Servers screen.
- "Refresh your subscription — it has no servers yet", when subscriptions exist but none is usable.

Connect, Restart and the tray's Connect item stay disabled while blocked. Connect is also blocked while the engine is not installed. Disconnect is never blocked.

If the proxy is running and a change leaves no usable subscription (removing the last one, clearing the cache, or a refresh that returns no servers), the app stops the proxy and shows a notice saying why. It does not reload the proxy with no servers. Add or refresh a subscription, then connect again.

A proxy that is already running when the app starts with no usable subscription (one an earlier run left running) is left running, so you can Disconnect it. Restart is refused until a subscription has servers.

### 2.4 What a provider can send

An app-managed subscription's response headers are read on each refresh. Header names match regardless of case. A header that is absent, empty or unreadable is treated as not sent.

| Header | What the app does |
|---|---|
| `subscription-userinfo` | Shows the traffic used, the plan total and the expiry date. See section 2.1 |
| `profile-title` | Names the subscription. Falls back to the file name in `content-disposition`, then to your label |
| `content-disposition` | When there is no `profile-title`, its file name (`filename*` is decoded first, then `filename`; a `.yaml`, `.yml` or `.txt` extension is dropped) names the subscription |
| `profile-web-page-url` | The "web page" link on the Home page. Only `http` and `https` addresses are kept |
| `profile-web-page-name` | The label of that web page link. Without it the address is shown |
| `profile-logo` | The subscription's logo on the Home page. Downloaded once per refresh, at most 512 KiB, within 10 seconds. PNG, JPEG and SVG are accepted. A logo that fails to download leaves the previous one in place |
| `announce` | The announcement shown on the Home page. The two characters `\n` become a line break |
| `announce-url` | A link shown with the announcement. Only `http` and `https` |
| `support-url` | The support link, shown with the announcement and with a device-limit notice. Only `http` and `https` |
| `profile-update-interval` | The refresh interval, in whole hours, when you have not set your own (section 4, P7) |
| `global-mode` | `false` disables the Global routing mode while the subscription is active. A Global choice then runs as Rule, and the Global option is unavailable |
| `expand-proxy-groups` | Stored. `false` means proxy groups start collapsed |
| `x-hwid-limit`, `x-hwid-max-devices-reached` | `true` means this device has no slot left. The refresh fails with "This subscription has reached its device limit. Contact support to free a slot." The last working payload stays in use, and the support link is shown |
| `Content-Type: text/html` or `text/xml` | The refresh fails with "The server returned a web page, not a subscription", because a landing or login page is not a subscription |

**Device identification.** Each fetch sends these headers, so a provider can tell devices apart:

- `x-hwid`: a stable, anonymous identifier. It is the first 32 hex characters of a SHA-256 of this computer's machine id (Windows `MachineGuid`, Linux `/etc/machine-id`, macOS `IOPlatformUUID`). The machine id itself is never sent. Where no machine id can be read, the app generates a random identifier once and keeps it in your data folder.
- `x-device-os`: `Windows`, `Linux` or `macOS`.
- `x-ver-os`: the OS version, best effort (on Windows the build number, as `10.0.<build>`).
- `x-device-model`: the device model, best effort. The CPU architecture when the model cannot be read.

The app also sends its own `User-Agent` (`starpod/<version>`) on every fetch.

To stop the device headers, turn off **Send device identification** in Settings. With it off, none of the four `x-` headers is sent. A provider that limits devices may then refuse the subscription.

## 3. Servers and node selection

The Home and Proxies screens are where you choose servers. The rules, the routing mode and the system proxy are not on Home.

**Home** shows the profile card, the announcements, the connect button and one **Server** drop-down. The drop-down picks a server of the primary group: the group that unmatched traffic goes to (see section 4). Its default is the group's automatic member, the url-test, fallback or load-balance member named "Auto" when there is one, otherwise the first automatic member. The drop-down is hidden when there is no primary group, and it is disabled when the primary group chooses its server by itself. **All servers** opens the Proxies screen.

**Proxies** holds everything about servers and routing:

- **Routing mode.** Rule, Global or Direct. Global is missing when the profile does not allow it.
- **System proxy** and **TUN**, the same switches the Settings screen has (see section 6.2). While connected, the row also shows the session upload and download.
- **Groups.** Each group is a card that opens to its servers. Groups start closed; the ones you open stay open the next time you start Starpod. Choose a server in a select group by clicking its tile; automatic groups choose their own server. Use the gauge button to test a group's latency, and **Test latency** in the header to test every server.
- **Filter, sort and hide.** The filter searches server names and labels. Sort by default order, latency or name, and hide unavailable servers.

The automatic default is only a starting point. Choosing a server on Home or on Proxies saves your choice for that group, and a later refresh or rebuild keeps it. Nothing is written for the default itself, so a subscription change that alters the group chooses again.

**Servers from your own list and from subscriptions.** The Proxies screen lists the nodes from your own list and from app-managed subscriptions.

- **Select a node.** While the proxy is running, the selection is applied live to the engine. While it is stopped, the selection is only saved and applies on the next start.
- **Selection survives refreshes.** The selected node is remembered by its internal id, not its name. If the provider renames a node, your selection stays on it.
- **Latency test.** While connected, tests each node by asking the engine to measure a request to `https://www.gstatic.com/generate_204` with a 5-second timeout. While not connected, it pings each server directly instead: the time to open a TCP connection to its address and port. That shows the server is reachable, not that the proxy works, and servers on UDP-only protocols (Hysteria, TUIC, WireGuard) stay untested. Results are kept until the proxy stops. The probe uses the engine's `/proxies/{name}/delay` endpoint, which follows the mihomo convention; it has not been checked against a live engine in this repository.

## 4. Configuration precedence

The running configuration is built from your settings and the subscriptions. When the two disagree, these rules decide:

| # | What | Rule |
|---|---|---|
| P1 | Ports | Your setting, then the subscription's, then the default |
| P1a | Allow-LAN, bind address | Your setting, then the safe default (off, loopback). A subscription is never consulted. |
| P2 | Mode, log level, TUN | Your setting, then the subscription's, then the default |
| P3 | Nodes | The union of subscription nodes and your nodes. On a name clash, yours wins |
| P4 | Proxy groups | Per group name. Your group replaces a subscription group of the same name |
| P5 | Rules | Your rules first, in your order. Then subscription rules. Then the `MATCH` tail, whose target is your selected group, else the first `MATCH` rule's target, else `PROXY` |
| P6 | Selected node | Your selection, by node id. Then the subscription's default |
| P7 | Refresh interval | Your automatic-refresh setting. Then the subscription's `profile-update-interval`. Then 60 minutes. `0` in your setting means manual only |

Two guarantees follow from this:

- **A refresh never changes your settings.** A refresh writes only the subscription's own metadata and cached payload. It has no path to your settings file.
- **A failed build changes nothing.** If the configuration does not validate, nothing is applied, the running proxy is untouched, and the error is shown.

Allow-LAN and the bind address are never taken from a subscription. See section 6.

**Warnings.** A build can succeed and still have things to look at, such as a geo rule that needs a database. These appear as a warning card on the Dashboard, listed under the current configuration. The card is shown only when there are warnings, and it does not stop the proxy. The warnings come from the last build that succeeded; a build that fails shows its error instead.

## 5. Rules

Rules decide where each connection goes. The engine checks them from top to bottom and the first match wins. The order is:

1. Your rules, in the order you set.
2. Subscription rules from app-managed subscriptions.
3. `MATCH`, the catch-all. The app always places it last. Its target is chosen in this order:
   - the primary group: the group the Home page's Server drop-down shows, when that group exists;
   - otherwise the target of the first `MATCH` rule, whether it comes from a subscription or from you, when that names an existing group or a built-in outbound such as `DIRECT`;
   - otherwise the app's `PROXY` group.

   So a subscription that ends with `MATCH,<its group>` sends unmatched traffic to that group, and the Home page shows that group first. Any rules written after the first `MATCH` are removed, and the build reports it.

### 5.1 Two editors

- **Structured editor.** One rule per row: kind, payload, target, and flags. Changes are validated before they are saved.
- **Disabling a rule.** Each of your rules has a Disable or Enable button. A disabled rule stays in your list, shown dimmed with a `disabled` badge, and takes no part in routing. Enable it again to bring it back in its place. Subscription rules have no toggle; they are read-only.
- **Raw text.** One engine rule per line, in the engine's own syntax. Blank lines and lines starting with `#` are ignored. If any line is invalid, nothing is saved, and every bad line is reported with its line number.

Raw text is parsed into the same rule model the structured editor edits, so a rule entered either way is stored the same way.

### 5.2 Rule kinds

The meow grammar has 31 rule tokens (`crates/starpod-core/src/rules/grammar.rs`). The editor offers 29 of them. The app's list of kinds is the one the editor shows; the two left out are explained below.

| Group | Kinds |
|---|---|
| Domain | `DOMAIN`, `DOMAIN-SUFFIX`, `DOMAIN-KEYWORD`, `DOMAIN-REGEX`, `DOMAIN-WILDCARD` |
| Geo | `GEOSITE`, `GEOIP`, `SRC-GEOIP`, `IP-ASN`, `SRC-IP-ASN` |
| IP | `IP-CIDR`, `SRC-IP-CIDR`, `IP-SUFFIX`, `SRC-IP-SUFFIX` |
| Port and network | `SRC-PORT`, `DST-PORT`, `IN-PORT`, `NETWORK`, `DSCP` |
| Process and user | `PROCESS-NAME`, `PROCESS-PATH`, `UID` |
| Inbound | `IN-NAME`, `IN-TYPE`, `IN-USER` |
| Logic | `AND`, `OR`, `NOT` |
| Catch-all | `MATCH` (written by the app, not by you) |

Not offered: `SUB-RULE`. It names a named rule block, and the app defines none, so it can never validate. A raw line with one is refused when you apply it. `RULE-SET` is accepted. It names a rule provider, which a subscription can declare. The rule validates only when some profile declares a provider with that name; otherwise the error says which name is missing.

Things to know:

- **Targets** are a proxy group or node name, or a built-in outbound: `DIRECT`, `REJECT`, `REJECT-DROP`, `COMPATIBLE`, `PASS`, `PASS-RULE`. A target that names nothing in the candidate configuration fails the build, and the error names the rule.
- **Flags.** `no-resolve` stops the engine resolving a domain to an IP before matching. `src` matches the source address instead of the destination.
- **Logic rules** (`AND`, `OR`, `NOT`) take their inner rules without a target. The enclosing rule owns the target.
- **Geo rules.** `GEOIP`, `SRC-GEOIP`, `IP-ASN` and `SRC-IP-ASN` make the app show a warning. The engine downloads its geo database the first time it needs it, so the first match can be slow. `GEOSITE` needs no database check; if the database is absent, the rule simply never matches.

## 6. Settings

Settings opens on a list of its sections (General, Network, TUN, DNS and the rest), each with an icon and a line on what it holds. Pick one to open it; the arrow at the top goes back to the list. The menu button in the top bar still opens the app's navigation from anywhere. In a narrow window, a setting's drop-down or text field sits under its name; switches stay on the right.

| Setting | Default | Notes |
|---|---|---|
| Language | System | Settings > General > Language. System follows your Windows or Linux language: Russian when it is Russian, English otherwise. English and Русский pin the language; the tray follows it too |
| Mixed port (HTTP and SOCKS) | 7890 | Used when you leave it unset |
| SOCKS port | 7891 | Used when you leave it unset |
| Port | unset | Optional |
| Allow LAN | Off | See below |
| Bind address | unset | Passed to the engine only when you set it |
| Mode | Rule | Rule, Global or Direct. Global sends everything through the selected node. Direct sends everything directly |
| Log level | Info | Silent, Error, Warning, Info or Debug |
| TUN | Off | See below |
| System proxy | Off | Points the operating system's proxy at the running engine. See 6.2 |
| Proxy mode | Manual | Manual sets a fixed server. PAC serves a script instead. See 6.2 |
| Bypass list | Local and private networks | Hosts the system proxy is skipped for |
| PAC script | Private hosts direct, the rest through the engine | Used in PAC mode |
| Start at login | Off | Starts Starpod when you log in. See 6.2 |
| Silent start | Off | With Start at login, Starpod starts in the tray without its window |
| Connect at start | Off | Connects once your saved state has loaded, when a subscription has servers |

**Allow LAN is off by default, and why.** When it is on, other machines on your network can use the proxy ports. You did not ask for that, so the default is off.

**A subscription cannot turn Allow LAN on.** Whether the proxy is reachable from your network is your decision alone. A subscription's `allow-lan` and `bind-address` values are ignored, and the default stays off until you change it.

**TUN MTU** is the largest packet size the adapter carries, from 1280 to 9000 bytes, or 0 for the engine's default. The floor is 1280 because that is the smallest MTU IPv6 allows, and TUN carries IPv6 traffic as well as IPv4. A smaller value is refused.

**TUN** sends all system traffic through a virtual network interface. It needs administrator rights on Windows, root on macOS, and root or `CAP_NET_ADMIN` on Linux. The app does not start the engine with those rights. Turning TUN on sends the setting to the running engine, which works only if the engine already has the privileges. The elevation helper described in `docs/research/ARCHITECTURE.md` section 4.5 is not implemented. Treat TUN as not yet usable.

### 6.1 Engine settings

Every setting below is optional. Leave it unset and the engine's own default applies, and nothing is written to the generated configuration. A value you set wins over the subscription's; the DNS block is the one exception, described below.

| Setting | Default | Notes |
|---|---|---|
| TUN stack | Engine default | `gvisor`, `mixed` or `system`. Applies while TUN is on |
| TUN strict route | Engine default | Drops unroutable traffic instead of letting it leak. Applies while TUN is on |
| TUN auto-redirect | Engine default | Linux only. Installs firewall redirects for TUN |
| TUN auto-detect interface | Engine default | Lets the engine find the outbound interface itself |
| TUN endpoint-independent NAT | Engine default | Keeps UDP NAT mappings per destination |
| TUN route addresses | Engine default | CIDRs routed through TUN, one per line |
| TUN excluded addresses | None | CIDRs kept off TUN, one per line, such as your LAN |
| TUN DNS hijack | Engine default | Targets as `host:port`, such as `any:53`. Validated on save |
| IPv6 | Engine default | On or off. Off stops the engine handling IPv6 |
| Unified delay | Engine default | Measures connection latency the same way for every protocol |
| TCP concurrent | Engine default | Races several addresses for a TCP connection at once |
| Find process mode | Engine default | `off`, `strict` or `always`. How the process behind a connection is looked up |
| Keep-alive interval | Engine default | Seconds, 1 to 86400 |
| DNS | Subscription's | Enable, enhanced mode (`fake-ip` or `redir-host`), fake-IP range (a CIDR), fake-IP filter, nameservers, fallback, default and proxy-server nameservers, nameserver policy (`domain: server, server`), use hosts. Only the settings you set are written |
| Override subscription DNS | Off | When on, your DNS block replaces the subscription's. When off, the subscription's DNS block is used if it has one |
| Hosts | None | Static `domain ip` entries, one per line |
| Sniffer | Engine default | Enable, force DNS mapping, parse pure IP, override destination, sniffed ports per protocol (`HTTP: 80, 8080-8880`; protocols HTTP, TLS and QUIC), force domains and skip domains |
| Geodata mode | Engine default | `dat` or `mmdb` |
| Geodata sources | Engine default | URLs for GeoIP, geosite, the MMDB and the ASN database. Use `https://` |
| Geodata auto-update | Engine default | The engine refreshes geodata in the background, every 1 to 8760 hours |
| Update geodata now | Not run | Downloads the geodata databases and reloads the engine. The last refresh time is shown |
| Latency test URL | `https://www.gstatic.com/generate_204` | The address each latency test measures against. Must be `http://` or `https://` |
| Latency test timeout | 5000 ms | Between 100 and 60000 ms |
| Test group latency | Not run | Measures every member of a proxy group at once and fills in the node latencies |
| Engine channel | Stable | `stable` for tagged releases, or `alpha` for the rolling builds |
| Check for engine update | Not run | Compares the installed engine with the newest one on the chosen channel |
| Upgrade engine | Not run | Downloads and checks the newest engine on the channel, then restarts the proxy on it if it was running |
| Log lines kept | 1000 | Engine log lines the Logs page keeps, from 100 to 2000 |

**Engine updates are checked, not automatic.** An upgrade replaces the engine binary after its checksum matches the release. A running proxy stops for the install and starts again on the new engine. If the install fails, the proxy restarts on the old one.

**Geodata files are written next to the generated configuration.** The engine's home directory is that same folder, so the engine reads the files the app downloads.

### 6.2 System proxy and startup

**System proxy** makes other programs use the engine without each being configured. It is set only while the engine runs. Turning it on while the engine is stopped does nothing until you connect. Disconnecting, quitting the app, or the engine stopping on its own restores your previous settings.

- **Manual** sets the HTTP, HTTPS and SOCKS servers to `127.0.0.1` and the mixed port.
- **PAC** tells the system to fetch a script from `127.0.0.1` on a port Starpod chooses. The script decides per host. The default sends private and loopback hosts direct and everything else through the engine, falling back to direct if the engine does not answer. The script must define `FindProxyForURL`, and `%mixed-port%` is replaced with the engine's port.

Starpod saves your previous proxy settings before it changes them. If Starpod crashes or is killed while the proxy is set, the next start restores those settings. Until then, other programs may fail to reach the internet, so start Starpod again or turn the system proxy setting back to what you want.

Where it works: **Windows** (the per-user proxy settings), **macOS** (every enabled network service, through `networksetup`), and **Linux** on GNOME (`gsettings`) or KDE (`kwriteconfig5` or `kwriteconfig6`). A desktop with none of these reports that the setting cannot be applied. Some applications read the proxy only at start, so they need a restart to use it.

**Start at login** adds Starpod to your user account's start-up list: a Run entry on Windows, an autostart desktop entry on Linux, or a LaunchAgent on macOS. Nothing is installed for other users. Turning it off removes the entry. The entry is rewritten at each start, so it follows the app if you move it.

**Silent start** applies only to a login start. The window stays hidden, and the tray icon is the way back in. If the desktop has no tray, the window opens anyway, since it is the only way to reach the app.

**Connect at start** connects once your saved state has loaded. It does nothing when no subscription supplies servers or the engine is not installed.

## 7. Logs

The Logs screen has two streams:

- **App events.** Actions, refreshes and errors, kept as the last 500 lines.
- **Engine output.** The engine's standard output and error, held in a bounded buffer. The subscription address, the header value and the controller secret are removed before a line is stored.

Secrets never appear in an error message either. The app scrubs every error text against the secrets it has read.

## 8. Tray and window

- **Navigation.** The page list is in a drawer, closed by default. The menu button at the top left opens it over the page; choosing a page, the close button, or a click outside it closes it. The top bar always shows whether you are connected.
- **One subscription at a time.** Only the subscription marked **In use** on the Profiles page supplies servers, rules and DNS, and only its announcements are shown. **Use** on another one switches to it, and a running proxy is reloaded onto it. A subscription you add is put in use once its first refresh has servers.

### 8.0 Updating Starpod

Starpod checks GitHub for a newer release at start (Settings > General, "Check for updates at startup") or when you press **Check for updates**. When one is found, Settings shows **Install update**, and the drawer shows the version.

**Install update** downloads this platform's archive from the release, checks it against the release's `.sha256` file, and replaces the running Starpod with it. Starpod then disconnects, restores your system proxy, quits and starts the new version. The previous binary is kept beside the new one as `starpod.exe.old` (`starpod.old` elsewhere) until the next start. A download that does not match its checksum installs nothing. The release is not signed, so the check catches a damaged download, not a compromised release.

The releases must be readable without signing in to GitHub: while the repository is private, the check and the install report that GitHub answered 404.


- **Tray available.** Closing the window hides it to the system tray. The proxy keeps running. Settings > General > Close to tray turns this off, so that closing the window quits instead. Quit in the tray menu stops the engine gracefully and exits. The engine gets a stop signal and 10 seconds to exit before it is killed.
- **Tray not available.** Some Linux desktops have no tray host. Closing the window then stops the engine and exits. The app says so once at startup.

The app starts itself at login only when you turn on **Start at login** (see 6.2).

### 8.1 One-click import

A subscription can be added from a link. Open a link of this shape in a browser or a shell:

```text
starpod://install-config?url=<percent-encoded https address>&name=<optional name>
```

For example, on Windows: `start starpod://install-config?url=https%3A%2F%2Fexample.com%2Fsub`

- The `url` must be an http or https address. Encode it so that its own `?`, `&` and `=` do not end the parameter. A literal `+` in the address must be sent as `%2B`, because a `+` in a link is read as a space.
- The `name` is optional. When it is missing, the subscription is named after the address's host.
- The links `clash://`, `mihomo://` and `koala-clash://` are also read when they are passed to Starpod on the command line. Starpod does not register them with the system, so they do not open Starpod from a browser.
- Starpod asks before it adds anything. The window comes to the front with an "Add subscription?" dialog that names only the host of the address. The address itself is not shown, because it usually carries a private token. Choose **Add** to add the subscription and fetch it, or **Cancel** to do nothing.
- If a subscription with the same address already exists, Add fetches it again instead of adding a copy.
- If the link is wrong, the window shows a banner that says why, and nothing is added.
- Only one Starpod runs at a time. A second launch, including one from a link, hands its request to the running app and exits.
- On Windows and Linux, Starpod registers the `starpod://` scheme for your user each time it starts. On Linux this writes `starpod.desktop` under `~/.local/share/applications` and makes it the handler for `starpod://`. macOS needs an app bundle to register a scheme, so links do not open Starpod there yet.

## 9. Where your data is

The paths follow each platform's own conventions. The table is taken from the `dirs` crate's mapping, which is what `starpod-store` uses.

| | Windows | macOS | Linux |
|---|---|---|---|
| Settings and rules (`user-state.json`) | `%APPDATA%\Starpod` | `~/Library/Application Support/Starpod` | `~/.config/Starpod` |
| Subscription metadata (`subscriptions.json`), generated config (`runtime-config.yaml`), fallback secrets, controller files (`controllers/`) | `%LOCALAPPDATA%\Starpod` | `~/Library/Application Support/Starpod` | `~/.local/share/Starpod` |
| Cached subscription payloads (`subscriptions/`) | `%LOCALAPPDATA%\Starpod` | `~/Library/Caches/Starpod` | `~/.cache/Starpod` |
| Engine binary | `%LOCALAPPDATA%\Starpod\engine\0.29.0` | `~/Library/Application Support/Starpod/engine/0.29.0` | `~/.local/share/Starpod/engine/0.29.0` |

Notes on this table:

- On macOS the settings and the data share one folder. They are still separate files, so a subscription refresh still cannot touch `user-state.json`. The separation is by file, not by folder, on that platform.
- On Windows the cache shares the data folder. Its files are in a `subscriptions` subfolder, which does not collide with `subscriptions.json`.
- The engine sits in the data folder, under `engine/`, like every other file the app writes. Earlier builds used a lowercase `starpod` folder for it; on Linux that is a different folder, and the engine there must be installed again.

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
