//! The persisted state, split across separate files.
//!
//! The split is the whole point. GOAL.md §3 requires that a subscription
//! update never silently overwrites a user-defined setting, and
//! ARCHITECTURE §3.4 says this is "enforced *structurally*: user preferences
//! are stored in a separate file from subscription-derived data, and the
//! merge reads them separately. There is no code path where a subscription
//! write can touch a preference file."
//!
//! So there are three independent files:
//!
//! | File | Contents | Secret? |
//! |---|---|---|
//! | `user-state.json` | the user's preferences, rules, nodes and groups | no |
//! | `subscriptions.json` | per-subscription metadata | **no** — the URL is a *reference* into the secret store |
//! | `cache/subscriptions/<id>` | the last valid subscription payload | maybe (node passwords) |
//!
//! The subscription metadata deliberately holds no URL and no `Authorization`
//! header. It holds the *key* under which the secret store holds them. That
//! is what lets `subscriptions.json` be world-readable if the user copies it
//! around, and it is why a subscription refresh can never leak a token by
//! writing metadata.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use starpod_core::domain::{Node, ProxyGroup, StoredRule, Subscription, UserPreferences};

use crate::error::{StoreError, io_at};
use crate::migration::{FileKind, Migrations, schema_field, schema_of};
use crate::paths::AppPaths;
use crate::redact;

/// The current schema version of the user-state file.
pub const USER_STATE_SCHEMA: u32 = 1;

/// The current schema version of the subscription-metadata file.
pub const SUBSCRIPTION_META_SCHEMA: u32 = 1;

/// Everything the user has authored.
///
/// This is the file a subscription refresh must never touch.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UserState {
    /// The user's own settings.
    #[serde(default)]
    pub preferences: UserPreferences,
    /// The user's own rules, in the user's own order.
    #[serde(default)]
    pub rules: Vec<StoredRule>,
    /// The user's own nodes.
    #[serde(default)]
    pub nodes: Vec<Node>,
    /// The user's own proxy groups.
    #[serde(default)]
    pub groups: Vec<ProxyGroup>,
    /// The subscription the user has selected, by subscription id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_subscription: Option<String>,
}

/// The metadata of every subscription the user has added.
///
/// No URLs, no headers, no tokens. Each entry refers to the secret store by
/// key; the app resolves the key to the actual URL at fetch time.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubscriptionMeta {
    /// The subscriptions, in the order the user added them.
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
    /// The secret-store key under which each subscription's URL lives.
    ///
    /// Kept beside the metadata (rather than inside it) so that the metadata
    /// file can be inspected without revealing anything.
    #[serde(default)]
    pub url_keys: Vec<SecretRef>,
    /// The secret-store key under which each subscription's optional
    /// `Authorization` header lives.
    #[serde(default)]
    pub header_keys: Vec<SecretRef>,
}

/// A reference to a secret held by the secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    /// Which subscription this secret belongs to.
    pub subscription_id: String,
    /// The key the secret is stored under.
    pub key: String,
}

/// The outcome of loading a state file.
///
/// A load never fails because the file was bad — it *reports*. The file is
/// moved aside and the caller starts from defaults, which is what
/// ARCHITECTURE §3.1 requires ("Parse failure → the file is renamed to
/// `<name>.corrupt-<timestamp>` and the app starts with defaults, surfacing a
/// warning on the Logs screen. Never silently discard; never crash.").
#[derive(Debug, Clone, PartialEq)]
pub enum LoadOutcome<T> {
    /// The file was read and is current.
    Loaded(T),
    /// The file was missing; the defaults were used.
    Missing,
    /// The file could not be parsed and was moved aside.
    ///
    /// The original file is at the path in `quarantine`.
    Corrupt {
        /// Where the unparseable file was moved to.
        quarantine: std::path::PathBuf,
        /// What was wrong with it.
        reason: String,
    },
    /// The file is from a newer build and was left untouched.
    ///
    /// The defaults were used. The file is *not* corrupt — it is simply
    /// newer than this build understands.
    Future {
        /// The schema version the file declares.
        version: u32,
    },
    /// The file could not be upgraded to the current schema.
    ///
    /// The file was left untouched, and the defaults were used.
    Unmigratable {
        /// The schema version the file declares.
        version: u32,
        /// Why the migration failed.
        reason: String,
    },
}

impl<T> LoadOutcome<T> {
    /// The loaded value, if the file was usable.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Loaded(value) => Some(value),
            _ => None,
        }
    }

    /// Whether the load produced a usable value.
    pub fn is_loaded(&self) -> bool {
        matches!(self, Self::Loaded(_))
    }

    /// Whether anything about the load went wrong and should be surfaced to
    /// the user.
    pub fn needs_attention(&self) -> bool {
        !matches!(self, Self::Loaded(_) | Self::Missing)
    }

    /// A one-line description of what happened, for the Logs screen.
    pub fn describe(&self) -> Option<String> {
        match self {
            Self::Loaded(_) | Self::Missing => None,
            Self::Corrupt { quarantine, reason } => Some(format!(
                "the file could not be read ({reason}); it was moved to {} and the defaults were used",
                quarantine.display()
            )),
            Self::Future { version } => Some(format!(
                "the file was written by a newer version of Starpod (schema {version}); it was left untouched and the defaults were used"
            )),
            Self::Unmigratable { version, reason } => Some(format!(
                "the file (schema {version}) could not be upgraded ({reason}); it was left untouched and the defaults were used"
            )),
        }
    }
}

/// Read a state file, handling the missing, corrupt, future and outdated
/// cases.
///
/// The generic here is over the *parsed* value only; the file format is
/// always the same envelope:
///
/// ```json
/// { "schema": 1, ...payload }
/// ```
///
/// The envelope is merged into the payload object, so a file is a flat JSON
/// object whose `schema` key is reserved.
pub fn load_state<T>(
    path: &Path,
    kind: FileKind,
    current_schema: u32,
    migrations: &Migrations,
) -> Result<LoadOutcome<T>, StoreError>
where
    T: for<'de> Deserialize<'de> + Default,
{
    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadOutcome::Missing);
        }
        Err(err) => return Err(io_at(path, err)),
    };

    let document: Value = match serde_json::from_slice(&raw) {
        Ok(document) => document,
        Err(err) => {
            let quarantine = quarantine_corrupt(path)?;
            return Ok(LoadOutcome::Corrupt {
                quarantine,
                reason: redact::redact_error(&err.to_string()),
            });
        }
    };

    if !document.is_object() {
        let quarantine = quarantine_corrupt(path)?;
        return Ok(LoadOutcome::Corrupt {
            quarantine,
            reason: "the top-level value is not a JSON object".to_string(),
        });
    }

    let version = schema_of(&document);

    // A file from a newer build: leave it exactly as it is.
    if version > current_schema {
        return Ok(LoadOutcome::Future { version });
    }

    let document = match migrations.migrate(kind, document, version, current_schema) {
        Ok(document) => document,
        Err(err) => {
            return Ok(LoadOutcome::Unmigratable {
                version,
                reason: err.to_string(),
            });
        }
    };

    match serde_json::from_value(document) {
        Ok(value) => Ok(LoadOutcome::Loaded(value)),
        Err(err) => {
            // The document parsed as JSON but not as this build's type. That
            // is still corruption from this build's point of view, and the
            // file is preserved rather than overwritten.
            let quarantine = quarantine_corrupt(path)?;
            Ok(LoadOutcome::Corrupt {
                quarantine,
                reason: redact::redact_error(&err.to_string()),
            })
        }
    }
}

/// Write a state file atomically, with the schema envelope.
pub fn save_state<T>(
    path: &Path,
    kind_label: &'static str,
    current_schema: u32,
    value: &T,
) -> Result<(), StoreError>
where
    T: Serialize,
{
    let mut document = serde_json::to_value(value).map_err(|err| StoreError::Serialize {
        what: kind_label,
        source: err,
    })?;
    if let Value::Object(map) = &mut document {
        map.extend(schema_field(current_schema));
    }
    let mut bytes = serde_json::to_vec_pretty(&document).map_err(|err| StoreError::Serialize {
        what: kind_label,
        source: err,
    })?;
    bytes.push(b'\n');
    crate::atomic::write_atomic(path, &bytes)
}

/// Move a corrupt file aside, keeping it for the user.
///
/// The name is `<name>.corrupt-<timestamp>`, where the timestamp is a unix
/// timestamp in seconds. The file is *moved*, not copied and not deleted: the
/// user's data is never thrown away.
pub fn quarantine_corrupt(path: &Path) -> Result<std::path::PathBuf, StoreError> {
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_string());
    let quarantine = path.with_file_name(format!("{file_name}.corrupt-{timestamp}"));

    // If a file with that exact name already exists (two corrupt loads in the
    // same second), append a counter rather than overwriting it.
    let mut quarantine = quarantine;
    let mut counter = 1u32;
    while quarantine.exists() {
        quarantine = path.with_file_name(format!("{file_name}.corrupt-{timestamp}-{counter}"));
        counter += 1;
        if counter > 1000 {
            return Err(StoreError::io(
                path,
                std::io::Error::other("could not find a free quarantine file name"),
            ));
        }
    }

    match std::fs::rename(path, &quarantine) {
        Ok(()) => Ok(quarantine),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // The file vanished between the read and the rename — another
            // process removed it. There is nothing to preserve.
            Ok(quarantine)
        }
        Err(err) => Err(io_at(path, err)),
    }
}

/// The high-level store: paths plus the read/write helpers.
///
/// The type is a thin wrapper on purpose. Its job is to make the *file
/// boundaries* explicit at the call site, so that a caller writing
/// subscription data cannot accidentally reach the user-state file.
#[derive(Debug)]
pub struct Store {
    paths: AppPaths,
    migrations: Migrations,
}

impl Store {
    /// Build a store over the given paths.
    pub fn new(paths: AppPaths) -> Self {
        Self {
            paths,
            migrations: Migrations::new(),
        }
    }

    /// Build a store with the default (currently empty) migration registry.
    ///
    /// A file at the current schema version needs no migration, and no
    /// migration exists yet, so the default registry is empty.
    pub fn with_default_migrations(paths: AppPaths) -> Self {
        Self::new(paths)
    }

    /// Register a migration step. See [`Migrations::step`].
    pub fn register_migration(
        &mut self,
        kind: FileKind,
        from: u32,
        step: impl Fn(Value) -> Result<Value, String> + Send + Sync + 'static,
    ) -> &mut Self {
        self.migrations.step(kind, from, step);
        self
    }

    /// The paths this store writes to.
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Load the user's own state.
    pub fn load_user_state(&self) -> Result<LoadOutcome<UserState>, StoreError> {
        load_state(
            &self.paths.user_state_file(),
            FileKind::UserState,
            USER_STATE_SCHEMA,
            &self.migrations,
        )
    }

    /// Save the user's own state.
    pub fn save_user_state(&self, state: &UserState) -> Result<(), StoreError> {
        save_state(
            &self.paths.user_state_file(),
            "user state",
            USER_STATE_SCHEMA,
            state,
        )
    }

    /// Load the subscription metadata.
    pub fn load_subscription_meta(&self) -> Result<LoadOutcome<SubscriptionMeta>, StoreError> {
        load_state(
            &self.paths.subscription_meta_file(),
            FileKind::SubscriptionMeta,
            SUBSCRIPTION_META_SCHEMA,
            &self.migrations,
        )
    }

    /// Save the subscription metadata.
    pub fn save_subscription_meta(&self, meta: &SubscriptionMeta) -> Result<(), StoreError> {
        save_state(
            &self.paths.subscription_meta_file(),
            "subscription list",
            SUBSCRIPTION_META_SCHEMA,
            meta,
        )
    }

    /// Read the cached payload of one subscription.
    ///
    /// A missing cache file is not an error: it means the subscription has
    /// never been fetched successfully, and the caller should fetch it.
    pub fn read_subscription_cache(&self, id: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.paths.subscription_cache_file(id)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(io_at(&path, err)),
        }
    }

    /// Write the cached payload of one subscription.
    pub fn write_subscription_cache(&self, id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.paths.subscription_cache_file(id)?;
        crate::atomic::write_atomic(&path, bytes)
    }

    /// Write the cached logo of one subscription, replacing any earlier logo of
    /// it. Returns the path written; its file name is what the subscription
    /// record keeps.
    pub fn write_subscription_logo(
        &self,
        id: &str,
        extension: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, StoreError> {
        self.remove_subscription_logos(id)?;
        let path = self.paths.subscription_logo_file(id, extension)?;
        crate::atomic::write_atomic(&path, bytes)?;
        Ok(path)
    }

    /// Remove every cached logo of one subscription, whatever its format.
    pub fn remove_subscription_logos(&self, id: &str) -> Result<(), StoreError> {
        for extension in ["png", "jpg", "svg"] {
            let path = self.paths.subscription_logo_file(id, extension)?;
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(io_at(&path, err)),
            }
        }
        Ok(())
    }

    /// Remove the cached payload of one subscription, and its logo.
    pub fn remove_subscription_cache(&self, id: &str) -> Result<(), StoreError> {
        self.remove_subscription_logos(id)?;
        let path = self.paths.subscription_cache_file(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(io_at(&path, err)),
        }
    }

    /// Read the source text of a local profile. `None` means the file is
    /// missing, which the caller reports as a damaged profile.
    pub fn read_profile_source(&self, id: &str) -> Result<Option<String>, StoreError> {
        let path = self.paths.profile_source_file(id)?;
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(io_at(&path, err)),
        }
    }

    /// Write the source text of a local profile, replacing it atomically.
    pub fn write_profile_source(&self, id: &str, text: &str) -> Result<(), StoreError> {
        let path = self.paths.profile_source_file(id)?;
        crate::atomic::write_atomic(&path, text.as_bytes())
    }

    /// Remove the source text of a local profile. A missing file is not an
    /// error.
    pub fn remove_profile_source(&self, id: &str) -> Result<(), StoreError> {
        let path = self.paths.profile_source_file(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(io_at(&path, err)),
        }
    }

    /// The list of subscriptions that have a cached payload.
    pub fn cached_subscription_ids(&self) -> Result<Vec<String>, StoreError> {
        let dir = self.paths.subscription_cache_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(io_at(&dir, err)),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_at(&dir, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_file() {
                ids.push(name);
            }
        }
        ids.sort();
        Ok(ids)
    }
}

/// The file kinds this crate persists.
///
/// Re-exported so that callers do not have to reach into the migration
/// module for the two kinds they are most likely to name.
pub use crate::migration::FileKind as StateFileKind;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("cache"),
        );
        (dir, Store::new(paths))
    }

    #[test]
    fn a_missing_file_yields_the_missing_outcome() {
        let (_dir, store) = store();
        let outcome = store.load_user_state().unwrap();
        assert_eq!(outcome, LoadOutcome::Missing);
        assert!(!outcome.needs_attention());
        assert!(outcome.describe().is_none());
    }

    #[test]
    fn a_saved_user_state_round_trips() {
        let (_dir, store) = store();
        let state = UserState {
            preferences: UserPreferences {
                mixed_port: Some(1234),
                ..UserPreferences::default()
            },
            ..UserState::default()
        };
        store.save_user_state(&state).unwrap();
        let outcome = store.load_user_state().unwrap();
        assert_eq!(outcome, LoadOutcome::Loaded(state));
    }

    #[test]
    fn the_saved_file_carries_the_schema_version() {
        let (_dir, store) = store();
        store.save_user_state(&UserState::default()).unwrap();
        let raw = std::fs::read(store.paths().user_state_file()).unwrap();
        let document: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(document["schema"], json!(USER_STATE_SCHEMA));
    }

    #[test]
    fn the_user_state_and_the_subscription_metadata_are_different_files() {
        let (_dir, store) = store();
        store.save_user_state(&UserState::default()).unwrap();
        store
            .save_subscription_meta(&SubscriptionMeta::default())
            .unwrap();
        assert_ne!(
            store.paths().user_state_file(),
            store.paths().subscription_meta_file()
        );
        assert!(store.paths().user_state_file().exists());
        assert!(store.paths().subscription_meta_file().exists());
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_the_defaults_are_used() {
        let (_dir, store) = store();
        let path = store.paths().user_state_file();
        crate::atomic::write_atomic(&path, b"{ this is not json").unwrap();

        let outcome = store.load_user_state().unwrap();
        match &outcome {
            LoadOutcome::Corrupt { quarantine, reason } => {
                assert!(quarantine.exists(), "the original file was not preserved");
                assert!(
                    quarantine
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .contains(".corrupt-")
                );
                assert!(!reason.is_empty());
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(outcome.needs_attention());
        assert!(outcome.describe().is_some());
        // The corrupt file is gone from its original name...
        assert!(!path.exists());
    }

    #[test]
    fn the_quarantined_file_still_holds_the_original_bytes() {
        let (_dir, store) = store();
        let path = store.paths().user_state_file();
        crate::atomic::write_atomic(&path, b"{ this is not json").unwrap();
        let outcome = store.load_user_state().unwrap();
        match outcome {
            LoadOutcome::Corrupt { quarantine, .. } => {
                assert_eq!(std::fs::read(&quarantine).unwrap(), b"{ this is not json");
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn a_future_schema_is_left_untouched() {
        let (_dir, store) = store();
        let path = store.paths().user_state_file();
        let document = json!({"schema": 99, "preferences": {}});
        crate::atomic::write_atomic(&path, document.to_string().as_bytes()).unwrap();

        let outcome = store.load_user_state().unwrap();
        assert_eq!(outcome, LoadOutcome::Future { version: 99 });
        assert!(path.exists(), "the newer file must not be touched");
        let still: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(still["schema"], json!(99));
    }

    #[test]
    fn an_unmigratable_file_is_left_untouched() {
        let (_dir, mut store) = store();
        store.register_migration(FileKind::UserState, 0, |_| Err("nope".to_string()));
        let path = store.paths().user_state_file();
        crate::atomic::write_atomic(&path, b"{}").unwrap();

        let outcome = store.load_user_state().unwrap();
        match &outcome {
            LoadOutcome::Unmigratable { version, reason } => {
                assert_eq!(*version, 0);
                assert!(reason.contains("nope"));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(path.exists(), "the unmigratable file must not be touched");
    }

    #[test]
    fn a_migratable_file_is_upgraded_on_load() {
        let (_dir, mut store) = store();
        store.register_migration(FileKind::UserState, 0, |mut v| {
            v.as_object_mut()
                .unwrap()
                .insert("migrated".to_string(), json!(true));
            Ok(v)
        });
        let path = store.paths().user_state_file();
        crate::atomic::write_atomic(&path, b"{\"preferences\":{}}").unwrap();

        let outcome = store.load_user_state().unwrap();
        assert!(outcome.is_loaded());
    }

    #[test]
    fn a_non_object_document_is_treated_as_corrupt() {
        let (_dir, store) = store();
        let path = store.paths().user_state_file();
        crate::atomic::write_atomic(&path, b"[1, 2, 3]").unwrap();
        let outcome = store.load_user_state().unwrap();
        assert!(matches!(outcome, LoadOutcome::Corrupt { .. }));
    }

    #[test]
    fn two_corrupt_files_in_the_same_second_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        crate::atomic::write_atomic(&path, b"nope").unwrap();
        let first = quarantine_corrupt(&path).unwrap();
        crate::atomic::write_atomic(&path, b"nope again").unwrap();
        let second = quarantine_corrupt(&path).unwrap();
        assert_ne!(first, second);
        assert!(first.exists() && second.exists());
    }

    #[test]
    fn the_subscription_cache_round_trips() {
        let (_dir, store) = store();
        assert_eq!(store.read_subscription_cache("abc").unwrap(), None);
        store
            .write_subscription_cache("abc", b"proxies: []")
            .unwrap();
        assert_eq!(
            store.read_subscription_cache("abc").unwrap(),
            Some(b"proxies: []".to_vec())
        );
        assert_eq!(
            store.cached_subscription_ids().unwrap(),
            vec!["abc".to_string()]
        );
        store.remove_subscription_cache("abc").unwrap();
        assert_eq!(store.read_subscription_cache("abc").unwrap(), None);
    }

    #[test]
    fn the_profile_source_round_trips_and_a_missing_one_is_none() {
        let (_dir, store) = store();
        assert_eq!(store.read_profile_source("loc").unwrap(), None);
        store
            .write_profile_source("loc", "proxies:\n  - name: a\n")
            .unwrap();
        assert_eq!(
            store.read_profile_source("loc").unwrap(),
            Some("proxies:\n  - name: a\n".to_string())
        );
        store.write_profile_source("loc", "replaced").unwrap();
        assert_eq!(
            store.read_profile_source("loc").unwrap(),
            Some("replaced".to_string())
        );
        assert!(store.paths().profile_source_file("loc").unwrap().exists());
        store.remove_profile_source("loc").unwrap();
        assert_eq!(store.read_profile_source("loc").unwrap(), None);
    }

    #[test]
    fn removing_a_missing_profile_source_is_not_an_error() {
        let (_dir, store) = store();
        store.remove_profile_source("never-pasted").unwrap();
    }

    #[test]
    fn an_unsafe_profile_source_id_is_refused() {
        let (_dir, store) = store();
        assert!(matches!(
            store.write_profile_source("../escape", "x").unwrap_err(),
            StoreError::UnsafeId { .. }
        ));
        assert!(store.read_profile_source("a/b").is_err());
    }

    #[test]
    fn the_profile_source_is_kept_when_the_cache_is_cleared() {
        let (_dir, store) = store();
        store.write_profile_source("loc", "proxies: []").unwrap();
        store.write_subscription_cache("loc", b"{}").unwrap();
        store.remove_subscription_cache("loc").unwrap();
        assert_eq!(
            store.read_profile_source("loc").unwrap(),
            Some("proxies: []".to_string())
        );
    }

    #[test]
    fn removing_a_missing_cache_entry_is_not_an_error() {
        let (_dir, store) = store();
        store.remove_subscription_cache("never-fetched").unwrap();
    }

    #[test]
    fn a_subscription_cache_id_that_is_not_a_safe_file_name_is_refused() {
        let (_dir, store) = store();
        let err = store
            .write_subscription_cache("../escape", b"x")
            .unwrap_err();
        assert!(matches!(err, StoreError::UnsafeId { .. }));
    }

    #[test]
    fn the_subscription_metadata_holds_no_url_field() {
        // The metadata is the file that is safe to inspect; it must not
        // contain the subscription URL in any form.
        let (_dir, store) = store();
        let meta = SubscriptionMeta {
            subscriptions: vec![Subscription {
                id: "sub-1".to_string(),
                label: "My provider".to_string(),
                mode: starpod_core::domain::SubscriptionMode::AppManaged,
                interval_secs: 3600,
                has_auth_header: true,
                last_updated: None,
                quota: None,
                node_count: 0,
                group_count: 0,
                last_error: None,
                profile_title: None,
                announce: None,
                support_url: None,
                web_url: None,
                web_page_name: None,
                announce_url: None,
                logo_file: None,
                global_mode_disabled: false,
                expand_proxy_groups: None,
                device_limit: false,
                local: false,
            }],
            url_keys: vec![SecretRef {
                subscription_id: "sub-1".to_string(),
                key: "subscription:sub-1:url".to_string(),
            }],
            header_keys: vec![],
        };
        store.save_subscription_meta(&meta).unwrap();
        let raw = std::fs::read_to_string(store.paths().subscription_meta_file()).unwrap();
        assert!(
            !raw.contains("http"),
            "the metadata must not contain a URL: {raw}"
        );
        assert!(raw.contains("subscription:sub-1:url"));
    }
}
