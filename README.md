# Starpod

Starpod is a desktop proxy client for Windows, Linux and macOS. It fetches proxy subscriptions, merges them with your own settings and rules, validates the result with the engine, and runs the engine as a supervised child process. The engine is [meow-rs](https://github.com/meow-rs/meow-rs) v0.29.0, downloaded from its release page. The interface is [Slint](https://slint.dev). The visual identity follows foxgirls.org; the design tokens are in `docs/research/foxgirls-design.md`.

**Status: pre-release.** The domain, subscription, store, engine and service crates are implemented and tested. The Slint window and the `starpod` binary are still being completed. This repository publishes no release binaries and has no signing or packaging yet.

## Features

Implemented in the crates. Limits are listed in the developer guide.

- Subscriptions added by URL, with an optional `Authorization` header. The URL and the header are stored in the OS secret store, not in the state files.
- Two modes per subscription: **app-managed** (the app fetches, parses, caches and merges) and **engine-managed** (the engine fetches and refreshes).
- Subscription bodies in the de facto Clash/Mihomo YAML format, plain or base64-wrapped. No vendor panel API is used.
- A last-good cache. A failed refresh keeps the previous payload and records the error.
- Traffic quota and expiry from the `subscription-userinfo` header, for app-managed subscriptions only.
- A fixed precedence (P1 to P6): your settings win over subscription data, and a refresh never changes them.
- Rules with a structured model and a raw-text mode, covering 31 engine rule tokens.
- Two-layer validation: the app's own checks, then the engine's `-t` parser on a candidate file. A rejected candidate never reaches the running engine.
- Engine installation from the release, checked against its SHA-256 file. Start, stop, reload, live node selection and per-node latency tests over the engine's REST controller.
- Corrupt state files are moved aside, never deleted, and the app starts from defaults.

Not complete or not verified: TUN elevation, adoption of an engine left running after a crash, autostart, release packaging. The full list is in `docs/DEVELOPMENT.md`, section "Known limitations".

## Platform scope

- Linux x86_64, Windows x86_64 and macOS ARM64 (`aarch64-apple-darwin`). CI builds and tests all three on every pull request.
- The engine download table also names Linux arm64, Windows arm64 and macOS x86_64. Those targets are not in CI and are not supported.

## Quick start

### Native build

Requirements: Rust 1.92.0. `rust-toolchain.toml` pins it, so rustup selects it automatically. Platform prerequisites are in `docs/DEVELOPMENT.md`.

```sh
cargo build --workspace
cargo run -p starpod-app          # runs the `starpod` binary
cargo test --workspace
```

### Docker (build and tests only)

Use this on a host without a Rust toolchain. The `dev` service in `docker-compose.yml` builds and tests inside Linux.

```sh
docker compose build
docker compose run --rm dev cargo test --workspace
docker compose run --rm dev cargo clippy --workspace --all-targets -- -D warnings
```

The GUI cannot be launched in the container. To get a Windows binary without a host toolchain, cross-compile it:

```sh
docker compose run --rm windows
```

This writes an unsigned `dist/starpod.exe` (target `x86_64-pc-windows-gnu`). The first build takes about 25 minutes; later ones take a few. On first run, install the engine from the Dashboard or Settings.

## Workspace

| Crate | Purpose |
|---|---|
| `starpod-core` | Domain types, the rule model and grammar, and the pure configuration pipeline (`build`). No I/O. |
| `starpod-subscription` | Fetching, body parsing, validation, cache and refresh schedule, behind a provider trait. |
| `starpod-store` | Per-platform paths, JSON state with quarantine and migrations, atomic writes, secret stores, redaction. |
| `starpod-engine` | Release download and checksum, YAML emitter, `meow -t` validation, process supervisor, REST client, TUN helpers. |
| `starpod-app` | UI-free service layer (`AppService`) and snapshots. Also the `starpod` binary (`src/main.rs`) and its Slint glue (`src/ui/`). |
| `starpod-ui` | Slint components, design tokens, icons, and the `Bridge` contract between Rust and the UI. |

## Documentation

- [User guide](docs/USER_GUIDE.md): first run, subscriptions, servers, rules, precedence, settings, tray, data locations.
- [Developer guide](docs/DEVELOPMENT.md): architecture, build setup, tests, CI, known limitations.
- [Architecture dossier](docs/research/ARCHITECTURE.md) and [engine contract](docs/research/meow-integration-contract.md): design decisions and the engine facts they rest on.
- [GOAL.md](GOAL.md) and [HANDOVER.md](HANDOVER.md): the project brief and the status log.

## Licence

The workspace manifest declares MIT. There is no `LICENSE` file in the repository yet.
