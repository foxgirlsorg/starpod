# Starpod Developer Guide

This guide covers how the code is organised, how to build and test it on each platform, how CI runs, and what is still missing. For the design rationale, read `docs/research/ARCHITECTURE.md`. For the engine facts the code depends on, read `docs/research/meow-integration-contract.md`.

## 1. Architecture

### 1.1 The pipeline

A proxy configuration is produced in five steps:

```text
Subscription(s) + User preferences + Rules
  → Normalization   → Merging   → Validation   → Runtime configuration   → Proxy core
```

Each step lives in one place:

| Step | Owner | Entry point |
|---|---|---|
| Fetch, sniff, parse, validate a subscription body | `starpod-subscription` | `pipeline::parse_payload` (pure); `provider` trait for the HTTP GET |
| Cache the last valid payload, schedule refreshes | `starpod-subscription` | `cache`, `refresh`, `schedule::RefreshSchedule` |
| Normalization: unique node and group names, rule targets | `starpod-core` | `config::build` → private `normalize` |
| Merging under precedence P1 to P6 | `starpod-core` | `config::build` → private `merge` |
| App-side validation | `starpod-core` | `config::build` → private `validate` |
| Engine-side validation (`meow -t`) | `starpod-engine` | `validate::validate_config` through `ProxyEngine::validate` |
| Emitting the engine's YAML | `starpod-engine` | `config::EngineDocument` |
| Process lifetime, REST control | `starpod-engine` | `supervisor`, `rest`, `meow` (the `ProxyEngine` implementation) |
| Orchestration: which step runs when | `starpod-app` | `service::AppService` (`commit`, `compose`) |

`starpod-core` does no I/O and starts no processes. That is what lets the merge be tested with plain fixtures. `starpod-app` talks to the engine only through the `ProxyEngine` trait in `starpod-engine`, so tests substitute a fake engine.

Precedence is documented in the user guide, section 4. The code is in `crates/starpod-core/src/config/mod.rs`, and the tests are in `crates/starpod-core/src/config/tests.rs`.

### 1.2 Where state lives

The persisted files are split so that structure prevents a subscription refresh from overwriting a setting:

- `user-state.json` (config directory): preferences, your nodes, groups and rules. Only user actions write it.
- `subscriptions.json` (data directory): subscription metadata. It holds references to secrets, never the secrets.
- `subscriptions/` (cache directory): one cached payload per subscription.

`starpod-store` owns these files. `AppPaths` in `crates/starpod-store/src/paths.rs` decides every path.

### 1.3 The apply sequence

Every change goes through `AppService::commit` in `crates/starpod-app/src/service.rs`:

1. **Writable check.** If a state file was written by a newer or unmigratable build, a change to it is refused with `ReadOnly`.
2. **Compose.** `compose` reads the URLs of engine-managed subscriptions from the secret store, builds `BuildInput`, and calls `starpod_core::build`. Error findings stop here and nothing is applied.
3. **Validate, if running.** `apply_if_running` checks the engine status. If the engine runs, it calls `ProxyEngine::validate`. That writes a candidate file next to the live file, runs `meow -t -f <candidate>`, and deletes the candidate. A rejection stops here and nothing is applied.
4. **Reload, if running.** `ProxyEngine::reload` validates again, writes the live file atomically (`<name>.tmp`, then rename) with owner-only permissions, and sends `PUT /configs {"path": ...}`. If the engine refuses, the previous live file is restored.
5. **Persist.** Changed state files are saved. A save failure is reported, but the new state is already in effect.
6. **Publish.** The new state becomes current and a snapshot is published on the watch channel.

When the engine is not running, `connect` does the same composition and validation, then `ProxyEngine::start`. `start` writes the live file, spawns `meow -f <live file> --secret <secret>` with `MEOW_RUNTIME_DIR` set to `data/controllers`, and reads the controller address from the file the engine writes there.

Two consequences follow from this order. A candidate the engine rejects never touches the running proxy. A failure at step 5 leaves the proxy running on the new configuration, which the error message says.

Note that step 3 and step 4 both run `meow -t`, so a reload runs the engine's parser twice. This is a cost, not a correctness problem.

Other operations: node selection is `PUT /proxies/{group}` on a running engine and is only saved while stopped. TUN is `PATCH /configs` on a running engine. Latency is `GET /proxies/{name}/delay`. Stop is a graceful signal (see section 1.5).

### 1.4 Rust and Slint

- `starpod-ui/ui/app.slint` is the single compilation unit. `starpod-ui/build.rs` compiles it, and the generated types (`AppWindow`, `AppTray`, the `Bridge` global and its structs) are exported from `starpod-ui`.
- `ui/bridge.slint` is the contract. It contains data structs and properties Rust pushes, plus callbacks Rust handles. It contains no logic. The header comment says so, and that rule is the one to keep.
- Rust owns all state. `starpod-app/src/ui/mapping.rs` turns a `AppSnapshot` into the bridge's structs, and nothing else touches Slint types.
- Threads: the tokio runtime runs on its own worker threads and owns the service. The Slint event loop runs on the main thread. Only the main thread touches a `Bridge`, a window or the tray. Tasks running on tokio reach the UI with `slint::invoke_from_event_loop`.
- The service is shared as `SharedAppService = Arc<tokio::sync::Mutex<AppService>>`. Actions lock it, call a method, and return. Views never wait on an action: they read `AppService::subscribe()`, a `watch` receiver of `AppSnapshot`.
- Background work in `starpod-app/src/ui/runtime.rs`: an engine status poll and an auto-refresh loop that checks subscriptions once a minute and refreshes app-managed ones whose interval has passed.

### 1.5 Engine supervision

- The engine is a child process, started with `tokio::process::Command`. Its stdout and stderr are drained into a bounded, redacted log buffer.
- Stop sends `SIGTERM` on Unix and `CTRL_BREAK_EVENT` on Windows, to a process group created for the child. It waits 10 seconds, then kills the child.
- The controller binds to loopback. The secret is passed on the command line (`--secret`); the engine has no other way to receive it. On Unix, other users on the same machine can read it from the process table. This is an engine interface limit, recorded in ARCHITECTURE section 4.2.
- A panic hook in `starpod-app/src/main.rs` stops the engine before the process exits.
- Engine downloads are verified against the release's `.sha256` file. See `starpod-engine/src/download.rs`.

## 2. Building

### 2.1 Requirements

The toolchain is Rust 1.92.0, pinned in `rust-toolchain.toml`. rustup installs it on first use. The components are `rustfmt` and `clippy`.

**Linux** needs these development packages. They are the same as the Dockerfile and CI:

```sh
sudo apt-get install -y --no-install-recommends \
  pkg-config libfontconfig1-dev libfreetype-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
  libgl1-mesa-dev libegl1-mesa-dev libdbus-1-dev
```

`libdbus-1-dev` is for the Secret Service store. The Slint renderer needs the fontconfig, freetype, xkbcommon, X11 and Wayland libraries. The GL and EGL libraries are for the femtovg renderer.

**Windows** needs the MSVC toolchain (`x86_64-pc-windows-msvc`) and the Visual Studio Build Tools with the C++ workload, which provide the linker and the Windows SDK. No other system libraries are required. The DPAPI and Win32 calls go through the `windows` and `windows-sys` crates.

**macOS** (Apple Silicon) needs the Xcode command line tools. The Keychain store uses `security-framework`, which needs no extra packages.

### 2.2 Native commands

```sh
cargo build --workspace
cargo run -p starpod-app          # the `starpod` binary
cargo build --release -p starpod-app
```

`starpod-app` is the only crate with a binary. Its `[[bin]]` is named `starpod`.

### 2.3 Docker

Docker is for building and testing on a machine without a Rust toolchain. The GUI cannot run in the container.

- `Dockerfile` starts from `rust:1.92-bookworm`, installs the Linux packages from section 2.1, installs toolchain 1.92.0, and runs as a non-root user `dev` (uid 1000). Some tests check Unix permissions, and root bypasses those checks.
- `docker-compose.yml` defines one service, `dev`. It mounts the repository at `/work`, mounts `./.scratch/cargo-registry` at the cargo registry path so crate sources can be read on the host, and uses the named volume `starpod-target` for build output, since bind-mounted build directories are slow on Windows.

```sh
docker compose build
docker compose run --rm dev cargo test --workspace
docker compose run --rm dev cargo clippy --workspace --all-targets -- -D warnings
docker compose run --rm dev cargo fmt --all --check
```

If cargo fails with a permission error on the registry directory, the bind-mounted `.scratch/cargo-registry` is not writable by uid 1000 on the host. Create the directory before the first run, or fix ownership once from inside the container:

```sh
docker compose run --rm --user root dev chown -R dev:dev /usr/local/cargo/registry
```

`.scratch/` is listed in `.gitignore`. It holds throwaway probes and the registry cache, and nothing in it is part of the project.

### 2.4 Assets

- `tools/convert_icons.py` and `tools/convert_logo.py` turn Lucide SVG icons and the foxgirls mark into Slint `Path` components. Each takes an input and an output path. Slint has no runtime SVG renderer, so the conversion happens at authoring time. The assets are in `crates/starpod-ui/ui/assets/`.
- The design tokens come from `docs/research/foxgirls-design.md`.

## 3. Testing

### 3.1 Unit tests

```sh
cargo test --workspace
cargo test -p starpod-core
```

Each crate tests its own pure logic:

- `starpod-core`: the precedence rules and validation (`config/tests.rs`), the rule grammar, parser and translation (`rules/`), and the timestamp format.
- `starpod-subscription`: body sniffing and parsing, the quota header, the cache, the refresh schedule and the refresh outcomes. Tests use a mock provider, so there is no network.
- `starpod-store`: state loading with corruption, migration and future-version handling; atomic writes; permissions; redaction; the secret stores and the fallback.
- `starpod-engine`: the YAML emitter, the release asset mapping and checksum handling, the TUN helpers, the REST client, and the candidate validation.
- `starpod-app`: the service's unit tests, and the view-model mapping in `src/ui/mapping.rs`.
- `starpod-ui`: a small number of tests for the generated model defaults. The UI has no automated visual tests.

### 3.2 The central-path integration test

`crates/starpod-app/tests/central_path.rs` runs the whole path through `AppService` with two stand-ins, built by `ServiceDeps`:

- `FakeEngine` implements `ProxyEngine`. It records every call, can reject validations on demand (`reject_validations`), can fail selections (`fail_selects`), and returns scripted latencies (`set_delay`).
- `Scripted` implements `SubscriptionProvider` and returns canned responses, including errors, in order. It also records the requests it received, so tests can check the token is sent.

Temporary directories (`tempfile`) hold the state. The test names describe the behaviour they pin. Examples:

- `connect_validates_then_starts_with_namespaced_nodes_and_user_rules_in_order`
- `a_user_preference_survives_a_refresh_with_a_different_payload`
- `a_failing_refresh_keeps_the_previous_payload_and_still_builds`
- `a_rejected_candidate_does_not_start_the_engine`
- `a_rejected_change_to_a_running_proxy_neither_reloads_nor_saves`
- `a_corrupt_user_state_is_quarantined_and_the_app_starts_with_defaults`
- `the_url_token_never_appears_in_the_snapshot_the_log_or_an_error`

Run it alone with:

```sh
cargo test -p starpod-app --test central_path
```

### 3.3 The real-engine test

One test in the same file runs against the real meow binary:

```rust
#[ignore = "needs the meow engine installed; run with --ignored"]
async fn the_installed_meow_engine_accepts_the_central_fixture()
```

It is skipped by default. To run it:

1. Install the engine from the app (Settings or Dashboard), or make sure `starpod_engine::download::installed_binary_path()` points at a v0.29.0 binary.
2. Run:

```sh
cargo test -p starpod-app --test central_path -- --ignored
```

If the engine is not installed, the test prints `skipped: the meow engine is not installed` and passes. Check the output: a green run without that line means the real engine was exercised.

### 3.4 What the tests do not cover

- The privileged TUN path. CI runners are not elevated, and the elevation helper is not implemented (section 5).
- The Slint window, tray and screens. Nothing renders or clicks the UI in CI.
- Real subscription providers. Tests use fixtures in the Clash/Mihomo shape.
- Real meow behaviour beyond the one ignored test.

## 4. Formatting, linting and CI

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Clippy runs with warnings as errors on all targets, including tests.

`.github/workflows/ci.yml` runs on every pull request and on pushes to `main`:

- **Matrix:** `ubuntu-latest`, `windows-latest`, `macos-14` (Apple Silicon). `fail-fast` is off, so one platform's failure does not cancel the others.
- **Linux only:** the apt packages from section 2.1, and `cargo fmt --all --check`.
- **Every platform:** the Rust toolchain 1.92.0 with `rustfmt` and `clippy` (`dtolnay/rust-toolchain`), the `Swatinem/rust-cache` cache, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.

CI does not run the ignored real-engine test, does not build a release, and does not check the UI visually.

## 5. Known limitations and remaining work

**TUN.** Enabling TUN starts the *engine* elevated (never the GUI): `ShellExecuteExW` with `runas` on Windows, `pkexec` on Linux, `osascript ... with administrator privileges` on macOS (`starpod-engine/src/elevate.rs`). If elevation is refused or fails, the engine starts without TUN and the status says so. Command construction and quoting are unit-tested; the privileged path itself has not been run on any platform. An elevated engine runs the binary from the user-writable data directory, so anything that can write there can run code as administrator/root once TUN is enabled. On Windows, an adopted *elevated* engine cannot be stopped from an unelevated app (TUN is turned off first; the process must be ended by an administrator). Logs of an elevated Windows engine are not captured.

**Adoption of an orphaned engine.** An ownership marker (`engine-owner.json`, owner-only) is written to the app's controller directory. On start, if the controller answers with the marker's secret, the running engine is adopted instead of starting a second one; a stale marker is deleted (`starpod-engine/src/adopt.rs`).

**Windows shutdown.** The engine is spawned with `CREATE_NO_WINDOW`; graceful stop attaches to its console and sends `CTRL_C_EVENT` (tokio's `ctrl_c()` does not fire on `CTRL_BREAK_EVENT`), then `TerminateProcess` after the grace period. Compile-checked; not yet exercised against a real meow on Windows. `meow-integration-contract.md` §7's claim that `ctrl_c` fires on CTRL_BREAK is wrong.

**Engine-managed subscriptions.** Rule order is re-asserted every 30 s after an engine refresh (`GET /rules` → reorder → `POST /rules` only when it differs). Name shadowing of a user node is reported as a warning when the engine reports a different server/port. The response envelopes of `/api/subscriptions`, `/rules` and `/proxies` are not documented in the contract and are parsed defensively (assumptions are commented in `rest.rs`). The Authorization header cannot be sent for engine-managed subscriptions (the engine's declaration has no header field).

**RULE-SET / SUB-RULE.** Not offered and rejected with a clear message: the app supplies no rule providers or sub-rule blocks.

**Latency probe.** `GET /proxies/{name}/delay?url=...&timeout=...` follows the mihomo convention; not checked against a live meow instance.

**Tray on Linux.** Requires a StatusNotifierItem host. Tray availability is judged from creation errors only; a desktop without a host may report success and show nothing.

**Autostart, release packaging, signing.** Not implemented. `docker compose run --rm windows` produces an unsigned `dist/starpod.exe` (x86_64-pc-windows-gnu). The engine download is checked against the release's SHA-256 file only (ARCHITECTURE R3).

**Secret fallback.** On a machine with no platform store, subscription addresses and headers are kept in `secrets.json` without encryption (owner-only file permissions).

**Documentation and code disagreements.**

- `crates/starpod-store/src/paths.rs` documents the Windows cache as `%LOCALAPPDATA%\Starpod\cache`; the code gives `%LOCALAPPDATA%\Starpod\subscriptions`.
- On macOS the config and data directories are the same folder (`dirs` maps both to `~/Library/Application Support/Starpod`); the "never overwrite" guarantee rests on separate files.

**Verification gaps.** No automated visual check of the UI, no privileged test, no test of macOS/Linux release artifacts, and the real-engine test is opt-in.
