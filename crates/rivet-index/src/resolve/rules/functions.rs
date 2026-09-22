//! Lexically bound function calls (spec §11.3 `exact`; T19).
//!
//! An unqualified function `call` resolves the way PHP does: a function
//! declared in the use's own scope chain wins; otherwise `N\name` in the use's
//! namespace; otherwise the global `name`. When both a namespaced and a global
//! function exist, the namespaced one is correct, not ambiguous.
//!
//! A `use function` alias is owned by `imports.rs`; if an alias is visible but
//! its target is unindexed, this rule must not fall back to `N\name` or the
//! global name. Method and static calls carry a receiver and are outside T19.

use rivet_core::{RefKind, Resolution, SymbolKind};
use rivet_store::{SymbolRow, UseRow};

use crate::resolve::rules::imports::function_alias_visible;
use crate::resolve::{RuleCtx, ScopeFacts, SymbolId};

/// Resolves an unqualified function call.
pub(crate) fn resolve(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
) -> Option<(SymbolId, Resolution)> {
    if use_row.ref_kind != RefKind::Call || use_row.receiver.is_some() {
        return None;
    }
    // Qualified and fully qualified spellings are not unqualified calls.
    if use_row.spelling.contains('\\') {
        return None;
    }
    // A visible `use function` alias owns this name. `imports.rs` either bound
    // it or its target is unindexed, in which case the use stays unresolved.
    if function_alias_visible(use_row, facts) {
        return None;
    }

    // A function declared directly in the use's lexical scope chain shadows the
    // namespace/global fallback. More than one such declaration is ambiguous.
    let mut declared: Option<&SymbolRow> = None;
    for id in &facts.declares {
        let Some(row) = ctx.symbol_by_id(id) else {
            continue;
        };
        if row.kind != SymbolKind::Function || row.lookup_name != use_row.lookup_name {
            continue;
        }
        match declared {
            None => declared = Some(row),
            Some(existing) if existing.id == row.id => {}
            Some(_) => return None,
        }
    }
    if let Some(row) = declared {
        return Some((row.id.clone(), Resolution::Exact));
    }

    // Namespaced fallback wins over the global function.
    if let Some(namespace) = facts.namespace.as_deref().filter(|ns| !ns.is_empty()) {
        let qualified = format!("{namespace}\\{}", use_row.spelling);
        if let Some(row) = ctx.unique_function(&qualified) {
            return Some((row.id.clone(), Resolution::Exact));
        }
    }
    ctx.unique_function(&use_row.spelling)
        .map(|row| (row.id.clone(), Resolution::Exact))
}
