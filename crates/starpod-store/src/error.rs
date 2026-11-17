//! Errors raised by the store.
//!
//! Two error families live here, and they are deliberately different shapes:
//!
//! - [`StoreError`] is for *I/O and format* problems — a directory that cannot
//!   be resolved, a file that cannot be written, a payload that will not
//!   serialize.
//! - [`MigrationError`] is for a *schema* problem — a file whose version this
//!   build cannot upgrade. It is separate because a migration failure must
//!   leave the original file untouched, and the caller needs to be able to say
//!   "we could not upgrade this file" without it looking like a disk error.
//!
//! No error message in this module ever embeds a secret. The store's inputs
//! are user state and metadata; the secrets themselves live in the secret
//! store, which reports failures without echoing the value.

use std::path::PathBuf;

/// A failure while reading or writing persisted state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A platform directory could not be resolved for the current user.
    #[error("could not resolve the {which} directory for this user")]
    NoDirectory {
        /// Which directory failed: `"config"`, `"data"` or `"cache"`.
        which: &'static str,
    },

    /// An I/O operation failed.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// The path the operation was applied to.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// A value could not be serialized to JSON.
    #[error("could not serialize {what}: {source}")]
    Serialize {
        /// What was being serialized, e.g. `"user state"`.
        what: &'static str,
        /// The underlying error.
        #[source]
        source: serde_json::Error,
    },

    /// A value could not be read back from JSON.
    #[error("could not parse {what} from {path}: {source}")]
    Parse {
        /// What was being parsed, e.g. `"user state"`.
        what: &'static str,
        /// The file that was read.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: serde_json::Error,
    },

    /// A file's permissions could not be restricted to the current user.
    #[error("could not restrict the permissions of {path} to the current user: {reason}")]
    Permissions {
        /// The file or directory whose permissions could not be set.
        path: PathBuf,
        /// Why the attempt failed.
        reason: String,
    },

    /// An identifier was not safe to use as a file name.
    #[error("{kind} id {id:?} is not usable as a file name")]
    UnsafeId {
        /// Which kind of id this was.
        kind: &'static str,
        /// The offending id.
        id: String,
    },

    /// The generated runtime configuration was refused because it carries a
    /// secret that must not be written to disk.
    ///
    /// The controller secret is passed to the engine on its command line
    /// (`--secret`), never in the config file (ARCHITECTURE §5.1). This error
    /// is the last line of defense against a call site that does that anyway.
    #[error(
        "the generated configuration contains a {key:?} key, which must never be written to disk"
    )]
    ForbiddenSecretKey {
        /// The offending key, e.g. `"secret"`.
        key: String,
    },
}

impl StoreError {
    /// Wrap an [`std::io::Error`] with the path it happened at.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// A failure while upgrading a state file to the current schema version.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MigrationError {
    /// No migration step exists for the version the file is at.
    ///
    /// The file is left exactly as it was: an unmigratable file is not
    /// corrupt, and destroying it would throw away the user's data.
    #[error("no migration is registered from schema version {from} of {kind}")]
    Missing {
        /// Which file kind was being migrated, by name.
        kind: &'static str,
        /// The version the file is at.
        from: u32,
    },

    /// A migration step ran but could not handle this particular file.
    #[error("the migration from schema version {from} of {kind} failed: {reason}")]
    Failed {
        /// Which file kind was being migrated, by name.
        kind: &'static str,
        /// The version the file was at before the failing step.
        from: u32,
        /// Why the step failed.
        reason: String,
    },
}

/// Attach a path to an [`std::io::Error`], producing a [`StoreError`].
pub(crate) fn io_at(path: impl Into<PathBuf>, source: std::io::Error) -> StoreError {
    StoreError::io(path, source)
}
