//! The error types.
//!
//! There are two, and they are deliberately different shapes:
//!
//! - [`ProviderError`] is what a [`crate::provider::SubscriptionProvider`]
//!   returns. It covers *transport* problems only: the request failed, the
//!   server answered with a status the app cannot use, the body could not
//!   be read.
//! - [`SubscriptionError`] is what the rest of the layer returns. It covers
//!   the whole pipeline: a provider error, a body that cannot be decoded,
//!   a body that decodes but contains nothing usable.
//!
//! The split matters for one decision in particular: a **401 or 403** is
//! not a generic failure. It means the subscription's token has expired or
//! the server refuses the request, and the user has to fix the URL or the
//! `Authorization` header. That is a distinct variant
//! ([`ProviderError::Unauthorized`]) so the UI can say exactly that
//! instead of "refresh failed".

use thiserror::Error;

/// A transport-level failure: the subscription could not be retrieved.
///
/// A provider is only allowed to fail this way. Anything that requires
/// *understanding* the body is a [`SubscriptionError`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// The request never completed — DNS failure, connection refused,
    /// TLS failure, timeout.
    #[error("the subscription could not be reached: {0}")]
    Network(String),

    /// The server answered with a status the app cannot use.
    ///
    /// The status is kept so the caller can distinguish a 404 (the
    /// subscription was removed) from a 500 (the server broke).
    #[error("the subscription server answered with HTTP {status}")]
    Status {
        /// The HTTP status code.
        status: u16,
    },

    /// The server answered `401` or `403`.
    ///
    /// The subscription's token has expired, or the server refuses the
    /// request. Only the user can fix this, so it must not be reported as
    /// a generic failure.
    #[error(
        "the subscription was refused (HTTP {status}): the token has expired or the \
             server does not authorize this request"
    )]
    Unauthorized {
        /// The HTTP status code — `401` or `403`.
        status: u16,
    },

    /// The response body could not be read to completion.
    #[error("the subscription body could not be read: {0}")]
    Body(String),
}

impl ProviderError {
    /// Whether this failure is one the user must fix by hand.
    ///
    /// An expired token is not a transient fault: retrying it on a timer
    /// produces the same failure forever, so the refresh loop should back
    /// off rather than retry aggressively.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Unauthorized { .. })
    }
}

/// A failure of the subscription pipeline as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SubscriptionError {
    /// The bytes could not be retrieved.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// The body was retrieved but is not a subscription the app can read.
    ///
    /// This covers a body that is not UTF-8, a body that is neither Clash
    /// YAML nor base64-wrapped Clash YAML, and a YAML document whose
    /// `proxies:` section is not a list.
    #[error("the subscription body could not be read: {0}")]
    InvalidBody(String),

    /// The body was a valid subscription, but not one node in it survived
    /// validation.
    ///
    /// This is deliberately distinct from
    /// [`SubscriptionError::EmptyAfterValidation`]: here the document had
    /// entries and every one of them was rejected, which usually means the
    /// publisher changed the format — a different diagnosis from "the
    /// subscription was empty".
    #[error("the subscription carried {dropped} entries but none of them was valid: {reason}")]
    EveryNodeDropped {
        /// How many entries were dropped.
        dropped: usize,
        /// Why they were dropped.
        reason: String,
    },

    /// The subscription parsed, but contained nothing at all.
    #[error("the subscription is empty")]
    EmptyAfterValidation,

    /// The provider reports that this device is not allowed to fetch the
    /// subscription any more (`x-hwid-limit` or `x-hwid-max-devices-reached`
    /// is `true`). Only the provider can free a slot, so the message points
    /// the user at support.
    #[error("This subscription has reached its device limit. Contact support to free a slot.")]
    DeviceLimitReached,

    /// The server answered with an HTML or XML page, not a subscription. This
    /// is what a provider's landing page or a login wall looks like.
    #[error("The server returned a web page, not a subscription")]
    ServedWebPage,
}

impl SubscriptionError {
    /// The provider error behind this one, if any.
    pub fn provider_error(&self) -> Option<&ProviderError> {
        match self {
            Self::Provider(error) => Some(error),
            _ => None,
        }
    }

    /// Whether the user must fix this by hand.
    ///
    /// An expired token is the case the UI must call out: the refresh loop
    /// should stop retrying and wait for the user to supply a new URL or
    /// header.
    pub fn is_fatal(&self) -> bool {
        self.provider_error().is_some_and(ProviderError::is_fatal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unauthorized_status_is_a_distinct_variant() {
        let error = ProviderError::Unauthorized { status: 401 };
        assert!(error.is_fatal());
        assert!(error.to_string().contains("expired"));
        assert!(!ProviderError::Network("boom".to_string()).is_fatal());
        assert!(!ProviderError::Status { status: 500 }.is_fatal());
    }

    #[test]
    fn a_plain_status_is_not_fatal() {
        assert!(!ProviderError::Status { status: 404 }.is_fatal());
    }

    #[test]
    fn a_provider_error_becomes_a_subscription_error() {
        let error: SubscriptionError = ProviderError::Network("boom".to_string()).into();
        assert!(error.provider_error().is_some());
        assert!(!error.is_fatal());
        assert!(error.to_string().contains("boom"));
    }

    #[test]
    fn an_unauthorized_provider_error_is_fatal_through_the_wrapper() {
        let error: SubscriptionError = ProviderError::Unauthorized { status: 403 }.into();
        assert!(error.is_fatal());
    }

    #[test]
    fn a_body_error_carries_no_provider_error() {
        let error = SubscriptionError::InvalidBody("not yaml".to_string());
        assert!(error.provider_error().is_none());
        assert!(!error.is_fatal());
    }

    #[test]
    fn every_node_dropped_is_its_own_variant() {
        let error = SubscriptionError::EveryNodeDropped {
            dropped: 3,
            reason: "unknown protocol".to_string(),
        };
        assert!(error.to_string().contains("3 entries"));
        assert!(error.to_string().contains("unknown protocol"));
    }

    #[test]
    fn empty_after_validation_is_its_own_variant() {
        let error = SubscriptionError::EmptyAfterValidation;
        assert!(error.to_string().contains("empty"));
        assert!(!error.is_fatal());
    }
}
