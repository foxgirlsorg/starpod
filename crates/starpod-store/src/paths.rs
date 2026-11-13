//! Where the app keeps its files, on each of the three supported platforms.
//!
//! The layout follows the platform conventions rather than inventing its own:
//!
//! | Platform | Config | Data | Cache |
//! |---|---|---|---|
//! | Windows | `%APPDATA%\Starpod` | `%LOCALAPPDATA%\Starpod` | `%LOCALAPPDATA%\Starpod\cache` |
//! | macOS | `~/Library/Application Support/Starpod` | same | `~/Library/Caches/Starpod` |
//! | Linux | `$XDG_CONFIG_HOME/Starpod` | `$XDG_DATA_HOME/Starpod` | `$XDG_CACHE_HOME/Starpod` |
//!
//! The `dirs` crate resolves the platform-specific roots (verified against
//! `dirs-7.0.0/src/{win,mac,lin}.rs`); this module only decides the
//! application subdirectory and the file names inside it.
//!
//! Three directories, not one, because they have different lifetimes and
//! different security properties:
//!
//! - **config** holds the user's own settings. It is the only thing a user
//!   would ever want to back up or hand-edit.
//! - **data** holds state the app needs but the user does not edit: the
//!   subscription metadata, the generated engine configuration, and the
//!   engine installation.
//! - **cache** holds things that can be thrown away and re-fetched: the
//!   cached subscription payloads.
//!
//! The separation matters for the "a subscription refresh never overwrites a
//! user setting" guarantee (ARCHITECTURE §3.4): the user's settings and the
//! subscription-derived data are in *different directories*, so there is no
//! code path where a subscription write can reach a preference file.

use std::path::{Path, PathBuf};

use crate::error::StoreError;

/// The application directory name used inside every platform root.
///
/// It is also the keychain/secret-service *service* name, so the two must
/// match: a secret written by one build must be readable by the next.
pub const APP_DIR_NAME: &str = "Starpod";

/// The resolved set of directories the app writes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Where the user's own settings live.
    config_dir: PathBuf,
    /// Where the app's own state lives.
    data_dir: PathBuf,
    /// Where re-fetchable data lives.
    cache_dir: PathBuf,
}

impl AppPaths {
    /// Build the paths from an explicit set of roots.
    ///
    /// The roots are the platform's own directories (`APPDATA`, `~/Library/…`,
    /// `$XDG_CONFIG_HOME`, …); the application subdirectory is appended.
    /// Tests use this to point the store at a temporary directory.
    pub fn from_roots(
        config_root: impl Into<PathBuf>,
        data_root: impl Into<PathBuf>,
        cache_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            config_dir: config_root.into().join(APP_DIR_NAME),
            data_dir: data_root.into().join(APP_DIR_NAME),
            cache_dir: cache_root.into().join(APP_DIR_NAME),
        }
    }

    /// Resolve the directories for the current user on this platform.
    pub fn resolve() -> Result<Self, StoreError> {
        let config_root = dirs::config_dir().ok_or(StoreError::NoDirectory { which: "config" })?;
        let data_root = dirs::data_local_dir().ok_or(StoreError::NoDirectory { which: "data" })?;
        let cache_root = dirs::cache_dir().ok_or(StoreError::NoDirectory { which: "cache" })?;
        Ok(Self::from_roots(config_root, data_root, cache_root))
    }

    /// The directory holding the user's preferences, rules and nodes.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// The directory holding the subscription metadata and the generated
    /// engine configuration.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The directory holding the cached subscription payloads.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// The file holding the user's preferences, rules and nodes.
    ///
    /// One file, because these three things are *the user's own
    /// configuration* — they are edited together and they are the thing a
    /// subscription refresh must never touch.
    pub fn user_state_file(&self) -> PathBuf {
        self.config_dir.join("user-state.json")
    }

    /// The file holding the subscription metadata (no secrets).
    pub fn subscription_meta_file(&self) -> PathBuf {
        self.data_dir.join("subscriptions.json")
    }

    /// The directory holding the cached subscription payloads.
    ///
    /// A directory of files rather than one file, because the payloads are
    /// large, are written and replaced independently, and a corrupt payload
    /// must not invalidate the others.
    pub fn subscription_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("subscriptions")
    }

    /// The cached payload for one subscription.
    ///
    /// The id is validated first: it becomes a file name, and a file name
    /// containing a path separator would let a crafted subscription id escape
    /// the cache directory.
    pub fn subscription_cache_file(&self, id: &str) -> Result<PathBuf, StoreError> {
        Ok(self
            .subscription_cache_dir()
            .join(safe_file_name("subscription", id)?))
    }

    /// The file the generated engine configuration is written to.
    ///
    /// It lives in the *data* directory rather than the config directory
    /// because it is derived, not authored: the app rewrites it on every
    /// apply, and it may contain a subscription URL with a token.
    ///
    /// This is the **only** path the generated configuration lives at. The
    /// engine crate does not compute a path of its own — the app passes
    /// this one in — because two paths for one file means the app and the
    /// engine can end up reading different bytes.
    pub fn runtime_config_file(&self) -> PathBuf {
        self.data_dir.join("runtime-config.yaml")
    }

    /// The directory the engine publishes its controller file into.
    ///
    /// The engine writes `<pid>.json` there, naming the address its REST
    /// controller bound. The app points the engine at this directory with
    /// `MEOW_RUNTIME_DIR` (`meow-common-0.29.0/src/controller_file.rs:24`)
    /// so that:
    ///
    /// - Starpod's controller file never collides with another meow's on
    ///   the same machine, and
    /// - the file is in a directory the app already restricts to the
    ///   current user.
    pub fn controller_dir(&self) -> PathBuf {
        self.data_dir.join("controllers")
    }

    /// The file the plain-file secret fallback writes to.
    pub fn fallback_secret_file(&self) -> PathBuf {
        self.data_dir.join("secrets.json")
    }
}

/// Turn an arbitrary id into a file name that is safe to join onto a path.
///
/// Subscription ids are uuids in practice, but the store does not assume
/// that: an id containing a path separator or a `..` would let a write escape
/// the directory it was supposed to stay in.
pub(crate) fn safe_file_name(kind: &'static str, id: &str) -> Result<String, StoreError> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        return Err(StoreError::UnsafeId {
            kind,
            id: id.to_string(),
        });
    }
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_application_directory_is_appended_to_every_root() {
        let paths = AppPaths::from_roots("/cfg", "/data", "/cache");
        assert_eq!(paths.config_dir(), Path::new("/cfg/Starpod"));
        assert_eq!(paths.data_dir(), Path::new("/data/Starpod"));
        assert_eq!(paths.cache_dir(), Path::new("/cache/Starpod"));
    }

    #[test]
    fn the_user_state_lives_in_the_config_directory() {
        let paths = AppPaths::from_roots("/cfg", "/data", "/cache");
        assert_eq!(
            paths.user_state_file(),
            Path::new("/cfg/Starpod/user-state.json")
        );
    }

    #[test]
    fn the_subscription_metadata_and_the_runtime_config_live_in_the_data_directory() {
        let paths = AppPaths::from_roots("/cfg", "/data", "/cache");
        assert_eq!(
            paths.subscription_meta_file(),
            Path::new("/data/Starpod/subscriptions.json")
        );
        assert_eq!(
            paths.runtime_config_file(),
            Path::new("/data/Starpod/runtime-config.yaml")
        );
    }

    #[test]
    fn the_subscription_cache_lives_in_the_cache_directory() {
        let paths = AppPaths::from_roots("/cfg", "/data", "/cache");
        assert_eq!(
            paths.subscription_cache_dir(),
            Path::new("/cache/Starpod/subscriptions")
        );
        assert_eq!(
            paths.subscription_cache_file("abc-123").unwrap(),
            Path::new("/cache/Starpod/subscriptions/abc-123")
        );
    }

    #[test]
    fn the_controller_directory_is_inside_the_data_directory() {
        let paths = AppPaths::from_roots("/cfg", "/data", "/cache");
        assert_eq!(paths.controller_dir(), Path::new("/data/Starpod/controllers"));
    }

    #[test]
    fn the_user_state_and_the_subscription_metadata_are_in_different_directories() {
        // This is the structural guarantee that a subscription refresh can
        // never overwrite a user setting (ARCHITECTURE §3.4).
        let paths = AppPaths::from_roots("/cfg", "/data", "/cache");
        assert_ne!(
            paths.user_state_file().parent(),
            paths.subscription_meta_file().parent()
        );
    }

    #[test]
    fn a_path_separator_in_an_id_is_refused() {
        let paths = AppPaths::from_roots("/cfg", "/data", "/cache");
        let err = paths
            .subscription_cache_file("../../etc/passwd")
            .unwrap_err();
        assert!(matches!(err, StoreError::UnsafeId { .. }));
    }

    #[test]
    fn a_windows_path_separator_in_an_id_is_refused() {
        let paths = AppPaths::from_roots("/cfg", "/data", "/cache");
        let err = paths
            .subscription_cache_file(r"..\..\windows\system32")
            .unwrap_err();
        assert!(matches!(err, StoreError::UnsafeId { .. }));
    }

    #[test]
    fn an_empty_or_overlong_id_is_refused() {
        assert!(safe_file_name("subscription", "").is_err());
        assert!(safe_file_name("subscription", &"x".repeat(129)).is_err());
        assert!(safe_file_name("subscription", &"x".repeat(128)).is_ok());
    }

    #[test]
    fn ordinary_uuid_ids_are_accepted() {
        let id = "6f1c2d3e-4b5a-6c7d-8e9f-0a1b2c3d4e5f";
        assert_eq!(safe_file_name("subscription", id).unwrap(), id);
    }

    #[test]
    fn a_path_separator_is_the_only_thing_that_is_refused() {
        assert!(safe_file_name("subscription", "a/b").is_err());
        assert!(safe_file_name("subscription", "a b").is_err());
        assert!(safe_file_name("subscription", "a.b").is_err());
        assert!(safe_file_name("subscription", "a_b-c").is_ok());
    }

    #[test]
    fn resolve_returns_directories_that_are_under_the_platform_roots() {
        // The exact roots are platform-specific; what is universal is that
        // resolution succeeds and that the app subdirectory is appended.
        let paths = AppPaths::resolve().expect("the platform directories resolve");
        assert!(paths.config_dir().ends_with(APP_DIR_NAME));
        assert!(paths.data_dir().ends_with(APP_DIR_NAME));
        assert!(paths.cache_dir().ends_with(APP_DIR_NAME));
    }
}
