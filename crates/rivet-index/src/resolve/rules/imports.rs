//! Direct import and namespace-relative class binding (spec §11.3 `exact`;
//! T19).
//!
//! A `type` spelling that names an import alias visible in the use's scope
//! chain binds to the imported declaration; the same holds for the alias token
//! of the `use` itself and for a function call that names a `use function`
//! alias. A fully qualified spelling (`\App\Services\SurveyService`) binds
//! directly when exactly one symbol has that qualified name.
//!
//! When no import matches, an unqualified class spelling binds to the class of
//! that name in the use's namespace (`N\Foo`), because PHP resolves that
//! lexically. An alias whose target is not indexed yields no binding: the use
//! stays unresolved rather than falling back to spelling alone.

use rivet_core::extract::ImportKind;
use rivet_core::{RefKind, Resolution};
use rivet_store::UseRow;

use crate::resolve::{RuleCtx, ScopeFacts, SymbolId};

/// Which import kinds one lookup considers.
#[derive(Clone, Copy)]
enum ImportFilter {
    Any,
    Class,
    Function,
    Const,
}

impl ImportFilter {
    fn accepts(self, kind: ImportKind) -> bool {
        match self {
            ImportFilter::Any => true,
            ImportFilter::Class => kind == ImportKind::Class,
            ImportFilter::Function => kind == ImportKind::Function,
            ImportFilter::Const => kind == ImportKind::Const,
        }
    }
}

/// The result of matching one use against the visible imports.
enum ImportOutcome {
    /// Exactly one indexed target was found.
    Bound((SymbolId, Resolution)),
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
    // A fully qualified spelling is a direct lexical binding, whatever kind it
    // names (class, function, or constant).
    if let Some(qname) = fully_qualified(&use_row.spelling) {
        return ctx
            .unique_resolved(qname)
            .map(|row| (row.id.clone(), Resolution::Exact));
    }

    match use_row.ref_kind {
        RefKind::Type => match via_imports(ctx, use_row, facts, ImportFilter::Class) {
            ImportOutcome::Bound(binding) => Some(binding),
            // A visible alias owns the name; an unindexed target stays
            // unresolved rather than becoming a namespace-relative class.
            ImportOutcome::Blocked => None,
            ImportOutcome::NoMatch => namespace_relative_class(ctx, use_row, facts),
        },
        RefKind::Import => match via_imports(ctx, use_row, facts, ImportFilter::Any) {
            ImportOutcome::Bound(binding) => Some(binding),
            ImportOutcome::Blocked | ImportOutcome::NoMatch => None,
        },
        RefKind::Call if use_row.receiver.is_none() => {
            match via_imports(ctx, use_row, facts, ImportFilter::Function) {
                ImportOutcome::Bound(binding) => Some(binding),
                // `functions.rs` owns the namespace/global fallback.
                ImportOutcome::Blocked | ImportOutcome::NoMatch => None,
            }
        }
        RefKind::Unknown => match via_imports(ctx, use_row, facts, ImportFilter::Const) {
            ImportOutcome::Bound(binding) => Some(binding),
            ImportOutcome::Blocked | ImportOutcome::NoMatch => None,
        },
        _ => None,
    }
}

/// Reports whether a non-`Import` use's spelling names a visible alias of a
/// considered kind, and resolves it.
///
/// For the alias token of a `use` declaration itself ([`RefKind::Import`]) the
/// binding is matched by span, which is exact and avoids aggregating aliases of
/// other kinds that happen to share the spelling.
fn via_imports(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    filter: ImportFilter,
) -> ImportOutcome {
    let mut matching = Vec::new();
    for import in &facts.imports {
        if !filter.accepts(import.kind) {
            continue;
        }
        let matches = if use_row.ref_kind == RefKind::Import {
            import.span.start_byte() == use_row.start_byte
                && import.span.end_byte() == use_row.end_byte
        } else {
            alias_matches(&import.alias, &use_row.spelling, import.kind)
        };
        if matches {
            matching.push(import);
        }
    }
    if matching.is_empty() {
        return ImportOutcome::NoMatch;
    }

    let mut targets: Vec<&rivet_store::SymbolRow> = Vec::new();
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
        1 => ImportOutcome::Bound((targets[0].id.clone(), Resolution::Exact)),
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

/// PHP's alias comparison: constants are case-sensitive, classes and functions
/// are not.
fn alias_matches(alias: &str, spelling: &str, kind: ImportKind) -> bool {
    match kind {
        ImportKind::Const => alias == spelling,
        ImportKind::Class | ImportKind::Function => alias.to_lowercase() == spelling.to_lowercase(),
    }
}

/// Binds an unqualified class spelling to `N\Foo` in the use's namespace.
fn namespace_relative_class(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
) -> Option<(SymbolId, Resolution)> {
    let spelling = &use_row.spelling;
    if spelling.contains('\\') {
        // `namespace\Foo` and other relative forms are outside T19.
        return None;
    }
    let candidate = match facts.namespace.as_deref() {
        Some(namespace) if !namespace.is_empty() => format!("{namespace}\\{spelling}"),
        _ => spelling.clone(),
    };
    ctx.unique_class_like(&candidate)
        .map(|row| (row.id.clone(), Resolution::Exact))
}

/// The name inside a leading-backslash fully qualified spelling, if any.
fn fully_qualified(spelling: &str) -> Option<&str> {
    let trimmed = spelling.strip_prefix('\\')?;
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Strips a leading backslash from an imported qualified name.
fn trim_leading(qname: &str) -> &str {
    qname.trim_start_matches('\\')
}
