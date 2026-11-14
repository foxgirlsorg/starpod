# Changelog

All notable changes to Starpod are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.0.1] - 2026-11-14

First usable build.

### Added

- Subscriptions, managed by the app or by the engine. Supported subscription
  headers: `announce` (a notice shown on the profile card), `profile-title`,
  support and web links, the update interval, the logo, the device limit, and
  the device ID headers.
- One-click import of a subscription from a `starpod://` link.
- Home front page: the active profile card with quota and expiry, the connect
  button with its status line, and the mode and server picker.
- Proxies screen with sorting, hiding of unavailable servers, and latency tests
  for a single server or for a whole group.
- Connections page listing the live connections, with sorting.
- Rules editor with rule providers (`RULE-SET`).
- Logs screen.
- Settings screen: system proxy with a PAC script, TUN, DNS, sniffer, geo data,
  engine channel and upgrade, global hotkeys, autostart, close to tray, and the
  interface language.
- Tray menu.
- Color emoji, rendered with Twemoji.
- English and Russian interface.

### Known limitations

- Builds are not code-signed, so Windows and macOS will warn on first run.
- `starpod://` links are not registered as a URL scheme on macOS.
- The Windows and macOS system paths (system proxy, TUN, autostart) have not
  been field-tested.
