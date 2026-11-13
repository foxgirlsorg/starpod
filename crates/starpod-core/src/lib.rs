//! The domain model and the runtime configuration pipeline.
//!
//! This crate is pure: it holds the types that describe a proxy client's
//! state and the functions that turn a subscription plus the user's own
//! preferences into a configuration the engine can run. It performs no I/O,
//! starts no processes, and knows nothing about the GUI.
//!
//! That separation is the point. GOAL.md §3 requires the configuration
//! builder to be *independently testable* — the merge pipeline below can be
//! exercised with plain fixtures, with no window on screen and no engine
//! running.

pub mod config;
pub mod domain;
pub mod rules;
pub mod timestamp;

pub use config::build;
pub use domain::{
    BuildInput, BuildOutcome, EngineManagedSubscription, Finding, LogLevel, Node, ProxyGroup,
    ProxyMode, RuleProvider, RuleProviderBehavior, RuntimeConfig, Severity, StoredRule,
    Subscription, SubscriptionMode, SubscriptionPayload, SubscriptionQuota, SubscriptionSettings,
    TunSettings, UserPreferences,
};
pub use rules::{RuleSpec, RuleTarget};
