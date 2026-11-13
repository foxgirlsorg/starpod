//! The rule model: what a rule *is*, how it is written, and how it is read.
//!
//! Three layers, deliberately separated:
//!
//! - [`grammar`] — the tokens the engine accepts, mirrored one-to-one;
//! - [`model`] — the user-facing tree the Rules screen edits;
//! - [`translate`] / [`parse`] — the two directions between the tree and the
//!   engine's flat rule lines.
//!
//! Nothing here performs I/O, and nothing here knows about the GUI or the
//! proxy engine, so the whole rule layer is testable on its own.

pub mod grammar;
pub mod model;
pub mod parse;
pub mod translate;

pub use grammar::{BuiltinTarget, GeoDataKind, RuleFlags, RuleKind};
pub use model::{LogicKind, RuleSpec, RuleTarget};
pub use parse::{ParseError, parse_block, parse_line};
pub use translate::{
    dead_payload_reason, has_unreachable_tail, match_index, translate, translate_all,
};
