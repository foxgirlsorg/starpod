<div align="center">

<img src="crates/starpod-ui/ui/assets/app-icon.png" width="120" alt="Starpod logo" />

<h1>STARPOD</h1>

**A cross-platform desktop proxy client for the meow-rs engine.**

[![Website](https://img.shields.io/badge/Website-foxgirls.org-de6161?style=flat-square)](https://foxgirls.org)
[![Release](https://img.shields.io/badge/Release-0.0.3-de6161?style=flat-square)](https://github.com/foxgirlsorg/starpod/releases)
[![Rust](https://img.shields.io/badge/Rust-1.92-de6161?style=flat-square&logo=rust&logoColor=white)](rust-toolchain.toml)
[![License](https://img.shields.io/badge/License-MIT-de6161?style=flat-square)](LICENSE)

</div>

---

Starpod fetches your proxy subscriptions, merges them with your own servers and rules, checks the result with the engine, and runs the engine as a supervised child process. You choose a server, connect, and it stays out of the way.

The engine is [meow-rs](https://github.com/meow-rs/meow-rs) v0.29.0. It is downloaded from its release page the first time you install it, so Starpod does not bundle it. The interface is built with [Slint](https://slint.dev) and follows the visual identity of [foxgirls.org](https://foxgirls.org).

## Features

- **Subscriptions by URL**, with an optional `Authorization` header. Addresses and headers are kept in the OS secret store, not in the state files.
- **Two modes per subscription**: app-managed (Starpod fetches, parses, caches and merges) or engine-managed (the engine fetches and refreshes).
- **Clash/Mihomo YAML bodies**, plain or base64-wrapped.
- **Last-good cache**: a failed refresh keeps the previous payload and records the error.
- **Traffic and expiry** from the `subscription-userinfo` header, for app-managed subscriptions.
- **Predictable precedence**: your settings win over subscription data, and a refresh never changes them.
- **Rules** with a structured editor and a raw-text mode, covering 31 engine rule tokens.
- **Two-layer validation**: Starpod's own checks, then the engine's `-t` parser. A rejected configuration never reaches the running engine.
- **Verified engine install**: the download is checked against the release's SHA-256 file.
- **Runtime control**: start, stop, reload, live node selection and per-node latency tests through the engine's REST controller.
- **Safe state handling**: corrupt state files are moved aside, never deleted, and Starpod starts from defaults.
- **Over-the-air updates** from GitHub Releases, checked against the release checksum.

Full behavior, limits and edge cases are in the [user guide](docs/USER_GUIDE.md).

## How it works

1. Add a subscription. Starpod does not fetch it until you refresh, or until its interval passes.
2. Starpod builds the running configuration from your settings and the subscriptions, using the precedence rules in the user guide.
3. The configuration is validated by Starpod and then by the engine. Nothing is applied if either check fails.
4. Connect starts the engine. Choose servers on Home or on Proxies; the choice is saved and survives refreshes.

Connecting needs at least one subscription that supplies servers. Starpod refuses to connect without one, so traffic never goes direct by accident.

## Platforms

| Platform | Target | Status |
| --- | --- | --- |
| Windows | `x86_64` | Built and tested in CI |
| Linux | `x86_64` | Built and tested in CI |
| macOS | `aarch64` (Apple silicon) | Built and tested in CI |

Other targets named in the engine download table (Linux and Windows arm64, macOS x86_64) are not in CI and are not supported.

## Download

Builds are on the [GitHub Releases](https://github.com/foxgirlsorg/starpod/releases) page, as an installer or a portable archive:

| Platform | Installer | Portable |
|---|---|---|
| Windows x86_64 | `starpod-windows-x86_64-setup.exe` | `starpod-windows-x86_64-portable.zip` |
| Linux x86_64 | `starpod-linux-x86_64-setup.deb` | `starpod-linux-x86_64-portable.tar.gz` |
| macOS Apple silicon | `starpod-macos-aarch64-setup.dmg` | `starpod-macos-aarch64-portable.tar.gz` |
| Android (arm64, x86_64) | `starpod-android.apk` | — |

On Android, install the APK (allow installs from your browser or file manager), then install the engine from the Home screen. Starpod connects as a VPN, which carries every app's traffic: Android asks for the VPN permission the first time you connect, and again if it has been taken away.

Each file has a `.sha256` checksum. The Windows installer installs for the current user and needs no administrator rights. The portable archives run from any folder, and the built-in updater keeps them current. Nothing is code-signed yet, so Windows SmartScreen and macOS Gatekeeper warn on first launch. TUN elevation and some platform paths have not been run on real machines. See the [known limitations](docs/DEVELOPMENT.md#5-known-limitations-and-remaining-work).

On first run, install the engine from the Dashboard or Settings screen.

## Build from source

Requirements: Rust 1.92.0. `rust-toolchain.toml` pins it, so rustup selects it automatically. Platform prerequisites are in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

```sh
cargo build --workspace
cargo run -p starpod-app          # runs the `starpod` binary
cargo test --workspace
```

### Docker (build and tests only)

Use this on a host without a Rust toolchain. The GUI cannot run in the container.

```sh
docker compose build
docker compose run --rm dev cargo test --workspace
docker compose run --rm dev cargo clippy --workspace --all-targets -- -D warnings
```

To produce an unsigned Windows binary without a host toolchain:

```sh
docker compose run --rm windows   # writes dist/starpod.exe
```

## Workspace

| Crate | Purpose |
| --- | --- |
| `starpod-core` | Domain types, the rule model and grammar, and the pure configuration pipeline. No I/O. |
| `starpod-subscription` | Fetching, body parsing, validation, cache and refresh schedule. |
| `starpod-store` | Per-platform paths, JSON state with quarantine and migrations, atomic writes, secret stores. |
| `starpod-engine` | Engine download and checksum, YAML emitter, `meow -t` validation, process supervisor, REST client. |
| `starpod-app` | UI-free service layer, the `starpod` binary and its Slint glue. |
| `starpod-ui` | Slint components, design tokens, icons, and the bridge between Rust and the UI. |

## Documentation

- [User guide](docs/USER_GUIDE.md): first run, subscriptions, servers, rules, precedence, settings, data locations.
- [Developer guide](docs/DEVELOPMENT.md): architecture, build setup, tests, CI, known limitations.
- [Changelog](CHANGELOG.md): what changed in each release.

## Credits

- Made by [foxgirls.org](https://foxgirls.org).
- The engine is [meow-rs](https://github.com/meow-rs/meow-rs).
- Emoji graphics are [Twemoji](https://github.com/jdecked/twemoji) (CC-BY 4.0), bundled as `crates/starpod-app/assets/twemoji.bin`. See `crates/starpod-app/assets/TWEMOJI-LICENSE.txt`.

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

---

<div align="center">
  <sub>© 2026 <a href="https://foxgirls.org">foxgirls.org</a> &nbsp;·&nbsp; MIT License</sub>
</div>
