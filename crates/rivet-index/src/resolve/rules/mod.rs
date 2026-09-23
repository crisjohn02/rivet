//! One ordered resolution rule per file (T19/T20 design).
//!
//! T19 provides [`imports`] and [`functions`]. T20 adds [`receivers`] and T21
//! adds [`new_expr`]; each is registered in `resolve/mod.rs` in that order.
//! [`rebinding`] is not a rule: it holds the local-variable rebinding checks
//! that [`receivers`] and [`new_expr`] share (AF3).

pub(crate) mod functions;
pub(crate) mod imports;
pub(crate) mod new_expr;
pub(crate) mod php_builtins;
pub(crate) mod rebinding;
pub(crate) mod receivers;

use std::collections::BTreeSet;

use rivet_core::extract::ImportKind;
use rivet_store::SymbolRow;

use crate::resolve::{RuleCtx, ScopeFacts};

/// The class a receiver rule determined for a member or scoped use (LR2).
///
/// A receiver rule binds a member only through [`ClassEvidence::Indexed`].
/// Both variants are recorded for a use no rule bound, so reference mode can
/// exclude a same-name use whose receiver class is unrelated to the target's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClassEvidence<'a> {
    /// The one indexed class-like the class spelling resolves to.
    Indexed(&'a SymbolRow),
    /// No indexed class-like has the spelling's qualified name, which PHP's
    /// compile-time name resolution fixes as this value.
    Named(String),
}

/// The class a class-name spelling names within one use's scope chain, as
/// [`ClassEvidence`] (LR2).
///
/// The indexed class [`resolve_class_spelling`] finds, when it finds one.
/// Otherwise the spelling's PHP compile-time qualified name
/// ([`php_qualified_name`]), but only when no indexed class-like has that
/// name: an indexed name the lookup still refused (duplicate declarations, a
/// conflicting import) is not evidence. `self`, `static`, and `parent` name
/// no class by their spelling, and an untrustworthy namespace or a conflicting
/// alias gives no qualified name, so each yields `None`.
pub(crate) fn class_evidence<'a>(
    ctx: &RuleCtx<'a>,
    spelling: &str,
    facts: &ScopeFacts,
) -> Option<ClassEvidence<'a>> {
    if let Some(row) = resolve_class_spelling(ctx, spelling, facts) {
        return Some(ClassEvidence::Indexed(row));
    }
    if matches!(
        spelling.to_ascii_lowercase().as_str(),
        "self" | "static" | "parent"
    ) {
        return None;
    }
    let qname = php_qualified_name(spelling, facts)?;
    if ctx.any_class_like(&qname) {
        return None;
    }
    Some(ClassEvidence::Named(qname))
}

/// PHP's compile-time resolution of a class-name spelling (PHP manual,
/// "Name resolution rules"), without a leading `\` (T36d; shared with
/// [`crate::hierarchy`] by LR2).
///
/// - `\A\B` is already fully qualified.
/// - `namespace\A` is relative to the current namespace.
/// - Otherwise the first segment is looked up among the visible class
///   imports (case-insensitively); a match replaces it with the import's
///   target.
/// - Otherwise the current namespace is prefixed.
///
/// `None` when the namespace cannot be attributed (AF1) or when imports with
/// different targets claim the first segment, which PHP rejects and the index
/// never guesses between.
pub(crate) fn php_qualified_name(spelling: &str, facts: &ScopeFacts) -> Option<String> {
    if facts.namespace_unattributed {
        return None;
    }
    if let Some(rest) = spelling.strip_prefix('\\') {
        return (!rest.is_empty()).then(|| rest.to_string());
    }
    let namespace = facts.namespace.as_deref().unwrap_or("");
    let prefixed = |name: &str| {
        if namespace.is_empty() {
            name.to_string()
        } else {
            format!("{namespace}\\{name}")
        }
    };
    if let Some((first, rest)) = spelling.split_once('\\')
        && first.eq_ignore_ascii_case("namespace")
    {
        return (!rest.is_empty()).then(|| prefixed(rest));
    }
    let (first, rest) = match spelling.split_once('\\') {
        Some((first, rest)) => (first, Some(rest)),
        None => (spelling, None),
    };
    let targets: BTreeSet<&str> = facts
        .imports
        .iter()
        .filter(|import| {
            import.kind == ImportKind::Class && import.alias.eq_ignore_ascii_case(first)
        })
        .map(|import| import.target_qualified.trim_start_matches('\\'))
        .collect();
    match targets.len() {
        0 => Some(prefixed(spelling)),
        1 => {
            let target = targets.into_iter().next()?;
            Some(match rest {
                Some(rest) => format!("{target}\\{rest}"),
                None => target.to_string(),
            })
        }
        _ => None,
    }
}

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
