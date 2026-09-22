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
//! The class spelling resolves through the same alias/fully-qualified/
//! namespace-relative scope chain as a `type` use, and the named member must be
//! declared directly on the resolved class. The result is always
//! [`Resolution::Scoped`]: dynamic dispatch can select another implementation
//! (spec §11.3), so receiver evidence never upgrades to `exact`.

use rivet_core::Resolution;
use rivet_core::extract::{NewBinding, UseHint};
use rivet_store::UseRow;

use crate::resolve::rules::resolve_class_spelling;
use crate::resolve::{RuleCtx, ScopeFacts, SymbolId};

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
    ctx.unique_member(&class.id, &use_row.spelling)
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

#[cfg(test)]
mod tests {
    use crate::resolve::resolve_all;
    use rivet_core::extract::{ImportKind, NewBinding, ScopeImport, UseHint};
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
}
