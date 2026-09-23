//! Declaration resolution, query matching, context ranking and fitting.
//!
//! T13 completes the symbol query forms of spec §10.2 against persisted rows:
//! canonical ID, native qualified name, `file:line`, dotted path, and short
//! name, plus deterministic ambiguity ordering, not-found suggestions, and
//! query normalization hardening. The forms live in the [`query`] module.
//!
//! T19 adds [`resolve`]: direct PHP import/namespace bindings and lexically
//! bound function references, resolved from the persisted uses and scopes.
//!
//! T36d adds [`hierarchy`]: each class-like's declared supertypes, resolved
//! through the same bindings as their `type` uses, and their transitive
//! closure.
//!
//! LR2 adds [`exclusion`]: the two kinds of evidence that let reference mode
//! leave out a same-name unresolved use that cannot refer to the target.

pub mod exclusion;
pub mod hierarchy;
mod query;
pub mod resolve;

pub use exclusion::{ClassRelation, SubtypeIndex, form_compatible, possibly_trait};
pub use hierarchy::{Hierarchy, Supertype, ancestors, direct_supertypes};
pub use query::{
    InvalidFileLine, QueryOutcome, check_query_syntax, levenshtein, lookup_name_matches,
    resolve_query, suggestions,
};
pub use resolve::{ResolvedLinks, Resolver, resolve_all, resolve_all_links, unindexed_php_files};
