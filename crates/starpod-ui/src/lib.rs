// The Slint user interface for Starpod.
//
// `.slint` files declare properties and callbacks only. Every handler is a
// Rust function in `starpod-app` that mutates `AppState` and then pushes the
// new state to the UI (ARCHITECTURE §6.3).
//
// `ui/app.slint` is the single compilation unit. Everything it exports is
// generated into this crate: `AppWindow`, `AppTray`, the `Bridge` global and
// the structs it carries.

slint::include_modules!();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_model_defaults_are_zeroed() {
        let settings = SettingsModel::default();
        assert_eq!(settings.mixed_port, 0);
        assert!(!settings.allow_lan);
        assert!(!settings.tun_enabled);
    }

    #[test]
    fn subscription_item_is_constructible() {
        let item = SubscriptionItem {
            id: "sub-1".into(),
            label: "Main".into(),
            mode: "app-managed".into(),
            status: "ok".into(),
            node_count: 3,
            has_quota: true,
            quota_used_fraction: 0.25,
            ..Default::default()
        };
        assert_eq!(item.id, "sub-1");
        assert_eq!(item.node_count, 3);
        assert!(item.has_quota);
    }

    #[test]
    fn node_item_marks_untested_latency_as_minus_one() {
        let node = NodeItem {
            latency_ms: -1,
            ..Default::default()
        };
        assert_eq!(node.latency_ms, -1);
        assert!(!node.selected);
    }
}
