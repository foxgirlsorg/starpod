//! Schema versioning and migration.
//!
//! Every persisted file carries a `schema` number. The number is the *format
//! version of that file*, not the app version: it changes only when the shape
//! of the file changes.
//!
//! The rules, from ARCHITECTURE §3.1 and GOAL.md §5:
//!
//! - **Unknown/newer version → the file is left untouched.** A file written by
//!   a newer build of the app is not corrupt; it is a file this build does not
//!   understand. Rewriting it would destroy data the user might still be able
//!   to recover with the newer build.
//! - **Older version → a pure migration function upgrades it.** The migration
//!   operates on the parsed JSON value, not on the file, so it cannot damage
//!   the file if it fails.
//! - **A migration that cannot handle the file preserves the original.** If
//!   the migration step returns an error, the file is not rewritten; the
//!   caller is told, and it can decide what to do.
//!
//! Migrations are registered per file kind, because the three files have
//! independent shapes. A migration registered for the subscription metadata
//! says nothing about the user state file.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::MigrationError;

/// The key under which the schema version is stored in every state file.
pub const SCHEMA_KEY: &str = "schema";

/// The schema version of the user-state file.
pub const USER_STATE_SCHEMA: u32 = 1;

/// The schema version of the subscription-metadata file.
pub const SUBSCRIPTION_META_SCHEMA: u32 = 1;

/// The schema version of the plain-file secret fallback.
pub const FALLBACK_SECRET_SCHEMA: u32 = 1;

/// The kind of file a migration applies to.
///
/// The kind is part of the error type because the message the user sees
/// ("the subscription list could not be upgraded") must name the right file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileKind {
    /// The user's preferences, rules, nodes and groups.
    UserState,
    /// The subscription metadata.
    SubscriptionMeta,
    /// The plain-file secret fallback.
    FallbackSecrets,
}

impl FileKind {
    /// A short human-readable name for this kind of file.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserState => "user state",
            Self::SubscriptionMeta => "subscription list",
            Self::FallbackSecrets => "secret fallback",
        }
    }
}

/// A registry of migration steps, keyed by the version they upgrade *from*.
///
/// A step is a pure function from the parsed JSON of the file to the parsed
/// JSON of the next version. Keeping it pure means a failed step cannot have
/// written anything.
pub struct Migrations {
    steps: BTreeMap<(FileKind, u32), MigrationStep>,
}

/// A migration step: a pure function from one schema version to the next.
type MigrationStep = Box<dyn Fn(Value) -> Result<Value, String> + Send + Sync>;

impl std::fmt::Debug for Migrations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migrations")
            .field("steps", &self.steps.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Default for Migrations {
    fn default() -> Self {
        Self::new()
    }
}

impl Migrations {
    /// An empty registry — a file at the current version needs no migration.
    pub fn new() -> Self {
        Self {
            steps: BTreeMap::new(),
        }
    }

    /// Register a step that upgrades `kind` from version `from` to `from + 1`.
    ///
    /// Registering a step for a version that already has one replaces it;
    /// this keeps the registration order in tests irrelevant.
    pub fn step(
        &mut self,
        kind: FileKind,
        from: u32,
        step: impl Fn(Value) -> Result<Value, String> + Send + Sync + 'static,
    ) -> &mut Self {
        self.steps.insert((kind, from), Box::new(step));
        self
    }

    /// Whether a step exists for `kind` at version `from`.
    pub fn has_step(&self, kind: FileKind, from: u32) -> bool {
        self.steps.contains_key(&(kind, from))
    }

    /// Migrate `value` from `from` to `current`.
    ///
    /// `value` is the parsed file, `from` the version it declares. The
    /// returned value is the migrated document, with its `schema` field set
    /// to `current`.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::Missing`] when there is no registered step
    /// for the version the file is at, and [`MigrationError::Failed`] when a
    /// step rejects the document. In both cases the caller must leave the
    /// original file alone.
    pub fn migrate(
        &self,
        kind: FileKind,
        value: Value,
        from: u32,
        current: u32,
    ) -> Result<Value, MigrationError> {
        if from > current {
            // A file from a newer build. Not corrupt — just unreadable by
            // this build. The caller leaves it alone.
            return Err(MigrationError::Missing {
                kind: kind.as_str(),
                from,
            });
        }
        if from == current {
            return Ok(value);
        }

        let mut document = value;
        let mut version = from;
        while version < current {
            let Some(step) = self.steps.get(&(kind, version)) else {
                return Err(MigrationError::Missing {
                    kind: kind.as_str(),
                    from: version,
                });
            };
            document = step(document).map_err(|reason| MigrationError::Failed {
                kind: kind.as_str(),
                from: version,
                reason,
            })?;
            version += 1;
        }
        set_schema(&mut document, current);
        Ok(document)
    }
}

/// Read the schema version out of a parsed document.
///
/// A document with no `schema` field is treated as version `0`, which is the
/// "pre-versioning" version: the oldest possible file, and therefore the one
/// most likely to have a registered migration.
pub fn schema_of(document: &Value) -> u32 {
    document
        .get(SCHEMA_KEY)
        .and_then(Value::as_u64)
        .map(|v| v.min(u32::MAX as u64) as u32)
        .unwrap_or(0)
}

/// Set the schema version of a parsed document.
pub fn set_schema(document: &mut Value, version: u32) {
    if let Value::Object(map) = document {
        map.insert(SCHEMA_KEY.to_string(), Value::from(version));
    }
}

/// A convenience for building the `schema` field of a serializable value.
pub fn schema_field(version: u32) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    map.insert(SCHEMA_KEY.to_string(), Value::from(version));
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registry() -> Migrations {
        let mut m = Migrations::new();
        m.step(FileKind::UserState, 0, |mut v| {
            let obj = v
                .as_object_mut()
                .ok_or_else(|| "the document is not an object".to_string())?;
            obj.insert("migrated".to_string(), Value::Bool(true));
            Ok(v)
        });
        m
    }

    #[test]
    fn a_document_at_the_current_version_is_returned_unchanged() {
        let m = registry();
        let doc = json!({"schema": 1, "x": 1});
        let out = m.migrate(FileKind::UserState, doc.clone(), 1, 1).unwrap();
        assert_eq!(out, doc);
    }

    #[test]
    fn an_older_document_is_upgraded_and_stamped() {
        let m = registry();
        let out = m
            .migrate(FileKind::UserState, json!({"x": 1}), 0, 1)
            .unwrap();
        assert_eq!(out["schema"], json!(1));
        assert_eq!(out["migrated"], json!(true));
        assert_eq!(out["x"], json!(1));
    }

    #[test]
    fn a_newer_document_is_refused_and_not_touched() {
        let m = registry();
        let err = m
            .migrate(FileKind::UserState, json!({"schema": 99}), 99, 1)
            .unwrap_err();
        assert!(matches!(err, MigrationError::Missing { from: 99, .. }));
    }

    #[test]
    fn a_missing_step_is_refused_rather_than_guessed() {
        let m = registry();
        // There is no step for the subscription metadata at version 0.
        let err = m
            .migrate(FileKind::SubscriptionMeta, json!({}), 0, 1)
            .unwrap_err();
        assert!(matches!(
            err,
            MigrationError::Missing {
                kind: "subscription list",
                ..
            }
        ));
    }

    #[test]
    fn a_failing_step_reports_why() {
        let mut m = Migrations::new();
        m.step(FileKind::UserState, 0, |_| {
            Err("the file has no preferences".to_string())
        });
        let err = m.migrate(FileKind::UserState, json!({}), 0, 1).unwrap_err();
        match err {
            MigrationError::Failed { kind, from, reason } => {
                assert_eq!(kind, "user state");
                assert_eq!(from, 0);
                assert_eq!(reason, "the file has no preferences");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_missing_schema_field_is_version_zero() {
        assert_eq!(schema_of(&json!({})), 0);
        assert_eq!(schema_of(&json!({"schema": 3})), 3);
    }

    #[test]
    fn a_non_numeric_schema_field_is_version_zero() {
        assert_eq!(schema_of(&json!({"schema": "one"})), 0);
        assert_eq!(schema_of(&json!({"schema": null})), 0);
    }

    #[test]
    fn multi_step_migrations_run_in_order() {
        let mut m = Migrations::new();
        m.step(FileKind::UserState, 0, |mut v| {
            v.as_object_mut()
                .unwrap()
                .insert("order".to_string(), json!("first"));
            Ok(v)
        });
        m.step(FileKind::UserState, 1, |mut v| {
            let prev = v["order"].as_str().unwrap().to_string();
            v.as_object_mut()
                .unwrap()
                .insert("order".to_string(), json!(format!("{prev},second")));
            Ok(v)
        });
        let out = m.migrate(FileKind::UserState, json!({}), 0, 2).unwrap();
        assert_eq!(out["order"], json!("first,second"));
        assert_eq!(out["schema"], json!(2));
    }

    #[test]
    fn steps_are_scoped_to_their_file_kind() {
        let mut m = Migrations::new();
        m.step(FileKind::UserState, 0, Ok);
        assert!(m.has_step(FileKind::UserState, 0));
        assert!(!m.has_step(FileKind::SubscriptionMeta, 0));
        assert!(!m.has_step(FileKind::FallbackSecrets, 0));
    }

    #[test]
    fn a_re_registered_step_replaces_the_previous_one() {
        let mut m = Migrations::new();
        m.step(FileKind::UserState, 0, |_| Ok(json!({"a": 1})));
        m.step(FileKind::UserState, 0, |_| Ok(json!({"b": 2})));
        let out = m.migrate(FileKind::UserState, json!({}), 0, 1).unwrap();
        assert_eq!(out["b"], json!(2));
    }

    #[test]
    fn the_schema_field_helper_produces_a_json_object() {
        let map = schema_field(4);
        assert_eq!(map.get(SCHEMA_KEY), Some(&json!(4)));
    }
}
