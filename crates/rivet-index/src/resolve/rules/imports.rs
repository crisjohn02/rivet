//! Direct import and namespace-relative class binding (spec §11.3 `exact`;
//! T19).
//!
//! A `type` spelling that names an import alias visible in the use's scope
//! chain binds to the imported declaration; the same holds for the alias token
//! of the `use` itself and for a function call that names a `use function`
//! alias. A fully qualified spelling (`\App\Services\SurveyService`) binds
//! directly when exactly one declaration of the kind its use requires has that
//! qualified name (AF2): a type use only a class-like, a call only a function,
//! and a bare constant read only a constant. `class Maker {} \Maker();` records
//! nothing.
//!
//! When no import matches, an unqualified class spelling binds to the class of
//! that name in the use's namespace (`N\Foo`), because PHP resolves that
//! lexically. An alias whose target is not indexed yields no binding: the use
//! stays unresolved rather than falling back to spelling alone.

use rivet_core::extract::{ImportKind, ScopeImport};
use rivet_core::{RefKind, Resolution};
use rivet_store::{SymbolRow, UseRow};

use crate::resolve::{RuleCtx, ScopeFacts, SymbolId};

/// Which import kinds one lookup considers.
#[derive(Clone, Copy)]
enum ImportFilter {
    Any,
    Function,
    Const,
}

impl ImportFilter {
    fn accepts(self, kind: ImportKind) -> bool {
        match self {
            ImportFilter::Any => true,
            ImportFilter::Function => kind == ImportKind::Function,
            ImportFilter::Const => kind == ImportKind::Const,
        }
    }
}

/// The result of matching one use against the visible imports.
pub(crate) enum ImportOutcome<'a> {
    /// Exactly one indexed target was found.
    Bound(&'a SymbolRow),
    /// An alias matched, but its target was unindexed or ambiguous.
    Blocked,
    /// No visible alias matched this use.
    NoMatch,
}

/// Resolves a direct import or a namespace-relative class spelling.
pub(crate) fn resolve(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
) -> Option<(SymbolId, Resolution)> {
    // A class spelling shares its scope-chain lookup with `receivers.rs`.
    if use_row.ref_kind == RefKind::Type {
        return super::resolve_class_spelling(ctx, &use_row.spelling, facts)
            .map(|row| (row.id.clone(), Resolution::Exact));
    }

    // A fully qualified spelling is a direct lexical binding, but only to a
    // declaration of the kind the use requires (AF2): a call names a function,
    // a bare constant read (`Unknown`) a constant. A class of the same name is
    // never a call target.
    //
    // An `Unknown` use is also what the extractor records for a class name in
    // an unclassified position such as `instanceof \App\I`, so it cannot tell
    // a constant from a class-like of the same name; with both declared it
    // records nothing rather than guessing.
    if let Some(qname) = fully_qualified(&use_row.spelling) {
        let target = match use_row.ref_kind {
            RefKind::Call if use_row.receiver.is_none() => ctx.unique_function(qname),
            RefKind::Unknown if !ctx.any_class_like(qname) => ctx.unique_const(qname),
            _ => None,
        };
        return target.map(|row| (row.id.clone(), Resolution::Exact));
    }

    match use_row.ref_kind {
        RefKind::Import => match via_imports(ctx, use_row, facts, ImportFilter::Any) {
            ImportOutcome::Bound(row) => Some((row.id.clone(), Resolution::Exact)),
            ImportOutcome::Blocked | ImportOutcome::NoMatch => None,
        },
        RefKind::Call if use_row.receiver.is_none() => {
            match via_imports(ctx, use_row, facts, ImportFilter::Function) {
                ImportOutcome::Bound(row) => Some((row.id.clone(), Resolution::Exact)),
                // `functions.rs` owns the namespace/global fallback.
                ImportOutcome::Blocked | ImportOutcome::NoMatch => None,
            }
        }
        RefKind::Unknown => match via_imports(ctx, use_row, facts, ImportFilter::Const) {
            ImportOutcome::Bound(row) => Some((row.id.clone(), Resolution::Exact)),
            ImportOutcome::Blocked | ImportOutcome::NoMatch => None,
        },
        _ => None,
    }
}

/// The sole class-like declaration a visible class import alias names.
///
/// Shared by the `type` rule here and the typed-receiver rule in
/// `receivers.rs`, so both resolve an alias, a fully qualified name, and a
/// namespace-relative name identically.
pub(crate) fn class_via_imports<'a>(
    ctx: &RuleCtx<'a>,
    spelling: &str,
    facts: &ScopeFacts,
) -> ImportOutcome<'a> {
    let matching: Vec<&ScopeImport> = facts
        .imports
        .iter()
        .filter(|import| {
            import.kind == ImportKind::Class && alias_matches(&import.alias, spelling, import.kind)
        })
        .collect();
    if matching.is_empty() {
        return ImportOutcome::NoMatch;
    }
    import_targets(ctx, &matching)
}

/// Reports whether a non-`Import` use's spelling names a visible alias of a
/// considered kind, and resolves it.
///
/// For the alias token of a `use` declaration itself ([`RefKind::Import`]) the
/// binding is matched by span, which is exact and avoids aggregating aliases of
/// other kinds that happen to share the spelling.
fn via_imports<'a>(
    ctx: &RuleCtx<'a>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    filter: ImportFilter,
) -> ImportOutcome<'a> {
    let matching: Vec<&ScopeImport> = facts
        .imports
        .iter()
        .filter(|import| {
            if !filter.accepts(import.kind) {
                return false;
            }
            if use_row.ref_kind == RefKind::Import {
                import.span.start_byte() == use_row.start_byte
                    && import.span.end_byte() == use_row.end_byte
            } else {
                alias_matches(&import.alias, &use_row.spelling, import.kind)
            }
        })
        .collect();
    if matching.is_empty() {
        return ImportOutcome::NoMatch;
    }
    import_targets(ctx, &matching)
}

/// Resolves the indexed declarations named by already-matched imports.
fn import_targets<'a>(ctx: &RuleCtx<'a>, matching: &[&ScopeImport]) -> ImportOutcome<'a> {
    let mut targets: Vec<&SymbolRow> = Vec::new();
    for import in matching {
        let target = match import.kind {
            ImportKind::Class => ctx.unique_class_like(trim_leading(&import.target_qualified)),
            ImportKind::Function => ctx.unique_function(trim_leading(&import.target_qualified)),
            ImportKind::Const => ctx.unique_const(trim_leading(&import.target_qualified)),
        };
        if let Some(row) = target
            && !targets.iter().any(|existing| existing.id == row.id)
        {
            targets.push(row);
        }
    }
    match targets.len() {
        1 => ImportOutcome::Bound(targets[0]),
        // Zero indexed targets or a conflict: never guess.
        _ => ImportOutcome::Blocked,
    }
}

/// Whether `functions.rs` must leave a call unresolved because a `use function`
/// alias owns the name (imports.rs either bound it or its target is unindexed).
pub(crate) fn function_alias_visible(use_row: &UseRow, facts: &ScopeFacts) -> bool {
    facts.imports.iter().any(|import| {
        import.kind == ImportKind::Function
            && alias_matches(&import.alias, &use_row.spelling, import.kind)
    })
}

/// Resolves a visible `use function` alias by spelling alone.
///
/// [`via_imports`] needs a [`UseRow`] only to match the alias token of an
/// `Import` use by span; call-argument adjudication (T21b) has no `UseRow` and
/// matches by spelling like every other non-`Import` use.
pub(crate) fn function_via_imports<'a>(
    ctx: &RuleCtx<'a>,
    facts: &ScopeFacts,
    spelling: &str,
) -> ImportOutcome<'a> {
    let matching: Vec<&ScopeImport> = facts
        .imports
        .iter()
        .filter(|import| {
            import.kind == ImportKind::Function
                && alias_matches(&import.alias, spelling, import.kind)
        })
        .collect();
    if matching.is_empty() {
        return ImportOutcome::NoMatch;
    }
    import_targets(ctx, &matching)
}

/// PHP's alias comparison: constants are case-sensitive, classes and functions
/// are not, and PHP folds ASCII letters only (AF2).
fn alias_matches(alias: &str, spelling: &str, kind: ImportKind) -> bool {
    match kind {
        ImportKind::Const => alias == spelling,
        ImportKind::Class | ImportKind::Function => alias.eq_ignore_ascii_case(spelling),
    }
}

/// Binds an unqualified class spelling to `N\Foo` in the use's namespace.
pub(crate) fn namespace_relative_class<'a>(
    ctx: &RuleCtx<'a>,
    spelling: &str,
    facts: &ScopeFacts,
) -> Option<&'a SymbolRow> {
    if spelling.contains('\\') {
        // `namespace\Foo` and other relative forms are outside T19.
        return None;
    }
    let candidate = match facts.namespace.as_deref() {
        Some(namespace) if !namespace.is_empty() => format!("{namespace}\\{spelling}"),
        _ => spelling.to_string(),
    };
    ctx.unique_class_like(&candidate)
}

/// The name inside a leading-backslash fully qualified spelling, if any.
pub(crate) fn fully_qualified(spelling: &str) -> Option<&str> {
    let trimmed = spelling.strip_prefix('\\')?;
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Strips a leading backslash from an imported qualified name.
fn trim_leading(qname: &str) -> &str {
    qname.trim_start_matches('\\')
}
