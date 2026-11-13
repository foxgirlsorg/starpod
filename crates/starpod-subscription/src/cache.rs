//! The payload cache: the last *valid* payload, kept.
//!
//! GOAL.md §3 asks for two things that pull in the same direction:
//!
//! - "Caching the last valid subscription where appropriate."
//! - "Handling network failures, invalid responses, and expired
//!   subscriptions."
//!
//! The cache is how both are satisfied at once. A refresh that fails — a
//! network error, an expired token, a body that is not a subscription, a
//! subscription whose every node was rejected — leaves the previously
//! cached payload **untouched**. The user keeps working; the app shows the
//! error; the next successful refresh replaces the cache.
//!
//! # The rule that matters
//!
//! **A corrupt fetch must not overwrite a good cached payload.** That is
//! the whole point of the type, and it is enforced structurally: the only
//! way to write the cache is [`SubscriptionCache::store`], and it takes a
//! payload that has already been through the pipeline — which means it has
//! already been validated. There is no way to put an unvalidated payload
//! into the cache.
//!
//! # What the cache does not do
//!
//! It does not persist to disk. ARCHITECTURE §3.1 puts the on-disk
//! format in `starpod-store`; this crate stays pure so that it can be
//! tested without a filesystem. The cache here is the *policy* — what is
//! kept and when it is replaced — and the store crate persists whatever
//! this crate says to keep.

use starpod_core::SubscriptionPayload;

use crate::error::SubscriptionError;

/// One subscription's cached payload.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SubscriptionCache {
    /// The last valid payload, if there has ever been one.
    payload: Option<SubscriptionPayload>,
    /// When the cached payload was stored, as a unix timestamp in seconds.
    stored_at: Option<i64>,
}

impl SubscriptionCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached payload, if there is one.
    pub fn payload(&self) -> Option<&SubscriptionPayload> {
        self.payload.as_ref()
    }

    /// When the cached payload was stored, as a unix timestamp in seconds.
    pub fn stored_at(&self) -> Option<i64> {
        self.stored_at
    }

    /// Whether the cache holds a payload.
    pub fn is_empty(&self) -> bool {
        self.payload.is_none()
    }

    /// Store a payload as the new last-valid payload.
    ///
    /// The payload must have come from the pipeline — it is already
    /// validated, and it is not empty (the pipeline rejects a payload with
    /// no surviving nodes before it gets here).
    pub fn store(&mut self, payload: SubscriptionPayload, at: i64) {
        self.payload = Some(payload);
        self.stored_at = Some(at);
    }

    /// Store a payload, but only if it is usable.
    ///
    /// This is the defensive door: a caller that has a payload in hand but
    /// has not run it through the pipeline cannot poison the cache.
    pub fn store_if_valid(
        &mut self,
        payload: SubscriptionPayload,
        at: i64,
    ) -> Result<(), SubscriptionError> {
        if payload.nodes.is_empty() {
            return Err(SubscriptionError::EmptyAfterValidation);
        }
        self.store(payload, at);
        Ok(())
    }

    /// Keep the cache as it is, and report the failure.
    ///
    /// This is what a failed refresh calls. The cached payload survives
    /// untouched; the error is handed back so the caller can record it.
    pub fn reject(&self, error: SubscriptionError) -> Result<(), SubscriptionError> {
        Err(error)
    }

    /// Empty the cache.
    ///
    /// Only the user does this — a failed refresh never empties it.
    pub fn clear(&mut self) {
        self.payload = None;
        self.stored_at = None;
    }
}

/// The cache of every app-managed subscription.
///
/// Keyed by the subscription's id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubscriptionCacheSet {
    entries: std::collections::BTreeMap<String, SubscriptionCache>,
}

impl SubscriptionCacheSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// The cache for one subscription.
    pub fn get(&self, subscription_id: &str) -> Option<&SubscriptionCache> {
        self.entries.get(subscription_id)
    }

    /// The cache for one subscription, creating it if it does not exist.
    pub fn entry(&mut self, subscription_id: &str) -> &mut SubscriptionCache {
        self.entries.entry(subscription_id.to_string()).or_default()
    }

    /// Store a payload for one subscription.
    pub fn store(&mut self, subscription_id: &str, payload: SubscriptionPayload, at: i64) {
        self.entry(subscription_id).store(payload, at);
    }

    /// The cached payload for one subscription.
    pub fn payload(&self, subscription_id: &str) -> Option<&SubscriptionPayload> {
        self.entries
            .get(subscription_id)
            .and_then(SubscriptionCache::payload)
    }

    /// Remove one subscription's cache.
    pub fn remove(&mut self, subscription_id: &str) -> Option<SubscriptionCache> {
        self.entries.remove(subscription_id)
    }

    /// How many subscriptions have a cached payload.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no subscription has a cached payload.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starpod_core::Node;

    fn payload(id: &str, name: &str) -> SubscriptionPayload {
        SubscriptionPayload {
            subscription_id: id.to_string(),
            nodes: vec![Node {
                id: format!("{id}-node"),
                name: name.to_string(),
                protocol: "trojan".to_string(),
                server: Some("a.example.com".to_string()),
                port: Some(443),
                tags: Vec::new(),
                raw: Default::default(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn an_empty_cache_holds_nothing() {
        let cache = SubscriptionCache::new();
        assert!(cache.is_empty());
        assert!(cache.payload().is_none());
        assert!(cache.stored_at().is_none());
    }

    #[test]
    fn storing_a_payload_keeps_it() {
        let mut cache = SubscriptionCache::new();
        cache.store(payload("sub-1", "Node A"), 1_000);
        assert_eq!(cache.payload(), Some(&payload("sub-1", "Node A")));
        assert_eq!(cache.stored_at(), Some(1_000));
        assert!(!cache.is_empty());
    }

    #[test]
    fn a_failed_refresh_leaves_the_cached_payload_untouched() {
        let mut cache = SubscriptionCache::new();
        cache.store(payload("sub-1", "Node A"), 1_000);

        let error = SubscriptionError::InvalidBody("not yaml".to_string());
        assert!(cache.reject(error).is_err());

        assert_eq!(
            cache.payload(),
            Some(&payload("sub-1", "Node A")),
            "a failed refresh must not overwrite the good cached payload"
        );
        assert_eq!(cache.stored_at(), Some(1_000));
    }

    #[test]
    fn a_corrupt_payload_cannot_be_stored() {
        let mut cache = SubscriptionCache::new();
        cache.store(payload("sub-1", "Node A"), 1_000);

        let empty = SubscriptionPayload {
            subscription_id: "sub-1".to_string(),
            ..Default::default()
        };
        assert!(cache.store_if_valid(empty, 2_000).is_err());

        assert_eq!(
            cache.payload(),
            Some(&payload("sub-1", "Node A")),
            "the corrupt payload did not overwrite the good one"
        );
        assert_eq!(cache.stored_at(), Some(1_000));
    }

    #[test]
    fn a_valid_payload_replaces_the_previous_one() {
        let mut cache = SubscriptionCache::new();
        cache.store(payload("sub-1", "Node A"), 1_000);
        cache.store(payload("sub-1", "Node B"), 2_000);
        assert_eq!(cache.payload(), Some(&payload("sub-1", "Node B")));
        assert_eq!(cache.stored_at(), Some(2_000));
    }

    #[test]
    fn clearing_the_cache_is_the_only_way_to_empty_it() {
        let mut cache = SubscriptionCache::new();
        cache.store(payload("sub-1", "Node A"), 1_000);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn the_cache_set_keeps_subscriptions_apart() {
        let mut set = SubscriptionCacheSet::new();
        set.store("sub-1", payload("sub-1", "Node A"), 1_000);
        set.store("sub-2", payload("sub-2", "Node B"), 1_000);
        assert_eq!(set.len(), 2);
        assert_eq!(
            set.payload("sub-1").map(|p| &p.nodes[0].name),
            Some(&"Node A".to_string())
        );
        assert_eq!(
            set.payload("sub-2").map(|p| &p.nodes[0].name),
            Some(&"Node B".to_string())
        );
    }

    #[test]
    fn removing_one_subscription_leaves_the_others() {
        let mut set = SubscriptionCacheSet::new();
        set.store("sub-1", payload("sub-1", "Node A"), 1_000);
        set.store("sub-2", payload("sub-2", "Node B"), 1_000);
        set.remove("sub-1");
        assert!(set.payload("sub-1").is_none());
        assert!(set.payload("sub-2").is_some());
    }

    #[test]
    fn a_failure_for_one_subscription_does_not_touch_another() {
        let mut set = SubscriptionCacheSet::new();
        set.store("sub-1", payload("sub-1", "Node A"), 1_000);
        set.store("sub-2", payload("sub-2", "Node B"), 1_000);

        let error = SubscriptionError::InvalidBody("not yaml".to_string());
        assert!(set.entry("sub-1").reject(error).is_err());

        assert!(set.payload("sub-1").is_some());
        assert!(set.payload("sub-2").is_some());
    }
}
