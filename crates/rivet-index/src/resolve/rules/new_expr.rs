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
//!   `CallArg` is adjudicated against the callee's indexed parameter list, or
//!   against [`php_builtins`](super::php_builtins) when no declaration exists; anything that cannot
//!   be proven by-value suppresses.
//!
//! AF3 closes three more rebinding forms and moves the T21b machinery to
//! [`rebinding`] so the typed-parameter rule shares it:
//!
//! - a reference taken to the variable (`$y = &$x`, a by-reference `foreach`
//!   over it) is recorded by the extractor as a rebinding;
//! - a constructor argument (`new Holder($x)`) is a call argument adjudicated
//!   against `Holder::__construct`; and
//! - the `global` rule below.
//!
//! The `global` rule. At a file's or namespace block's top level the variable
//! is a PHP global, which any function declaring it `global` (or writing it
//! through `$GLOBALS`) rebinds when called. The binding is suppressed when
//! both hold:
//!
//! 1. some indexed function-like scope anywhere in the snapshot declares the
//!    variable's name `global` or uses that literal `$GLOBALS` key; or some
//!    such scope rebinds globals it does not name (`global $$n`, a dynamic
//!    `$GLOBALS` key, `$GLOBALS` passed whole, `eval` or an include inside a
//!    function); or some PHP file could not be indexed, since it could hold
//!    such a function (as the AF2 fallback rule assumes); and
//! 2. an explicit call (function, method, static, `new`, `clone`) starts after
//!    the assignment's `new` expression and before the use, and does not
//!    contain the use (a call containing the use runs after the receiver is
//!    read). When the scope contains a `goto`, any such call after the
//!    assignment counts, since a backward jump can run it before the use.
//!
//! Implicit invocations of user code (magic methods, destructors, autoload)
//! are not counted as calls; magic methods are outside v0.1 (spec
//! ADDING-A-LANGUAGE "Not resolved in v0.1").
//!
//! The class spelling resolves through the same alias/fully-qualified/
//! namespace-relative scope chain as a `type` use, and the named member must be
//! declared directly on the resolved class. The result is always
//! [`Resolution::Scoped`]: dynamic dispatch can select another implementation
//! (spec §11.3), so receiver evidence never upgrades to `exact`.

use rivet_core::Resolution;
use rivet_core::extract::UseHint;
use rivet_store::UseRow;

use crate::resolve::rules::rebinding::{ReadPoint, new_receiver_trusted};
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
    // Conditions (a)-(g), shared with call-argument adjudication (AF3).
    let at = ReadPoint {
        start_byte: use_row.start_byte,
        end_byte: use_row.end_byte,
        block: use_block,
    };
    if !new_receiver_trusted(ctx, use_row, facts, variable, at) {
        return None;
    }
    let class = resolve_class_spelling(ctx, &class_spelling, facts)?;
    // The member must be of the kind the use names (AF2).
    let member = MemberUse::of(use_row, facts)?;
    ctx.unique_member(&class.id, &use_row.spelling, member)
        .map(|member| (member.id.clone(), Resolution::Scoped))
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
            value_end: None,
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

    /// A declaration row carrying a signature. Since AF3 the resolver reads
    /// by-reference flags from `parameter_lists`, not from this text; see
    /// [`recorded_flags`].
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
    #[allow(clippy::too_many_arguments)]
    fn scope_t21b(
        file: &str,
        scope_key: &str,
        parent: Option<&str>,
        new_bindings: &[NewBinding],
        call_args: &[CallArg],
        unanalysable: bool,
        declares: &[&str],
        parameter_lists: &[(String, Vec<bool>)],
    ) -> ScopeRow {
        let parameter_lists: Vec<serde_json::Value> = parameter_lists
            .iter()
            .map(|(symbol, by_ref)| serde_json::json!({"symbol": symbol, "by_ref": by_ref}))
            .collect();
        let facts_json = serde_json::json!({
            "imports": [],
            "typed_bindings": [],
            "new_bindings": new_bindings,
            "call_args": call_args,
            "unanalysable": unanalysable,
            "declares": declares,
            "parameter_lists": parameter_lists,
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
        let parameter_lists: Vec<(String, Vec<bool>)> = symbols
            .iter()
            .filter_map(|row| {
                let signature = row.signature.as_deref()?;
                Some((row.id.clone(), recorded_flags(signature)))
            })
            .collect();
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
                &parameter_lists,
            )],
        )
    }

    /// The by-reference flags the extractor records for each test signature
    /// (AF3). A lookup table, not a parser: an unlisted signature panics.
    fn recorded_flags(signature: &str) -> Vec<bool> {
        match signature {
            "function takesRef(&$x): void" | "public function takesRef(&$x): void" => vec![true],
            "function takesValue($x): void" => vec![false],
            "public function go(): void" => Vec::new(),
            other => panic!("no recorded flags for test signature {other:?}"),
        }
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

    /// A scope row whose facts are the given JSON object.
    fn scope_json(
        file: &str,
        scope_key: &str,
        parent: Option<&str>,
        facts: serde_json::Value,
    ) -> ScopeRow {
        ScopeRow {
            file: file.to_string(),
            scope_key: scope_key.to_string(),
            parent_scope_key: parent.map(str::to_string),
            facts_json: facts.to_string(),
        }
    }

    /// The file-scope `$s = new \A\Svc()` (variable at 10..12, `new`
    /// expression ending at 30) with `$s->go()` at 100, plus a function scope
    /// in `R.php` with the given facts (AF3 `global` rule).
    fn global_case(
        call_sites: &[(u32, u32)],
        goto: bool,
        function_facts: serde_json::Value,
    ) -> Store {
        let mut binding = assignment("$s", "\\A\\Svc", true, None, (10, 12));
        binding.value_end = Some(30);
        let sites: Vec<serde_json::Value> = call_sites
            .iter()
            .map(|(start, end)| serde_json::json!({"start_byte": start, "end_byte": end}))
            .collect();
        seed(
            svc_symbols(),
            vec![new_use(
                "C.php", "go", "$s", "top:file", 100, "\\A\\Svc", None,
            )],
            vec![
                scope_json(
                    "C.php",
                    "top:file",
                    None,
                    serde_json::json!({
                        "new_bindings": [binding],
                        "global_scope": true,
                        "call_sites": sites,
                        "goto_present": goto,
                    }),
                ),
                scope_json("R.php", "0:0", Some("top:file"), function_facts),
            ],
        )
    }

    #[test]
    fn a_call_between_suppresses_when_a_function_declares_the_name_global() {
        let store = global_case(
            &[(50, 60)],
            false,
            serde_json::json!({"global_names": ["$s"]}),
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn a_global_for_another_name_keeps_the_binding() {
        let store = global_case(
            &[(50, 60)],
            false,
            serde_json::json!({"global_names": ["$t"]}),
        );
        assert_eq!(only_binding(&store).target_id, "Svc.php#A\\Svc::go");
    }

    #[test]
    fn a_dynamic_global_write_suppresses_across_a_call() {
        let store = global_case(
            &[(50, 60)],
            false,
            serde_json::json!({"dynamic_global_write": true}),
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn calls_that_cannot_intervene_keep_the_binding() {
        // Inside the `new` expression (15..29), containing the use (95..110),
        // and after the use (120..130): none runs between the assignment and
        // the use.
        let store = global_case(
            &[(15, 29), (95, 110), (120, 130)],
            false,
            serde_json::json!({"global_names": ["$s"]}),
        );
        assert_eq!(only_binding(&store).target_id, "Svc.php#A\\Svc::go");
    }

    #[test]
    fn a_goto_makes_a_later_call_intervene() {
        let store = global_case(
            &[(120, 130)],
            true,
            serde_json::json!({"global_names": ["$s"]}),
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn a_function_scope_is_not_subject_to_the_global_rule() {
        // The same facts in a scope that is not global: its `$s` is a local.
        let mut binding = assignment("$s", "\\A\\Svc", true, None, (10, 12));
        binding.value_end = Some(30);
        let store = seed(
            svc_symbols(),
            vec![new_use("C.php", "go", "$s", "0:0", 100, "\\A\\Svc", None)],
            vec![
                scope_json(
                    "C.php",
                    "0:0",
                    None,
                    serde_json::json!({"new_bindings": [binding]}),
                ),
                scope_json(
                    "R.php",
                    "0:0",
                    None,
                    serde_json::json!({"global_names": ["$s"], "dynamic_global_write": true}),
                ),
            ],
        );
        assert_eq!(only_binding(&store).target_id, "Svc.php#A\\Svc::go");
    }

    /// `A\Holder` with a constructor whose one parameter has the given flag.
    fn holder_store(constructor: Option<bool>) -> Store {
        let mut symbols = svc_symbols();
        symbols.push(symbol("Holder.php", "A\\Holder", SymbolKind::Class, None));
        let mut lists = Vec::new();
        if let Some(by_ref) = constructor {
            let ctor = symbol(
                "Holder.php",
                "A\\Holder::__construct",
                SymbolKind::Method,
                Some("Holder.php#A\\Holder"),
            );
            lists.push(serde_json::json!({"symbol": ctor.id, "by_ref": [by_ref]}));
            symbols.push(ctor);
        }
        let arg = call_arg(
            "$s",
            "__construct",
            CallArgKind::Constructor,
            Some(0),
            Some(CallReceiver::Class {
                spelling: "\\A\\Holder".to_string(),
            }),
        );
        seed(
            symbols,
            vec![new_use(
                "C.php", "go", "$s", "top:file", 100, "\\A\\Svc", None,
            )],
            vec![scope_json(
                "C.php",
                "top:file",
                None,
                serde_json::json!({
                    "new_bindings": [assignment("$s", "\\A\\Svc", true, None, (10, 12))],
                    "call_args": [arg],
                    "parameter_lists": lists,
                }),
            )],
        )
    }

    #[test]
    fn a_by_reference_constructor_parameter_records_nothing() {
        assert!(
            resolve_all(&holder_store(Some(true)))
                .expect("resolve")
                .is_empty()
        );
    }

    #[test]
    fn a_by_value_constructor_parameter_keeps_the_binding() {
        assert_eq!(
            only_binding(&holder_store(Some(false))).target_id,
            "Svc.php#A\\Svc::go"
        );
    }

    #[test]
    fn a_class_without_its_own_constructor_records_nothing() {
        assert!(
            resolve_all(&holder_store(None))
                .expect("resolve")
                .is_empty()
        );
    }

    #[test]
    fn a_declaration_without_recorded_parameters_is_unknown() {
        // Facts written before AF3 have no `parameter_lists`; the signature
        // text is no longer parsed, so the parameter is not assumed by-value.
        let takes_value = with_signature(
            "F.php",
            "takesValue",
            SymbolKind::Function,
            None,
            "function takesValue($x): void",
        );
        let mut symbols = svc_symbols();
        symbols.push(takes_value.clone());
        let store = seed(
            symbols,
            vec![new_use(
                "C.php", "go", "$s", "top:file", 100, "\\A\\Svc", None,
            )],
            vec![scope_t21b(
                "C.php",
                "top:file",
                None,
                &[assignment("$s", "\\A\\Svc", true, None, (10, 12))],
                &[call_arg(
                    "$s",
                    "takesValue",
                    CallArgKind::Function,
                    Some(0),
                    None,
                )],
                false,
                &[&takes_value.id],
                &[],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }
}
