//! Preceding `new` receiver hints (spec §11.3 `scoped`, §11.4 item 2; T21).
//!
//! A use whose hint is [`UseHint::NewExpr`] names a member of the class of a
//! local variable's direct `new` assignment. The binding holds only when that
//! assignment is safe to trust:
//!
//! - the variable has exactly one simple assignment in the use's scope
//!   (reassignment records nothing);
//! - that assignment is a direct `new` and precedes the use in source order;
//!   and
//! - the assignment and the use share the same control-flow block, so a
//!   conditional or loop body never leaks a binding to the outside (or vice
//!   versa).
//!
//! T21b adds two more suppression conditions:
//!
//! - the scope contains a construct whose effect on locals cannot be bounded
//!   (a dynamic variable write, a `$GLOBALS` write, `extract`, `eval`, or a
//!   dynamic-callee call), so no binding anywhere in it is recorded; and
//! - the variable is passed to a call that may rebind it. A recorded
//!   [`CallArg`] is adjudicated against the callee's indexed parameter list, or
//!   against [`php_builtins`] when no declaration exists; anything that cannot
//!   be proven by-value suppresses.
//!
//! The class spelling resolves through the same alias/fully-qualified/
//! namespace-relative scope chain as a `type` use, and the named member must be
//! declared directly on the resolved class. The result is always
//! [`Resolution::Scoped`]: dynamic dispatch can select another implementation
//! (spec §11.3), so receiver evidence never upgrades to `exact`.

use rivet_core::extract::{CallArg, CallArgKind, CallReceiver, NewBinding, UseHint};
use rivet_core::{Resolution, SymbolKind};
use rivet_store::{SymbolRow, UseRow};

use crate::resolve::rules::imports::{self, ImportOutcome};
use crate::resolve::rules::php_builtins;
use crate::resolve::rules::receivers::enclosing_class;
use crate::resolve::rules::resolve_class_spelling;
use crate::resolve::{MemberUse, RuleCtx, ScopeFacts, SymbolId};

/// Resolves a `new`-receiver member use conservatively.
pub(crate) fn resolve(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
) -> Option<(SymbolId, Resolution)> {
    let hint: UseHint = serde_json::from_str(&use_row.hint_json).ok()?;
    let UseHint::NewExpr {
        class_spelling,
        use_block,
    } = hint
    else {
        return None;
    };
    let variable = use_row.receiver.as_deref()?;
    // (e) T21b: the scope contains a construct whose effect on locals cannot be
    // bounded, so no binding anywhere in it is trustworthy.
    if facts.unanalysable {
        return None;
    }
    // (f) T21b: a by-reference argument in this scope rebinds the variable. An
    // argument to an indexed declaration with a by-value parameter does not.
    if call_arg_rebinds(ctx, use_row, facts, variable) {
        return None;
    }
    let assignment = sole_assignment(facts, variable)?;
    // (c) The one assignment must be a direct `new`, not a call, parameter, or
    // property read.
    if !assignment.direct_new {
        return None;
    }
    // (b) The assignment must precede the use in source order.
    if assignment.span.end_byte() > use_row.start_byte {
        return None;
    }
    // (d) Assignment and use must sit in the same control-flow block.
    if assignment.block != use_block {
        return None;
    }
    let class = resolve_class_spelling(ctx, &class_spelling, facts)?;
    // The member must be of the kind the use names (AF2).
    let member = MemberUse::of(use_row, facts)?;
    ctx.unique_member(&class.id, &use_row.spelling, member)
        .map(|member| (member.id.clone(), Resolution::Scoped))
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
fn call_arg_rebinds(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    variable: &str,
) -> bool {
    facts
        .call_args
        .iter()
        .filter(|arg| arg.variable == variable)
        .any(|arg| call_arg_is_rebinding(ctx, use_row, facts, arg))
}

/// Whether one call argument rebinds the variable it passes.
fn call_arg_is_rebinding(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
    arg: &CallArg,
) -> bool {
    match arg.kind {
        CallArgKind::Function => match resolve_callee_function(ctx, facts, &arg.callee) {
            CalleeResolution::Indexed(row) => parameter_rebinds(row, arg.position),
            // No indexed declaration: a known by-reference builtin suppresses
            // only at its by-reference position; an unknown global function is
            // never assumed safe.
            CalleeResolution::Unindexed => match arg.position {
                Some(position) => builtin_rebinds(&arg.callee, position),
                None => true,
            },
            CalleeResolution::Unknown => true,
        },
        CallArgKind::Method | CallArgKind::StaticMethod => {
            let Some(receiver) = arg.receiver.as_ref() else {
                return true;
            };
            let class = match receiver {
                CallReceiver::Class { spelling } => resolve_class_spelling(ctx, spelling, facts),
                CallReceiver::SelfClass => enclosing_class(ctx, use_row),
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
                Some(method) => parameter_rebinds(method, arg.position),
                None => true,
            }
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
fn parameter_rebinds(row: &SymbolRow, position: Option<u32>) -> bool {
    let Some(position) = position else {
        return true;
    };
    // A parameter list that cannot be read or a position with no declared
    // parameter is not assumed by-value.
    row.signature
        .as_deref()
        .and_then(|signature| parameter_is_by_ref(signature, position as usize))
        .unwrap_or(true)
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

/// Whether the parameter at 0-based `position` in a declaration `signature` is
/// declared by reference.
///
/// `None` means the parameter list cannot be read or the position has no
/// declared parameter, so the caller must suppress rather than assume by-value.
fn parameter_is_by_ref(signature: &str, position: usize) -> Option<bool> {
    // Block comments are removed first so a `)` or `,` inside one cannot close
    // or split the parameter list. Line comments are left in place: a collapsed
    // signature has no newlines, so their extent is unknowable and keeping them
    // can only produce a safe false positive.
    let cleaned = strip_block_comments(signature.as_bytes());
    let cleaned = String::from_utf8_lossy(&cleaned);
    let list = parameter_list(&cleaned)?;
    let parameters = split_top_level(list);
    parameters
        .get(position)
        .map(|parameter| parameter_has_reference(parameter))
}

/// The text between a declaration's parameter parentheses.
///
/// The `function` keyword anchors the search so a declaration attribute's
/// parentheses are not mistaken for the parameter list.
fn parameter_list(signature: &str) -> Option<&str> {
    let keyword = signature.find("function")?;
    let open = signature[keyword..].find('(')? + keyword;
    let bytes = signature.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut index = open;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(quote_byte) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == quote_byte {
                quote = None;
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&signature[open + 1..index]);
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}

/// Splits a parameter list on top-level commas, ignoring commas nested in
/// brackets or quoted default values.
fn split_top_level(list: &str) -> Vec<&str> {
    let bytes = list.as_bytes();
    let mut parameters = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(quote_byte) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == quote_byte {
                quote = None;
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => {
                    parameters.push(&list[start..index]);
                    start = index + 1;
                }
                _ => {}
            }
        }
        index += 1;
    }
    parameters.push(&list[start..]);
    if parameters.len() == 1 && parameters[0].trim().is_empty() {
        return Vec::new();
    }
    parameters
}

/// Whether one parameter text declares its variable by reference.
///
/// The reference modifier sits immediately before the variable (`&$x`,
/// `&...$x`), so it is the only `&` adjacent to the `$`. An intersection type
/// (`A&B $x`) has its `&` between type names and does not match. Whitespace
/// between the `&` and `$` is ignored; a string default containing `&$` is a
/// safe false positive (it suppresses rather than binds).
fn parameter_has_reference(parameter: &str) -> bool {
    let compact: Vec<u8> = parameter
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    compact.windows(2).any(|window| window == b"&$")
        || compact.windows(4).any(|window| window == b"&...")
}

/// Removes block comments from a signature, preserving quoted strings.
///
/// A `)` or `,` inside a block comment would otherwise close or split the
/// parameter list. Line comments (`//`, `#`) are deliberately left in place: a
/// collapsed signature has no newlines, so their extent is unknowable, and
/// keeping them can only produce a safe false positive (a suppression).
fn strip_block_comments(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut index = 0;
    let mut quote: Option<u8> = None;
    while index < value.len() {
        let byte = value[index];
        if let Some(quote_byte) = quote {
            out.push(byte);
            if byte == b'\\' {
                if let Some(escaped) = value.get(index + 1) {
                    out.push(*escaped);
                    index += 2;
                    continue;
                }
            } else if byte == quote_byte {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' => {
                quote = Some(byte);
                out.push(byte);
                index += 1;
            }
            b'/' if value.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < value.len() && !(value[index] == b'*' && value[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(value.len());
            }
            _ => {
                out.push(byte);
                index += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::resolve::resolve_all;
    use rivet_core::extract::{
        CallArg, CallArgKind, CallReceiver, ImportKind, NewBinding, ScopeImport, UseHint,
    };
    use rivet_core::{ParseStatus, RefKind, Resolution, Span, SymbolKind};
    use rivet_store::{
        BindingRow, FileRow, Fingerprint, INDEX_FORMAT_VERSION, InventoryInput, ScopeRow, Store,
        SymbolRow, UseRow,
    };

    /// A `files` row for `path` with no content; resolution never reads bytes.
    fn file(path: &str) -> FileRow {
        FileRow {
            path: path.to_string(),
            language: Some("php".to_string()),
            mtime_ns: 0,
            size: 0,
            content_hash: None,
            source: None,
            parse_status: ParseStatus::Ok,
        }
    }

    /// A declaration row; `parent_id` is the canonical ID of its container.
    fn symbol(file: &str, qname: &str, kind: SymbolKind, parent_id: Option<&str>) -> SymbolRow {
        let name = qname
            .rsplit(['\\', ':'])
            .next()
            .unwrap_or(qname)
            .to_string();
        let lookup_name = match kind {
            SymbolKind::Property | SymbolKind::Const => name.clone(),
            _ => name.to_lowercase(),
        };
        SymbolRow {
            id: format!("{file}#{qname}"),
            file: file.to_string(),
            name,
            lookup_name,
            qualified_name: qname.to_string(),
            kind,
            parent_id: parent_id.map(str::to_string),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            end_line: 1,
            signature: None,
            doc_comment: None,
        }
    }

    /// A use row carrying an explicit `new` receiver hint.
    fn new_use(
        file: &str,
        spelling: &str,
        receiver: &str,
        scope_key: &str,
        start_byte: u32,
        class_spelling: &str,
        use_block: Option<u32>,
    ) -> UseRow {
        let hint = UseHint::NewExpr {
            class_spelling: class_spelling.to_string(),
            use_block,
        };
        UseRow {
            use_id: None,
            file: file.to_string(),
            containing_symbol: None,
            scope_key: scope_key.to_string(),
            spelling: spelling.to_string(),
            lookup_name: spelling.to_lowercase(),
            ref_kind: RefKind::Call,
            start_byte,
            end_byte: start_byte + spelling.len() as u32,
            line: 1,
            col: 1,
            receiver: Some(receiver.to_string()),
            hint_json: serde_json::to_string(&hint).expect("hint serializes"),
        }
    }

    /// One simple assignment fact for the tests.
    fn assignment(
        variable: &str,
        class_spelling: &str,
        direct_new: bool,
        block: Option<u32>,
        span: (u32, u32),
    ) -> NewBinding {
        NewBinding {
            variable: variable.to_string(),
            class_spelling: class_spelling.to_string(),
            span: Span::new(span.0, span.1).expect("valid assignment span"),
            direct_new,
            block,
        }
    }

    /// A scope row with generated facts JSON.
    fn scope(
        file: &str,
        scope_key: &str,
        parent: Option<&str>,
        imports: &[ScopeImport],
        declares: &[&str],
        new_bindings: &[NewBinding],
    ) -> ScopeRow {
        let facts_json = serde_json::json!({
            "imports": imports,
            "typed_bindings": [],
            "new_bindings": new_bindings,
            "declares": declares,
        })
        .to_string();
        ScopeRow {
            file: file.to_string(),
            scope_key: scope_key.to_string(),
            parent_scope_key: parent.map(str::to_string),
            facts_json,
        }
    }

    /// A declaration row carrying a signature for parameter parsing.
    fn with_signature(
        file: &str,
        qname: &str,
        kind: SymbolKind,
        parent_id: Option<&str>,
        signature: &str,
    ) -> SymbolRow {
        let mut row = symbol(file, qname, kind, parent_id);
        row.signature = Some(signature.to_string());
        row
    }

    /// A scope row carrying T21b call-argument and unanalysable facts.
    fn scope_t21b(
        file: &str,
        scope_key: &str,
        parent: Option<&str>,
        new_bindings: &[NewBinding],
        call_args: &[CallArg],
        unanalysable: bool,
        declares: &[&str],
    ) -> ScopeRow {
        let facts_json = serde_json::json!({
            "imports": [],
            "typed_bindings": [],
            "new_bindings": new_bindings,
            "call_args": call_args,
            "unanalysable": unanalysable,
            "declares": declares,
        })
        .to_string();
        ScopeRow {
            file: file.to_string(),
            scope_key: scope_key.to_string(),
            parent_scope_key: parent.map(str::to_string),
            facts_json,
        }
    }

    /// One call-argument fact for the tests.
    fn call_arg(
        variable: &str,
        callee: &str,
        kind: CallArgKind,
        position: Option<u32>,
        receiver: Option<CallReceiver>,
    ) -> CallArg {
        CallArg {
            variable: variable.to_string(),
            callee: callee.to_string(),
            kind,
            position,
            receiver,
            span: Span::new(50, 52).expect("valid argument span"),
        }
    }

    /// Publishes the rows into an in-memory store and returns it.
    fn seed(symbols: Vec<SymbolRow>, uses: Vec<UseRow>, scopes: Vec<ScopeRow>) -> Store {
        let mut paths: Vec<String> = symbols
            .iter()
            .map(|row| row.file.clone())
            .chain(uses.iter().map(|row| row.file.clone()))
            .chain(scopes.iter().map(|row| row.file.clone()))
            .collect();
        paths.sort();
        paths.dedup();
        let mut store = Store::open_in_memory().expect("open in-memory store");
        store
            .publish_inventory(InventoryInput {
                fingerprint: Fingerprint {
                    index_format_version: INDEX_FORMAT_VERSION.to_string(),
                    effective_config: "test".to_string(),
                    extractor: "test".to_string(),
                    resolver: "php-rules-v1".to_string(),
                },
                files: paths.iter().map(|path| file(path)).collect(),
                symbols,
                uses,
                scopes,
                bindings: Vec::new(),
                force: false,
            })
            .expect("publish new_expr fixture");
        store
    }

    /// The sole binding, or panics on a different count.
    fn only_binding(store: &Store) -> BindingRow {
        let bindings = resolve_all(store).expect("resolve");
        assert_eq!(
            bindings.len(),
            1,
            "expected exactly one binding: {bindings:?}"
        );
        bindings.into_iter().next().expect("one binding")
    }

    /// The `A\Svc::go` method and its class.
    fn svc_symbols() -> Vec<SymbolRow> {
        vec![
            symbol("Svc.php", "A\\Svc", SymbolKind::Class, None),
            symbol(
                "Svc.php",
                "A\\Svc::go",
                SymbolKind::Method,
                Some("Svc.php#A\\Svc"),
            ),
        ]
    }

    #[test]
    fn direct_new_before_the_use_binds_scoped() {
        let store = seed(
            svc_symbols(),
            vec![new_use("C.php", "go", "$s", "1:0", 100, "\\A\\Svc", None)],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[assignment("$s", "\\A\\Svc", true, None, (10, 12))],
            )],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "Svc.php#A\\Svc::go");
        assert_eq!(binding.resolution, Resolution::Scoped);
    }

    #[test]
    fn reassignment_records_nothing() {
        // (a) The same variable is assigned twice, even to the same class.
        let store = seed(
            svc_symbols(),
            vec![new_use("C.php", "go", "$s", "1:0", 100, "\\A\\Svc", None)],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[
                    assignment("$s", "\\A\\Svc", true, None, (10, 12)),
                    assignment("$s", "\\A\\Svc", true, None, (60, 62)),
                ],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn new_plus_rebinding_records_nothing() {
        // (a) T21a: the extractor records a non-`new` rebinding (`foreach`,
        // destructuring, `catch`, ...) as a second fact for the same variable.
        // It must suppress the direct `new` just like a second `new` does.
        let store = seed(
            svc_symbols(),
            vec![new_use("C.php", "go", "$s", "1:0", 100, "\\A\\Svc", None)],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[
                    assignment("$s", "\\A\\Svc", true, None, (10, 12)),
                    assignment("$s", "", false, None, (40, 42)),
                ],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn assignment_after_the_use_records_nothing() {
        // (b) The only assignment starts after the use.
        let store = seed(
            svc_symbols(),
            vec![new_use("C.php", "go", "$s", "1:0", 100, "\\A\\Svc", None)],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[assignment("$s", "\\A\\Svc", true, None, (120, 122))],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn non_new_assignment_records_nothing() {
        // (c) The only assignment is a call, not a direct `new`.
        let store = seed(
            svc_symbols(),
            vec![new_use("C.php", "go", "$s", "1:0", 100, "\\A\\Svc", None)],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[assignment("$s", "", false, None, (10, 12))],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn conditional_assignment_outside_the_use_records_nothing() {
        // (d) The assignment is inside a conditional block; the use is not.
        let store = seed(
            svc_symbols(),
            vec![new_use("C.php", "go", "$s", "1:0", 100, "\\A\\Svc", None)],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[assignment("$s", "\\A\\Svc", true, Some(40), (50, 52))],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn use_inside_a_conditional_records_nothing() {
        // (d) The reverse: the use is inside a block; the assignment is not.
        let store = seed(
            svc_symbols(),
            vec![new_use(
                "C.php",
                "go",
                "$s",
                "1:0",
                100,
                "\\A\\Svc",
                Some(80),
            )],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[assignment("$s", "\\A\\Svc", true, None, (10, 12))],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn same_conditional_block_binds_scoped() {
        // (d) Both assignment and use share the same conditional block.
        let store = seed(
            svc_symbols(),
            vec![new_use(
                "C.php",
                "go",
                "$s",
                "1:0",
                100,
                "\\A\\Svc",
                Some(40),
            )],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[assignment("$s", "\\A\\Svc", true, Some(40), (50, 52))],
            )],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "Svc.php#A\\Svc::go");
        assert_eq!(binding.resolution, Resolution::Scoped);
    }

    #[test]
    fn alias_import_resolves_the_new_class() {
        // The happy path can also resolve through an import alias.
        let import = ScopeImport {
            alias: "Svc".to_string(),
            target_qualified: "A\\Svc".to_string(),
            kind: ImportKind::Class,
            span: Span::new(0, 3).expect("valid import span"),
        };
        let store = seed(
            svc_symbols(),
            vec![new_use("C.php", "go", "$s", "1:0", 100, "Svc", None)],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[import],
                &[],
                &[assignment("$s", "Svc", true, None, (10, 12))],
            )],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "Svc.php#A\\Svc::go");
        assert_eq!(binding.resolution, Resolution::Scoped);
    }

    #[test]
    fn ambiguous_class_records_nothing() {
        // The spelling matches two indexed classes, so no unique class exists.
        let symbols = vec![
            symbol("One.php", "A\\Svc", SymbolKind::Class, None),
            symbol("Two.php", "a\\svc", SymbolKind::Class, None),
        ];
        let store = seed(
            symbols,
            vec![new_use("C.php", "go", "$s", "1:0", 100, "\\A\\Svc", None)],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[assignment("$s", "\\A\\Svc", true, None, (10, 12))],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn missing_member_records_nothing() {
        // The class resolves but does not declare the named member.
        let store = seed(
            svc_symbols(),
            vec![new_use(
                "C.php", "missing", "$s", "1:0", 100, "\\A\\Svc", None,
            )],
            vec![scope(
                "C.php",
                "1:0",
                Some("top:file"),
                &[],
                &[],
                &[assignment("$s", "\\A\\Svc", true, None, (10, 12))],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    /// A direct `new` assignment for `$s` at the use's scope, with optional
    /// T21b facts, published against the `A\Svc` fixture.
    fn t21b_store(
        symbols: Vec<SymbolRow>,
        call_args: &[CallArg],
        unanalysable: bool,
        declares: &[&str],
    ) -> Store {
        seed(
            symbols,
            vec![new_use(
                "C.php", "go", "$s", "top:file", 100, "\\A\\Svc", None,
            )],
            vec![scope_t21b(
                "C.php",
                "top:file",
                None,
                &[assignment("$s", "\\A\\Svc", true, None, (10, 12))],
                call_args,
                unanalysable,
                declares,
            )],
        )
    }

    #[test]
    fn indexed_by_reference_function_parameter_records_nothing() {
        let takes_ref = with_signature(
            "F.php",
            "takesRef",
            SymbolKind::Function,
            None,
            "function takesRef(&$x): void",
        );
        let store = t21b_store(
            {
                let mut symbols = svc_symbols();
                symbols.push(takes_ref.clone());
                symbols
            },
            &[call_arg(
                "$s",
                "takesRef",
                CallArgKind::Function,
                Some(0),
                None,
            )],
            false,
            &[&takes_ref.id],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn indexed_by_value_function_parameter_keeps_the_binding() {
        let takes_value = with_signature(
            "F.php",
            "takesValue",
            SymbolKind::Function,
            None,
            "function takesValue($x): void",
        );
        let store = t21b_store(
            {
                let mut symbols = svc_symbols();
                symbols.push(takes_value.clone());
                symbols
            },
            &[call_arg(
                "$s",
                "takesValue",
                CallArgKind::Function,
                Some(0),
                None,
            )],
            false,
            &[&takes_value.id],
        );
        assert_eq!(only_binding(&store).target_id, "Svc.php#A\\Svc::go");
    }

    #[test]
    fn by_reference_builtin_records_nothing() {
        let store = t21b_store(
            svc_symbols(),
            &[call_arg(
                "$s",
                "preg_match",
                CallArgKind::Function,
                Some(2),
                None,
            )],
            false,
            &[],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn unknown_global_function_records_nothing() {
        let store = t21b_store(
            svc_symbols(),
            &[call_arg(
                "$s",
                "strlen",
                CallArgKind::Function,
                Some(0),
                None,
            )],
            false,
            &[],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn unanalysable_scope_records_nothing() {
        let store = t21b_store(svc_symbols(), &[], true, &[]);
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn indexed_by_reference_method_parameter_records_nothing() {
        let class = symbol("Svc.php", "A\\Svc", SymbolKind::Class, None);
        let go = with_signature(
            "Svc.php",
            "A\\Svc::go",
            SymbolKind::Method,
            Some("Svc.php#A\\Svc"),
            "public function go(): void",
        );
        let takes_ref = with_signature(
            "Svc.php",
            "A\\Svc::takesRef",
            SymbolKind::Method,
            Some("Svc.php#A\\Svc"),
            "public function takesRef(&$x): void",
        );
        let store = t21b_store(
            vec![class, go, takes_ref],
            &[call_arg(
                "$s",
                "takesRef",
                CallArgKind::Method,
                Some(0),
                Some(CallReceiver::Class {
                    spelling: "\\A\\Svc".to_string(),
                }),
            )],
            false,
            &[],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn method_on_an_unknown_receiver_records_nothing() {
        let store = t21b_store(
            svc_symbols(),
            &[call_arg(
                "$s",
                "takesValue",
                CallArgKind::Method,
                Some(0),
                Some(CallReceiver::Unknown),
            )],
            false,
            &[],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn a_named_argument_position_records_nothing() {
        let store = t21b_store(
            svc_symbols(),
            &[call_arg(
                "$s",
                "takesValue",
                CallArgKind::Function,
                None,
                None,
            )],
            false,
            &[],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn a_call_argument_for_another_variable_keeps_the_binding() {
        let takes_ref = with_signature(
            "F.php",
            "takesRef",
            SymbolKind::Function,
            None,
            "function takesRef(&$x): void",
        );
        let store = t21b_store(
            {
                let mut symbols = svc_symbols();
                symbols.push(takes_ref.clone());
                symbols
            },
            &[call_arg(
                "$other",
                "takesRef",
                CallArgKind::Function,
                Some(0),
                None,
            )],
            false,
            &[&takes_ref.id],
        );
        assert_eq!(only_binding(&store).target_id, "Svc.php#A\\Svc::go");
    }

    #[test]
    fn signature_parameter_reference_parsing() {
        use super::parameter_is_by_ref;
        assert_eq!(parameter_is_by_ref("function f(&$x): void", 0), Some(true));
        assert_eq!(parameter_is_by_ref("function f($x): void", 0), Some(false));
        assert_eq!(
            parameter_is_by_ref("function f(int $a, &$b): void", 1),
            Some(true)
        );
        assert_eq!(
            parameter_is_by_ref("public function f(A&B $x): void", 0),
            Some(false)
        );
        assert_eq!(
            parameter_is_by_ref("function f(&...$xs): void", 0),
            Some(true)
        );
        assert_eq!(
            parameter_is_by_ref("function f($x = [1, 2]): void", 0),
            Some(false)
        );
        assert_eq!(parameter_is_by_ref("function f($x): void", 3), None);
        assert_eq!(parameter_is_by_ref("function f()", 0), None);
        assert_eq!(
            parameter_is_by_ref("public function f(#[Attr(1)] &$x): void", 0),
            Some(true)
        );
        assert_eq!(
            parameter_is_by_ref("function f(& /* c */ $x): void", 0),
            Some(true)
        );
        assert_eq!(
            parameter_is_by_ref("function f(/* ) , */ &$x): void", 0),
            Some(true),
            "a comment cannot close or split the parameter list"
        );
        assert_eq!(
            parameter_is_by_ref("function f($x = 'a&$b'): void", 0),
            Some(true),
            "a string default containing &$ suppresses rather than binds"
        );
        assert_eq!(
            parameter_is_by_ref("function f(# comment\n &$x): void", 0),
            Some(true)
        );
    }
}
