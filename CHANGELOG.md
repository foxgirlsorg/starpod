# Changelog

All notable changes to Starpod are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.0] - 2026-11-17

### Changed

- The engine follows its newest release. A first install takes the newest
  engine on the chosen channel, and at start Starpod installs a newer one when
  it is out, reconnecting if the proxy was on. When the release list cannot be
  read, a first install falls back to meow 0.29.0.
- The Android APK packs the newest stable engine at build time, and reports
  that version.

## [1.0.0] - 2026-11-17

### Added

- **Android app** (`org.foxgirls.starpod`, Android 8+, arm64 and x86_64). It
  connects through Android's VPN service, so every app's traffic goes through
  the proxy. Android asks for the VPN permission on the first connect, and
  again if it has been taken away. The engine ships inside the APK. Targets
  Android 15 (API 35).
- An APK installed directly updates itself over the air; Android asks you to
  confirm each update.
- A bottom dock on phones: Home, Profiles, Proxies, Settings and More, which
  holds Connections, Rules and Logs.
- Home backgrounds: matrix, terrain, map, dots and Eru, in the accent color.
  Matrix and terrain are animated.
- A configurable accent color, defaulting to `#de6161`.
- Add a profile from a pasted config, and edit any profile's content.
- **Restart engine** in Settings > Engine.
- An About card with the version and a link to GitHub.
- Installers next to the portable archives, for Windows, Linux and macOS.

### Changed

- Every page fits a phone-width window: 16px margins, page header buttons on
  the title row, the Proxies routing card and server sort on one row each, and
  narrow layouts for Rules and Connections.
- The connect button animates while connecting or disconnecting, from the
  moment it is pressed.
- A change that leaves the engine's configuration as it is no longer reloads or
  restarts the engine, so changing the theme or other display settings is
  instant.
- Copies installed by a store or package manager (WinGet, Scoop, Homebrew,
  Flatpak, Snap, F-Droid, Obtainium, Google Play and others) leave updates to
  it, and hide Starpod's own updater.
- Tapping the tab of the page you are on takes it back to its start.
- Every server is tested once when a profile is added or put in use.
- Log lines scroll sideways instead of being cut off.
- The interface and documentation use American English.

### Fixed

- The Windows system proxy is set through WinINet, so it takes effect.
- Select fields have a solid background over Home's picture.

## [0.0.3] - 2026-11-17

### Added

- Over-the-air updates from GitHub Releases: **Install update** downloads this
  platform's build, checks it against the release's checksum, swaps it in and
  restarts, with download progress.
- The latency test works while disconnected, by timing a TCP connection to
  each server.
- The app icon and in-app mark are the new red Starpod mark.

### Changed

- One subscription is in use at a time, and only its announcements show.
- The window is portrait. Navigation is a drawer behind the menu button.
- Home pins the profile card and news to the top and the server picker to the
  bottom, with the connect button centered between them.
- Proxies: the routing mode and switches share one row; groups start collapsed
  and remember the ones you open; Expand all and Collapse all are gone.
- Settings opens on a list of its sections with icons; each section has a back
  arrow. Wide controls sit under their label in a narrow window.
- Buttons no longer show a focus ring.
- Release notes are written by the release workflow from the commit messages.

### Fixed

- Traffic in app-managed mode: DNS entries now name their proxy the way the
  engine expects.
- The cached subscription is read again on start.
- Adding a subscription no longer stops the running proxy.
- The macOS CI build runs on a supported runner.

## [0.0.2] - 2026-11-14

### Changed

- Home is now the profile card, announcements, the connect button and one
  **Server** drop-down for the primary group, with an **All servers** link to
  the Proxies page. The routing mode, the system proxy and TUN switches, the
  session traffic, the group picker and the latency test moved from Home to the
  Proxies page.
- A primary group that the user has not chosen a server for starts on its
  automatic member: the url-test, fallback or load-balance member named "Auto",
  or the first automatic member when none has that name. The default is not
  saved; a choice made by the user always wins.

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
