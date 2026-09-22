//! One ordered resolution rule per file (T19/T20 design).
//!
//! T19 provides [`imports`] and [`functions`]. T20 adds [`receivers`] and T21
//! adds [`new_expr`]; each is registered in `resolve/mod.rs` in that order.

pub(crate) mod functions;
pub(crate) mod imports;
pub(crate) mod new_expr;
pub(crate) mod receivers;

use rivet_store::SymbolRow;

use crate::resolve::{RuleCtx, ScopeFacts};

/// Resolves a class-name spelling the way PHP resolves it within one use's
/// scope chain: a fully qualified spelling first, then a visible class import
/// alias, then the class of that name in the use's namespace.
///
/// [`imports`] (a `type` use) and [`receivers`] (an explicit receiver type)
/// share this lookup. `None` means no unique indexed class, including the case
/// where a visible alias owns the name but its target is unindexed.
pub(crate) fn resolve_class_spelling<'a>(
    ctx: &RuleCtx<'a>,
    spelling: &str,
    facts: &ScopeFacts,
) -> Option<&'a SymbolRow> {
    if let Some(qname) = imports::fully_qualified(spelling) {
        return ctx.unique_class_like(qname);
    }
    match imports::class_via_imports(ctx, spelling, facts) {
        imports::ImportOutcome::Bound(row) => Some(row),
        imports::ImportOutcome::Blocked => None,
        imports::ImportOutcome::NoMatch => imports::namespace_relative_class(ctx, spelling, facts),
    }
}
