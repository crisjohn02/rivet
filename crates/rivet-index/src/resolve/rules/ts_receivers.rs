//! TypeScript receiver hints: `this`, explicit annotations, and `new` (spec
//! §11.3 `scoped`, §11.4 rules 1-3; T45).
//!
//! Binds a member use (a call, read, or write through a receiver) whose hint
//! names its receiver's class, the way [`receivers`](super::receivers) and
//! [`new_expr`](super::new_expr) do for PHP:
//!
//! - **`this`** ([`UseHint::This`]): the named class enclosing the use, found
//!   through the use's container (a method's class, or the class itself for a
//!   field initializer or static block; arrow functions are transparent). The
//!   extractor records the hint only where `this` is that class.
//! - **Typed** ([`UseHint::Typed`], a parameter, field, constructor parameter
//!   property, or variable annotation) and **`new`** ([`UseHint::NewExpr`], a
//!   `const` bound by `new C(...)`): the class the annotation's type name or
//!   the `new` target names. The hint carries the span of that name's `type`
//!   use, and the class is whatever the T44 lexical rules
//!   ([`ts_lexical`](super::ts_lexical), [`ts_namespace`](super::ts_namespace))
//!   bind that use to, so the name is looked up in the scope where it is
//!   written, never the member use's scope: same-file declarations with
//!   shadowing and value/type spaces, direct relative imports, and
//!   namespace-import members. An annotation needs a type-space declaration
//!   and a `new` target a value-space one, as T44 already decides for those
//!   uses. A generic type parameter of the same name hides the class.
//!
//! The declaration must be exactly one class or interface: a type alias, an
//! enum, a namespace, a function, an unresolved or ambiguous name (including
//! declaration merging such as `class X` plus `interface X`), a package or
//! path-alias import, and a re-export give no receiver class.
//!
//! **Members.** The receiver class must itself declare exactly one method or
//! property of the use's spelling (case-sensitive, an ES private name with
//! its `#`) on the receiver's side: `this` in a static member, static block,
//! or static field initializer binds only static members, and every other
//! receiver only instance members. A member whose side the facts do not
//! record (a constructor) blocks the binding. Inheritance is never traversed,
//! so a member declared only on a base class stays unresolved, and an
//! interface receiver binds the interface's own member, never an implementing
//! class's (§11.4). A getter and setter sharing a name are two candidates, so
//! a use of that name stays unresolved: uses record no read/write distinction.
//!
//! Every binding is [`Resolution::Scoped`]: structural typing and dispatch can
//! select another implementation, so receiver evidence never upgrades.

use rivet_core::extract::UseHint;
use rivet_core::{RefKind, Resolution, Span, SymbolKind};
use rivet_store::{SymbolRow, UseRow};

use super::{ts_lexical, ts_namespace};
use crate::resolve::{RuleCtx, ScopeFacts, SymbolId};

/// Binds a member use through its `this`, typed, or `new` receiver hint.
pub(crate) fn resolve(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    _facts: &ScopeFacts,
) -> Option<(SymbolId, Resolution)> {
    let receiver = use_row.receiver.as_deref()?;
    if !matches!(
        use_row.ref_kind,
        RefKind::Call | RefKind::Read | RefKind::Write
    ) {
        return None;
    }
    let hint: UseHint = serde_json::from_str(&use_row.hint_json).ok()?;
    let (class, is_static) = match hint {
        UseHint::This {
            is_static: Some(is_static),
        } if receiver == "this" => (enclosing_class(ctx, use_row)?, is_static),
        UseHint::Typed {
            name_span: Some(span),
            ..
        }
        | UseHint::NewExpr {
            name_span: Some(span),
            ..
        } => (named_class(ctx, &use_row.file, span)?, false),
        _ => return None,
    };
    let member = own_member(ctx, class, &use_row.spelling, is_static)?;
    Some((member.id.clone(), Resolution::Scoped))
}

/// The named class whose body holds `use_row`: its container when that is a
/// class (a field initializer, a static block), or the class of its container
/// method.
fn enclosing_class<'a>(ctx: &RuleCtx<'a>, use_row: &UseRow) -> Option<&'a SymbolRow> {
    let container = ctx.symbol_by_id(use_row.containing_symbol.as_deref()?)?;
    let class = match container.kind {
        SymbolKind::Class => container,
        SymbolKind::Method => ctx.symbol_by_id(container.parent_id.as_deref()?)?,
        _ => return None,
    };
    (class.kind == SymbolKind::Class && class.file == use_row.file).then_some(class)
}

/// The one class or interface the `type` use at `span` of `file` binds to
/// through the T44 lexical rules, in that use's own scope.
fn named_class<'a>(ctx: &RuleCtx<'a>, file: &str, span: Span) -> Option<&'a SymbolRow> {
    let name = ctx.typescript.use_at(file, span)?;
    if name.ref_kind != RefKind::Type {
        return None;
    }
    let facts = ScopeFacts::default();
    let (id, _) = ts_lexical::resolve(ctx, name, &facts)
        .or_else(|| ts_namespace::resolve(ctx, name, &facts))?;
    let class = ctx.symbol_by_id(&id)?;
    matches!(class.kind, SymbolKind::Class | SymbolKind::Interface).then_some(class)
}

/// The one method or property `class` itself declares under `spelling` on
/// the static side when `is_static`, the instance side otherwise. `None` when
/// there is none, more than one, or a same-name member of unknown side.
fn own_member<'a>(
    ctx: &RuleCtx<'a>,
    class: &SymbolRow,
    spelling: &str,
    is_static: bool,
) -> Option<&'a SymbolRow> {
    let mut found: Option<&'a SymbolRow> = None;
    for &member in ctx.typescript.members(&class.id) {
        if !matches!(member.kind, SymbolKind::Method | SymbolKind::Property)
            || member.name != spelling
        {
            continue;
        }
        if ctx.typescript.member_side(&member.id)? != is_static {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(member);
    }
    found
}
