//! The UI-free application service layer.
//!
//! This crate is the thing the UI calls. It implements the central path
//! end to end:
//!
//! ```text
//! subscription fetch → parse → validate → cache
//!   → merge with user preferences and rules (starpod_core::build)
//!   → candidate configuration
//!   → engine validation (meow -t)
//!   → start / reload
//! ```
//!
//! # Using it
//!
//! [`AppService::open_default`] builds the real dependencies. Tests and
//! embedders build one with [`AppService::new`] and a [`ServiceDeps`] that
//! carries their own engine, provider and secret store.
//!
//! The UI holds the service behind [`SharedAppService`] for actions and
//! subscribes to [`AppService::subscribe`] for the view state. Reading the
//! snapshot never waits for an action, so a slow engine call does not freeze
//! the window.
//!
//! The crate has no GUI dependency and no engine-specific names. It talks to
//! the engine only through [`starpod_engine::ProxyEngine`].

mod compose;
pub mod error;
mod reassert;
pub mod service;
pub mod snapshot;
pub mod ui;

pub use error::{AppError, RuleLineError};
pub use service::{AppService, ConfigCheck, ServiceDeps, SharedAppService};
pub use snapshot::{
    AppSnapshot, EngineView, NodeView, NoticeKind, RuleKindView, RuleView, SubscriptionStatus,
    SubscriptionView,
};
