//! Shared local-variable rebinding checks (T21b, moved and extended in AF3).
//!
//! Both receiver rules that trust a local variable use these checks: the
//! `new`-receiver rule in [`new_expr`](super::new_expr) and the
//! typed-parameter branch of [`receivers`](super::receivers). AF3 moved the
//! T21b by-reference adjudication here from `new_expr.rs` so the two rules
//! share one copy:
//!
//! - `call_arg_rebinds_at` decides whether a recorded [`CallArg`] may rebind the
//!   variable it passes, by the callee's indexed parameter list, the
//!   [`php_builtins`] table, or, failing both, by suppressing;
//! - [`local_untrustworthy`] adds the other conditions a trusted typed
//!   parameter needs: no unanalysable construct in the scope and no recorded
//!   rebinding of the variable anywhere in it; and
//! - [`new_receiver_trusted`] holds the `new`-receiver conditions (a)-(g),
//!   moved unchanged from `new_expr.rs` so a call argument's receiver can be
//!   checked by them too.
//!
//! A method-call argument whose receiver class came from a local hint
//! ([`CallReceiver::Variable`]) is adjudicated only when that receiver passes
//! the same check its own use would (AF3). The check is one level deep: while
//! checking a receiver, any of the receiver's own method-call arguments whose
//! receiver is again a hinted variable counts as rebinding, so adjudication
//! never recurses and never loops.
//!
//! AF3 also changed how a parameter list is read: from per-declaration flags
//! recorded from the parse tree (`RuleCtx::parameter_by_ref`) rather than by
//! re-parsing signature text, and added constructor arguments
//! ([`CallArgKind::Constructor`]), adjudicated against `Class::__construct`
//! exactly as a method call is.

use rivet_core::SymbolKind;
use rivet_core::extract::{CallArg, CallArgKind, CallReceiver, NewBinding, ReceiverEvidence};
use rivet_store::{SymbolRow, UseRow};

use crate::resolve::rules::imports::{self, ImportOutcome};
use crate::resolve::rules::php_builtins;
use crate::resolve::rules::receivers::enclosing_class;
use crate::resolve::rules::resolve_class_spelling;
use crate::resolve::{MemberUse, RuleCtx, ScopeFacts};

/// Whether `variable` cannot be trusted to still hold the value its one
/// trusted binding gave it, anywhere in the use's own scope.
///
/// True when the scope is unanalysable, when any rebinding of the variable is
/// recorded in the scope (in any position, before or after the use, so a
/// reassignment later in a loop body counts), or when a call argument may
/// rebind it. A typed parameter has no entry of its own in `new_bindings`, so
/// for it any entry is a rebinding.
pub(crate) fn local_untrustworthy(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    variable: &str,
) -> bool {
    local_untrustworthy_at(ctx, use_row, facts, variable, false)
}

/// [`local_untrustworthy`], with `nested` set while checking a call
/// argument's receiver.
fn local_untrustworthy_at(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    variable: &str,
    nested: bool,
) -> bool {
    facts.unanalysable
        || facts
            .new_bindings
            .iter()
            .any(|binding| binding.variable == variable)
        || call_arg_rebinds_at(ctx, use_row, facts, variable, nested)
}

/// Where a trusted `new` receiver is read: the receiver's use span, or a
/// call argument's span when the receiver is the callee's object.
#[derive(Clone, Copy)]
pub(crate) struct ReadPoint {
    pub(crate) start_byte: u32,
    pub(crate) end_byte: u32,
    /// The nearest enclosing control-flow block at the read.
    pub(crate) block: Option<u32>,
}

/// Whether a `new`-receiver `variable` read at `at` is trustworthy: the
/// T21/T21a/T21b conditions plus the AF3 `global` rule (see `new_expr.rs`).
pub(crate) fn new_receiver_trusted(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    variable: &str,
    at: ReadPoint,
) -> bool {
    new_receiver_trusted_at(ctx, use_row, facts, variable, at, false)
}

/// [`new_receiver_trusted`], with `nested` set while checking a call
/// argument's receiver.
fn new_receiver_trusted_at(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    variable: &str,
    at: ReadPoint,
    nested: bool,
) -> bool {
    // (e) T21b: the scope contains a construct whose effect on locals cannot be
    // bounded, so no binding anywhere in it is trustworthy.
    if facts.unanalysable {
        return false;
    }
    // (f) T21b: a by-reference argument in this scope rebinds the variable. An
    // argument to an indexed declaration with a by-value parameter does not.
    if call_arg_rebinds_at(ctx, use_row, facts, variable, nested) {
        return false;
    }
    let Some(assignment) = sole_assignment(facts, variable) else {
        return false;
    };
    // (c) The one assignment must be a direct `new`, not a call, parameter, or
    // property read.
    if !assignment.direct_new {
        return false;
    }
    // (b) The assignment must precede the read in source order.
    if assignment.span.end_byte() > at.start_byte {
        return false;
    }
    // (d) Assignment and read must sit in the same control-flow block.
    if assignment.block != at.block {
        return false;
    }
    // (g) AF3: a call between the assignment and the read may rebind a global.
    !global_call_may_rebind(ctx, facts, variable, assignment, at)
}

/// The `global` rule (AF3; see the `new_expr.rs` module docs).
fn global_call_may_rebind(
    ctx: &RuleCtx<'_>,
    facts: &ScopeFacts,
    variable: &str,
    assignment: &NewBinding,
    at: ReadPoint,
) -> bool {
    if !facts.global_scope {
        return false;
    }
    let rebindable =
        ctx.global_names.contains(variable) || ctx.dynamic_global_write || ctx.php_files_unindexed;
    if !rebindable {
        return false;
    }
    // A call inside the assignment's own `new` expression runs before the
    // assignment completes. Facts without `value_end` measure from the
    // variable, which can only over-suppress.
    let after = assignment.value_end.unwrap_or(assignment.span.end_byte());
    facts.call_sites.iter().any(|call| {
        let contains_read = call.start_byte() <= at.start_byte && at.end_byte <= call.end_byte();
        call.start_byte() >= after
            && !contains_read
            && (facts.goto_present || call.start_byte() < at.start_byte)
    })
}

/// The variable's single simple assignment in the use's scope.
///
/// More than one assignment returns `None`: a reassigned variable is never a
/// trustworthy receiver (condition (a)).
fn sole_assignment<'a>(facts: &'a ScopeFacts, variable: &str) -> Option<&'a NewBinding> {
    let mut found: Option<&NewBinding> = None;
    for binding in &facts.new_bindings {
        if binding.variable != variable {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(binding);
    }
    found
}

/// Whether any recorded call argument rebinds `variable` (T21b).
///
/// The check is deliberately order-independent, like the reassignment test: a
/// variable passed by reference anywhere in the scope is not a trustworthy
/// receiver.
fn call_arg_rebinds_at(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    variable: &str,
    nested: bool,
) -> bool {
    facts
        .call_args
        .iter()
        .filter(|arg| arg.variable == variable)
        .any(|arg| call_arg_is_rebinding(ctx, use_row, facts, arg, nested))
}

/// Whether one call argument rebinds the variable it passes.
fn call_arg_is_rebinding(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    arg: &CallArg,
    nested: bool,
) -> bool {
    match arg.kind {
        CallArgKind::Function => match resolve_callee_function(ctx, facts, &arg.callee) {
            CalleeResolution::Indexed(row) => parameter_rebinds(ctx, row, arg.position),
            // No indexed declaration: a known by-reference builtin suppresses
            // only at its by-reference position; an unknown global function is
            // never assumed safe.
            CalleeResolution::Unindexed => match arg.position {
                Some(position) => builtin_rebinds(&arg.callee, position),
                None => true,
            },
            CalleeResolution::Unknown => true,
        },
        // A constructor argument is adjudicated against `Class::__construct`
        // exactly as a method argument is (AF3): an unresolved or ambiguous
        // class, or a class that declares no constructor itself, leaves the
        // callee unknown and suppresses.
        CallArgKind::Method | CallArgKind::StaticMethod | CallArgKind::Constructor => {
            let Some(receiver) = arg.receiver.as_ref() else {
                return true;
            };
            let class = match receiver {
                CallReceiver::Class { spelling } => resolve_class_spelling(ctx, spelling, facts),
                CallReceiver::SelfClass => enclosing_class(ctx, use_row),
                // A hinted receiver variable names its class only while the
                // variable itself is trustworthy (AF3); one level deep only.
                CallReceiver::Variable { variable, evidence } => {
                    if nested || !receiver_trusted(ctx, use_row, facts, variable, evidence, arg) {
                        return true;
                    }
                    let spelling = match evidence {
                        ReceiverEvidence::New { class_spelling, .. } => class_spelling,
                        ReceiverEvidence::TypedParameter { type_spelling } => type_spelling,
                    };
                    resolve_class_spelling(ctx, spelling, facts)
                }
                CallReceiver::Unknown => None,
            };
            // An unknown receiver or a member the class does not declare leaves
            // the callee unresolved, so the argument must suppress.
            let Some(class) = class else {
                return true;
            };
            // The callee of a method-call argument is a method, never a
            // same-name property or constant (AF2).
            match ctx.unique_member(&class.id, &arg.callee, MemberUse::Method) {
                Some(method) => parameter_rebinds(ctx, method, arg.position),
                None => true,
            }
        }
    }
}

/// Whether a call argument's hinted receiver `variable` passes the check its
/// own receiver use would (AF3), read at the argument's position.
fn receiver_trusted(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    variable: &str,
    evidence: &ReceiverEvidence,
    arg: &CallArg,
) -> bool {
    match evidence {
        ReceiverEvidence::New { use_block, .. } => {
            let at = ReadPoint {
                start_byte: arg.span.start_byte(),
                end_byte: arg.span.end_byte(),
                block: *use_block,
            };
            new_receiver_trusted_at(ctx, use_row, facts, variable, at, true)
        }
        ReceiverEvidence::TypedParameter { .. } => {
            !local_untrustworthy_at(ctx, use_row, facts, variable, true)
        }
    }
}

/// The outcome of resolving a function-call callee for argument adjudication.
enum CalleeResolution<'a> {
    /// Exactly one indexed function declaration.
    Indexed(&'a SymbolRow),
    /// No indexed function; the name may still name a global builtin.
    Unindexed,
    /// The name is owned by something unindexed or conflicting, so it is not a
    /// builtin candidate and cannot be proven by-value.
    Unknown,
}

/// Resolves a function callee the way `functions.rs` resolves a call, but
/// returns the declaration so its parameter list can be read.
fn resolve_callee_function<'a>(
    ctx: &RuleCtx<'a>,
    facts: &ScopeFacts,
    spelling: &str,
) -> CalleeResolution<'a> {
    if let Some(qname) = imports::fully_qualified(spelling) {
        return match ctx.unique_function(qname) {
            Some(row) => CalleeResolution::Indexed(row),
            None => CalleeResolution::Unindexed,
        };
    }
    // A qualified (namespace-relative) name is a project function, never a
    // builtin, and its unindexed form is unknown.
    if spelling.contains('\\') {
        return CalleeResolution::Unindexed;
    }
    // A visible `use function` alias owns the name; if its target is unindexed
    // the call is unresolved rather than the global builtin.
    match imports::function_via_imports(ctx, facts, spelling) {
        ImportOutcome::Bound(row) => return CalleeResolution::Indexed(row),
        ImportOutcome::Blocked => return CalleeResolution::Unknown,
        ImportOutcome::NoMatch => {}
    }
    // A function declared directly in the use's lexical scope chain shadows the
    // namespace/global fallback. More than one such declaration is ambiguous.
    let mut declared: Option<&SymbolRow> = None;
    for id in &facts.declares {
        let Some(row) = ctx.symbol_by_id(id) else {
            continue;
        };
        if row.kind != SymbolKind::Function || row.lookup_name != spelling.to_ascii_lowercase() {
            continue;
        }
        match declared {
            None => declared = Some(row),
            Some(existing) if existing.id == row.id => {}
            Some(_) => return CalleeResolution::Unknown,
        }
    }
    if let Some(row) = declared {
        return CalleeResolution::Indexed(row);
    }
    if let Some(namespace) = facts.namespace.as_deref().filter(|ns| !ns.is_empty()) {
        if let Some(row) = ctx.unique_function(&format!("{namespace}\\{spelling}")) {
            return CalleeResolution::Indexed(row);
        }
        // As in `functions.rs`: an unindexed PHP file may declare the
        // namespaced callee, so neither the global function nor a builtin can
        // stand in for it (AF2).
        if ctx.php_files_unindexed {
            return CalleeResolution::Unknown;
        }
    }
    match ctx.unique_function(spelling) {
        Some(row) => CalleeResolution::Indexed(row),
        None => CalleeResolution::Unindexed,
    }
}

/// Whether the parameter at `position` of an indexed declaration rebinds its
/// argument.
///
/// AF3 reads the flag recorded from the declaration's parse tree. A named
/// argument (no position), a declaration with no recorded parameter list, and
/// a position with no declared parameter are all unknown, and unknown is never
/// assumed by-value.
fn parameter_rebinds(ctx: &RuleCtx<'_>, row: &SymbolRow, position: Option<u32>) -> bool {
    let Some(position) = position else {
        return true;
    };
    ctx.parameter_by_ref(&row.id, position).unwrap_or(true)
}

/// Whether `callee` is a known by-reference builtin at `position`.
fn builtin_rebinds(callee: &str, position: u32) -> bool {
    let name = callee.trim_start_matches('\\');
    if name.contains('\\') {
        // A qualified, unindexed function is not a builtin.
        return true;
    }
    match php_builtins::by_ref_positions(name) {
        Some(positions) => positions.contains(&position),
        // An unknown global function is never assumed safe.
        None => true,
    }
}
