//! The Slint side of Starpod.
//!
//! - [`mapping`] turns the service's snapshot into the `Bridge` contract.
//!   It is pure, and it is where the unit tests are.
//! - [`callbacks`] maps every Bridge callback to a service call.
//! - [`runtime`] builds the window and tray, runs the event loop, and moves
//!   results from the tokio runtime to the Slint thread.

pub mod callbacks;
pub mod demo;
pub mod emoji;
pub mod hotkeys;
pub mod mapping;
pub mod runtime;
pub mod tray;

pub use runtime::{Launch, run};
