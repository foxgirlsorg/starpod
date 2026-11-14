//! Global keyboard shortcuts: the saved accelerators, their validation, and
//! their registration with the operating system (ARCHITECTURE §6.4).
//!
//! `global-hotkey` reports presses through a channel, and the manager belongs
//! to the thread that created it, which is the Slint thread. So the manager
//! lives in [`crate::ui::runtime`]'s UI state and presses are read from a
//! timer there. A press runs the same Bridge callback a button would, so a
//! shortcut and its button cannot disagree.
//!
//! Accelerators are written as `global-hotkey` reads them, for example
//! `Ctrl+Alt+KeyS` or `Ctrl+Shift+F5`. An empty field means no shortcut.

use std::str::FromStr;

use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{HotKey, HotKeyParseError},
};
use starpod_core::HotkeyBindings;
use starpod_ui::HotkeyModel;

use crate::AppError;

/// An app action a shortcut can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Show the window, or hide it when it is shown.
    ShowWindow,
    /// Connect when disconnected, disconnect when connected.
    ToggleConnect,
    /// Turn the system proxy on or off.
    ToggleSystemProxy,
    /// Route by the rules.
    ModeRule,
    /// Route everything through the proxy.
    ModeGlobal,
    /// Route everything directly.
    ModeDirect,
}

impl Action {
    /// Every action, in the order the settings form lists them.
    pub const ALL: [Action; 6] = [
        Action::ShowWindow,
        Action::ToggleConnect,
        Action::ToggleSystemProxy,
        Action::ModeRule,
        Action::ModeGlobal,
        Action::ModeDirect,
    ];

    /// The name shown in an error message.
    pub fn label(self) -> &'static str {
        match self {
            Action::ShowWindow => "show or hide the window",
            Action::ToggleConnect => "connect or disconnect",
            Action::ToggleSystemProxy => "the system proxy",
            Action::ModeRule => "rule mode",
            Action::ModeGlobal => "global mode",
            Action::ModeDirect => "direct mode",
        }
    }

    /// The accelerator the saved bindings give this action.
    fn accelerator(self, bindings: &HotkeyBindings) -> &str {
        match self {
            Action::ShowWindow => &bindings.show_window,
            Action::ToggleConnect => &bindings.toggle_connect,
            Action::ToggleSystemProxy => &bindings.toggle_system_proxy,
            Action::ModeRule => &bindings.mode_rule,
            Action::ModeGlobal => &bindings.mode_global,
            Action::ModeDirect => &bindings.mode_direct,
        }
    }
}

/// The bindings with each accelerator trimmed, after checking that every
/// non-empty one parses and that no two actions share one. A bad binding is
/// refused here, so it never reaches the saved preferences.
pub fn validate(bindings: &HotkeyBindings) -> Result<HotkeyBindings, AppError> {
    let trimmed = HotkeyBindings {
        show_window: bindings.show_window.trim().to_string(),
        toggle_connect: bindings.toggle_connect.trim().to_string(),
        toggle_system_proxy: bindings.toggle_system_proxy.trim().to_string(),
        mode_rule: bindings.mode_rule.trim().to_string(),
        mode_global: bindings.mode_global.trim().to_string(),
        mode_direct: bindings.mode_direct.trim().to_string(),
    };
    let mut seen: Vec<(HotKey, Action)> = Vec::new();
    for action in Action::ALL {
        let text = action.accelerator(&trimmed);
        if text.is_empty() {
            continue;
        }
        let hotkey = parse(text).map_err(|error| {
            AppError::Invalid(format!(
                "“{text}” is not a shortcut for {}: {error}",
                action.label()
            ))
        })?;
        if let Some((_, other)) = seen.iter().find(|(key, _)| *key == hotkey) {
            return Err(AppError::Invalid(format!(
                "{} and {} cannot share the shortcut “{text}”",
                other.label(),
                action.label()
            )));
        }
        seen.push((hotkey, action));
    }
    Ok(trimmed)
}

/// Parse one accelerator.
fn parse(text: &str) -> Result<HotKey, HotKeyParseError> {
    HotKey::from_str(text)
}

/// The form model for the saved bindings.
pub fn model_from(bindings: &HotkeyBindings) -> HotkeyModel {
    HotkeyModel {
        show_window: bindings.show_window.as_str().into(),
        toggle_connect: bindings.toggle_connect.as_str().into(),
        toggle_system_proxy: bindings.toggle_system_proxy.as_str().into(),
        mode_rule: bindings.mode_rule.as_str().into(),
        mode_global: bindings.mode_global.as_str().into(),
        mode_direct: bindings.mode_direct.as_str().into(),
    }
}

/// The bindings from the settings form. Call this on the Slint thread, where
/// the model's strings can be read.
pub fn bindings_from_model(model: &HotkeyModel) -> HotkeyBindings {
    HotkeyBindings {
        show_window: model.show_window.to_string(),
        toggle_connect: model.toggle_connect.to_string(),
        toggle_system_proxy: model.toggle_system_proxy.to_string(),
        mode_rule: model.mode_rule.to_string(),
        mode_global: model.mode_global.to_string(),
        mode_direct: model.mode_direct.to_string(),
    }
}

/// The shortcuts currently registered with the operating system.
#[derive(Default)]
pub struct Shortcuts {
    /// The manager. Created when the first shortcut is needed; it must stay
    /// alive for the shortcuts to work.
    manager: Option<GlobalHotKeyManager>,
    /// Each registered shortcut and the action it runs.
    registered: Vec<(HotKey, Action)>,
    /// The bindings last applied, so an unchanged set is not registered again.
    applied: Option<HotkeyBindings>,
}

impl Shortcuts {
    /// Make the registered shortcuts match `bindings`. Returns a message when
    /// something could not be registered, for example a key another program
    /// already holds or a platform with no global shortcuts.
    pub fn apply(&mut self, bindings: &HotkeyBindings) -> Option<String> {
        if self.applied.as_ref() == Some(bindings) {
            return None;
        }
        self.applied = Some(bindings.clone());
        self.unregister_all();

        let mut wanted = Vec::new();
        for action in Action::ALL {
            let text = action.accelerator(bindings);
            if text.is_empty() {
                continue;
            }
            match parse(text) {
                Ok(hotkey) => wanted.push((hotkey, action)),
                Err(error) => {
                    return Some(format!(
                        "the shortcut “{text}” for {} is not valid: {error}",
                        action.label()
                    ));
                }
            }
        }
        if wanted.is_empty() {
            return None;
        }

        if self.manager.is_none() {
            match GlobalHotKeyManager::new() {
                Ok(manager) => self.manager = Some(manager),
                Err(error) => {
                    return Some(format!(
                        "global shortcuts are not supported on this system: {error}"
                    ));
                }
            }
        }
        let manager = self.manager.as_ref()?;
        let mut failures = Vec::new();
        for (hotkey, action) in wanted {
            match manager.register(hotkey) {
                Ok(()) => self.registered.push((hotkey, action)),
                Err(error) => failures.push(format!("{}: {error}", action.label())),
            }
        }
        (!failures.is_empty()).then(|| {
            format!(
                "could not register the shortcut for {}",
                failures.join("; ")
            )
        })
    }

    /// Unregister every shortcut this value registered.
    fn unregister_all(&mut self) {
        if let Some(manager) = self.manager.as_ref() {
            for (hotkey, _) in self.registered.drain(..) {
                let _ = manager.unregister(hotkey);
            }
        }
        self.registered.clear();
    }

    /// The actions whose shortcut was pressed since the last call.
    pub fn drain_presses(&self) -> Vec<Action> {
        let receiver = GlobalHotKeyEvent::receiver();
        let mut actions = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if let Some((_, action)) = self.registered.iter().find(|(key, _)| key.id() == event.id)
            {
                actions.push(*action);
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings(show: &str, connect: &str) -> HotkeyBindings {
        HotkeyBindings {
            show_window: show.to_string(),
            toggle_connect: connect.to_string(),
            ..HotkeyBindings::default()
        }
    }

    #[test]
    fn empty_bindings_are_valid_and_stay_empty() {
        let valid = validate(&HotkeyBindings::default()).unwrap();
        assert!(valid.is_empty());
    }

    #[test]
    fn a_valid_accelerator_is_kept_trimmed() {
        let valid = validate(&bindings("  Ctrl+Alt+KeyS ", "")).unwrap();
        assert_eq!(valid.show_window, "Ctrl+Alt+KeyS");
    }

    #[test]
    fn an_unparseable_accelerator_is_refused_with_its_action() {
        let error = validate(&bindings("Ctrl+NotAKey", "")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("show or hide the window"), "{message}");
        assert!(message.contains("Ctrl+NotAKey"), "{message}");
    }

    #[test]
    fn two_actions_cannot_share_one_accelerator() {
        let error = validate(&bindings("Ctrl+Alt+KeyS", "Ctrl+Alt+KeyS")).unwrap_err();
        assert!(error.to_string().contains("cannot share"), "{error}");
    }

    #[test]
    fn the_model_round_trips_through_the_bindings() {
        let saved = bindings("Ctrl+Alt+KeyS", "Ctrl+Alt+KeyC");
        let model = model_from(&saved);
        assert_eq!(bindings_from_model(&model), saved);
    }
}
