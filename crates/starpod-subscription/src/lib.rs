//! The subscription layer: fetching, parsing, validating and refreshing
//! proxy subscriptions.
//!
//! # What this crate is
//!
//! GOAL.md §3 asks for a subscription management layer that retrieves
//! subscription data, handles authentication, parses and validates it,
//! survives network failures and invalid responses, and caches the last
//! valid payload. This crate implements that layer and nothing else.
//!
//! # The seams
//!
//! There are exactly two seams, and both are narrow:
//!
//! 1. [`provider::SubscriptionProvider`] — *where the bytes come from*. The
//!    only I/O is an HTTP GET of the body plus reading the response
//!    headers. A mock returns canned bytes, so the whole crate is
//!    testable without a network.
//! 2. [`pipeline`] — *what the bytes mean*. A pure function from bytes to a
//!    [`starpod_core::SubscriptionPayload`]. No clock, no filesystem, no
//!    engine, no GUI.
//!
//! # The pipeline, and where namespacing happens
//!
//! The body parsing pipeline is ARCHITECTURE §3.2.2:
//!
//! ```text
//! bytes → sniff format → YAML parse → shape check → per-entry validation → SubscriptionPayload
//! ```
//!
//! **Namespacing happens in the pipeline, not in the provider.** A
//! [`provider::FetchResponse`] carries the raw bytes and nothing else; the
//! [`pipeline::parse_payload`] function is handed the subscription's label
//! and is the single place where a node name becomes
//! `<label> / <node name>`. The provider stays a transport, and the
//! pipeline stays the only thing that knows the app's naming rules.
//!
//! # Where the engine's rules come from
//!
//! Every claim about what the engine accepts cites the vendored meow
//! source. The short version:
//!
//! - The engine's own subscription parser requires a `proxies:` key and
//!   will not unwrap base64
//!   (`meow-config-0.29.0/src/subscription.rs:336-343`). The *app* accepts
//!   base64-wrapped YAML as well, because that is what panels emit.
//! - The engine's fetcher discards the `subscription-userinfo` header
//!   (`meow-config-0.29.0/src/subscription.rs:278-288` calls
//!   `internal_http::fetch`, which returns only the body). The quota is
//!   therefore only available to the app.
//! - The node protocols are the ones `proxy_parser.rs` dispatches on
//!   (`meow-config-0.29.0/src/proxy_parser.rs:326-539`).
//!
//! # What this crate deliberately does not do
//!
//! It does not spawn processes, does not talk to the engine, and does not
//! know about the GUI. It performs HTTP, and it performs it behind a trait
//! so the tests never need a server.

pub mod cache;
pub mod error;
pub mod metadata;
pub mod pipeline;
pub mod provider;
pub mod refresh;
pub mod schedule;

pub use cache::{SubscriptionCache, SubscriptionCacheSet};
pub use error::{ProviderError, SubscriptionError};
pub use metadata::{SubscriptionMetadata, SubscriptionQuota, format_timestamp};
pub use pipeline::{ParsedBody, PayloadFormat, parse_payload};
pub use provider::{
    FetchRequest, FetchResponse, HttpSubscriptionProvider, ProviderKind,
    StaticSubscriptionProvider, SubscriptionProvider,
};
pub use refresh::{RefreshOutcome, refresh};
pub use schedule::{RefreshSchedule, next_due_at};
