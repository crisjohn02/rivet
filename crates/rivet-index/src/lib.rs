//! Declaration resolution, query matching, context ranking and fitting.
//!
//! T13 completes the symbol query forms of spec §10.2 against persisted rows:
//! canonical ID, native qualified name, `file:line`, dotted path, and short
//! name, plus deterministic ambiguity ordering, not-found suggestions, and
//! query normalization hardening. The forms live in the [`query`] module.

mod query;

pub use query::{QueryOutcome, levenshtein, resolve_query, suggestions};
