// The design tokens, the components and the view models for Starpod.
//
// `.slint` files declare properties and callbacks only. Every handler is a
// Rust function here or in `starpod-app` that mutates `AppState` and then
// pushes the new state to the UI (ARCHITECTURE §6.3).

pub mod view;

slint::include_modules!();
