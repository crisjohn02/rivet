//! TypeScript namespace-import members (spec §11.3 `exact`; T44).
//!
//! After `import * as ns from "./m"`, `ns.member` (a call, a read or write, a
//! `new ns.C()` target, or a type-position `ns.Member`) binds `member` to the
//! declaration `./m` exports as `member`, in the member use's space. This is
//! a lexical module-qualified name, not member access on an object, so the
//! receiver-evidence clause of spec §11.4 rule 4 does not apply: `ns` is
//! bound lexically to a module, and the module's exports are its members.
//!
//! The receiver must be exactly the namespace import's local name, found as
//! the nearest binding of that name in the use's space, so a local that
//! shadows `ns` leaves the use unresolved. `ns.member.deeper` binds only
//! `member`: `deeper`'s receiver is `ns.member`, not a namespace import. A
//! name the module does not export locally (`ns.missing`, a re-export, an
//! `export *` name) stays unresolved, as does every other receiver
//! (`this.x`, `obj.m()`, a default or named import used as an object):
//! T45 owns receivers.

use rivet_core::extract::ModuleImportKind;
use rivet_core::{RefKind, Resolution};
use rivet_store::UseRow;

use super::ts_scopes::{self, Binding};
use crate::resolve::{RuleCtx, ScopeFacts, SymbolId};

/// Binds a member of a namespace import.
pub(crate) fn resolve(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    _facts: &ScopeFacts,
) -> Option<(SymbolId, Resolution)> {
    let receiver = use_row.receiver.as_deref()?;
    if !is_identifier(receiver)
        || !matches!(
            use_row.ref_kind,
            RefKind::Call | RefKind::Read | RefKind::Write | RefKind::Type | RefKind::Unknown
        )
    {
        return None;
    }
    let space = ts_scopes::use_space(ctx, use_row);
    let Binding::Imports {
        module_scope: true,
        imports,
    } = ts_scopes::binding_of(ctx, &use_row.file, &use_row.scope_key, receiver, space)
    else {
        return None;
    };
    let [import] = imports.as_slice() else {
        return None;
    };
    if import.kind != ModuleImportKind::Namespace {
        return None;
    }
    let module = ts_scopes::lookup_module(ctx, &use_row.file, &import.specifier)?;
    let target = ts_scopes::exported(ctx, module, &use_row.spelling, space)?;
    if ts_scopes::merged_body_conflict(ctx, use_row, receiver, target) {
        return None;
    }
    Some((target.id.clone(), Resolution::Exact))
}

/// Whether `text` is one plain identifier (not `this` or `super`), the only
/// receiver a namespace import can be.
fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first == '$' || first.is_alphabetic())
        && chars.all(|c| c == '_' || c == '$' || c.is_alphanumeric())
        && !matches!(text, "this" | "super")
}
