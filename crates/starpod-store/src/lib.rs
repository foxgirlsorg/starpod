//! Persistence for Starpod.
//!
//! This crate owns everything that puts bytes on disk, and the handling of
//! secrets. It depends on neither the GUI nor the proxy engine: the engine
//! is a subprocess the app drives, and the GUI is a view over the state this
//! crate persists.
//!
//! # What lives where
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`paths`] | the per-platform config, data and cache directories |
//! | [`state`] | the persisted files, and the corruption/migration handling |
//! | [`migration`] | the schema-version machinery and the migration registry |
//! | [`atomic`] | atomic file replacement (write temp → fsync → rename) |
//! | [`permissions`] | making a file or directory readable only by its owner |
//! | [`secret`] | the `SecretStore` trait, the platform stores, and the fallback |
//! | [`runtime_config`] | writing the generated engine configuration |
//! | [`redact`] | making a secret safe to log or display |
//!
//! # The separation is the guarantee
//!
//! GOAL.md §3 requires that a subscription update never silently overwrites
//! a user-defined setting. This crate enforces that *structurally*: the
//! user's own state lives in one file, the subscription metadata in another,
//! and the cached payloads in a third. There is no code path where a
//! subscription write can reach a preference file.
//!
//! The user's preferences, rules and nodes are in [`state::UserState`]; the
//! subscription metadata is in [`state::SubscriptionMeta`] and holds **no
//! secrets** — only the keys under which the secret store holds the
//! subscription's URL.
//!
//! # Corruption and versioning
//!
//! Every file carries a `schema` number. A file that cannot be parsed is
//! *moved aside* — never deleted, never silently overwritten — and the app
//! starts from the defaults, reporting what happened. A file from a newer
//! build is left exactly as it is. A file from an older build is migrated by
//! a registered step; a step that cannot handle the file leaves the original
//! alone.
//!
//! # Secrets
//!
//! The [`secret::SecretStore`] trait has four operations and is object-safe,
//! so the application can hold a `Box<dyn SecretStore>` and swap the
//! implementation. Each platform has its own store — DPAPI on Windows, the
//! Keychain on macOS, the Secret Service on Linux — and there is a plain-file
//! fallback for machines with none of those. The fallback is **not
//! encrypted**; [`secret::SecretStore::is_encrypted`] reports the difference
//! so the Settings screen can state the weaker guarantee plainly.

pub mod atomic;
pub mod crash;
pub mod error;
pub mod migration;
pub mod paths;
pub mod permissions;
pub mod redact;
pub mod runtime_config;
pub mod secret;
pub mod state;

pub use error::{MigrationError, StoreError};
pub use migration::{FileKind, Migrations};
pub use paths::{APP_DIR_NAME, AppPaths};
pub use runtime_config::{WrittenConfig, write_runtime_config};
pub use secret::{SecretError, SecretStore, SelectedStore, open_secret_store};
pub use state::{LoadOutcome, SecretRef, Store, SubscriptionMeta, UserState};
