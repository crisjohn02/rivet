//! TypeScript direct imports and same-file lexical bindings (spec §11.3
//! `exact`, §11.4 rule 4; T44).
//!
//! Binds a use written without a receiver:
//!
//! - **An `import` use** (the local name an import binding creates) binds to
//!   the declaration its import names: a named or aliased import to what the
//!   module exports under the imported name, following the module's own
//!   `export { x as a }` renames; a default import to the module's default
//!   export when that identifies one explicit local declaration. `import
//!   type` binds the same way. The name must identify one declaration across
//!   both spaces. A re-export specifier's `import` use, a namespace import,
//!   and a `require` import bind nothing.
//! - **Any other use** (a call, a `type` use, a bare write, an `unknown` use)
//!   binds through the nearest scope that binds its name in its space, with
//!   shadowing accounted for ([`ts_scopes::binding_of`]): a same-file
//!   declaration that scope declares, or the declaration an import binding
//!   of the module scope names, exactly as its `import` use binds.
//!
//! Everything else stays unresolved: a name bound by a local that is no
//! symbol (a parameter or a function-local `const double` hides an imported
//! `double`), a global, a re-export, `export *`, a path alias or package
//! specifier, `require`, a name the module does not export, an unresolved or
//! ambiguous module, and more than one candidate declaration. A use with a
//! receiver is [`ts_namespace`](super::ts_namespace)'s, or
//! [`ts_receivers`](super::ts_receivers)'s (T45).

use rivet_core::{RefKind, Resolution};
use rivet_store::{SymbolRow, UseRow};

use super::ts_scopes::{self, Binding, MODULE_SCOPE, Space};
use crate::resolve::{RuleCtx, ScopeFacts, SymbolId};

/// Binds an `import` use or a receiver-less use of a lexically bound name.
pub(crate) fn resolve(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    _facts: &ScopeFacts,
) -> Option<(SymbolId, Resolution)> {
    let target = if use_row.ref_kind == RefKind::Import {
        import_use(ctx, use_row)?
    } else {
        lexical_use(ctx, use_row)?
    };
    Some((target.id.clone(), Resolution::Exact))
}

/// The declaration the import binding an `import` use sits on names.
fn import_use<'a>(ctx: &RuleCtx<'a>, use_row: &UseRow) -> Option<&'a SymbolRow> {
    if use_row.scope_key != MODULE_SCOPE {
        return None;
    }
    let scope = ctx.typescript.scope(&use_row.file, &use_row.scope_key)?;
    let mut matching = scope.imports().iter().filter(|import| {
        import.span.start_byte() == use_row.start_byte && import.span.end_byte() == use_row.end_byte
    });
    let import = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    // A re-export binds no local name, and re-exports are not followed.
    import.local.as_ref()?;
    ts_scopes::import_target(ctx, &use_row.file, import, Space::Any)
}

/// The declaration a receiver-less use's name is lexically bound to.
fn lexical_use<'a>(ctx: &RuleCtx<'a>, use_row: &UseRow) -> Option<&'a SymbolRow> {
    if use_row.receiver.is_some()
        || !matches!(
            use_row.ref_kind,
            RefKind::Call | RefKind::Type | RefKind::Unknown | RefKind::Write | RefKind::Read
        )
    {
        return None;
    }
    let space = ts_scopes::use_space(ctx, use_row);
    let name = use_row.spelling.as_str();
    let target = match ts_scopes::binding_of(ctx, &use_row.file, &use_row.scope_key, name, space) {
        Binding::Locals { scope, locals } => {
            ts_scopes::declaration_of_locals(ctx, &use_row.file, scope, &locals, name, space)?
        }
        Binding::Imports {
            module_scope,
            imports,
        } => ts_scopes::declaration_of_imports(ctx, &use_row.file, module_scope, &imports, space)?,
        Binding::Unbound | Binding::Unknowable => return None,
    };
    if ts_scopes::merged_body_conflict(ctx, use_row, name, target) {
        return None;
    }
    Some(target)
}
