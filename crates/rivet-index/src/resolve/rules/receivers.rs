//! `$this`/`self`/`static` member uses and explicit receiver types (spec §11.3
//! `scoped`; T20).
//!
//! Rule 1: a use whose hint is [`UseHint::This`] or [`UseHint::SelfOrStatic`]
//! names a member of the class that encloses its containing symbol. The member
//! binds only when that class declares it; inheritance is not traversed in
//! v0.1, so a missing member records nothing. `parent::` is deliberately not
//! bound because it names an ancestor class.
//!
//! Rule 2: a [`UseHint::Typed`] receiver resolves its type spelling through the
//! same alias/fully-qualified/namespace-relative scope chain as a `type` use,
//! then binds the named member of that class.
//!
//! Both rules yield [`Resolution::Scoped`] only: late static binding and
//! runtime dispatch can select another implementation, so receiver evidence
//! never upgrades to `exact` (spec §11.3).

use rivet_core::extract::UseHint;
use rivet_core::{Resolution, SymbolKind};
use rivet_store::{SymbolRow, UseRow};

use crate::resolve::rules::resolve_class_spelling;
use crate::resolve::{RuleCtx, ScopeFacts, SymbolId};

/// Resolves a `$this`/`self`/`static` member or an explicitly typed receiver.
pub(crate) fn resolve(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
) -> Option<(SymbolId, Resolution)> {
    let hint: UseHint = serde_json::from_str(&use_row.hint_json).ok()?;
    let class = match hint {
        UseHint::This | UseHint::SelfOrStatic => {
            if !names_enclosing_class(use_row, &hint) {
                return None;
            }
            enclosing_class(ctx, use_row)?
        }
        UseHint::Typed { type_spelling } => resolve_class_spelling(ctx, &type_spelling, facts)?,
        _ => return None,
    };
    ctx.unique_member(&class.id, &use_row.spelling)
        .map(|member| (member.id.clone(), Resolution::Scoped))
}

/// Whether a `This`/`SelfOrStatic` hint names the enclosing class itself.
///
/// `$this` always does; `self` and `static` do too, but `parent` names the
/// ancestor class, which v0.1 does not traverse.
fn names_enclosing_class(use_row: &UseRow, hint: &UseHint) -> bool {
    match hint {
        UseHint::This => use_row.receiver.as_deref() == Some("$this"),
        UseHint::SelfOrStatic => use_row.receiver.as_deref().is_some_and(|receiver| {
            receiver.eq_ignore_ascii_case("self") || receiver.eq_ignore_ascii_case("static")
        }),
        _ => false,
    }
}

/// The nearest class-like symbol enclosing the use's container, if any.
pub(crate) fn enclosing_class<'a>(ctx: &RuleCtx<'a>, use_row: &UseRow) -> Option<&'a SymbolRow> {
    let mut current = use_row.containing_symbol.as_deref();
    while let Some(id) = current {
        let row = ctx.symbol_by_id(id)?;
        if is_class_like(row.kind) {
            return Some(row);
        }
        current = row.parent_id.as_deref();
    }
    None
}

/// Whether `kind` names a class-like declaration (a trait is class-kind).
fn is_class_like(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
    )
}

#[cfg(test)]
mod tests {
    use crate::resolve::resolve_all;
    use rivet_core::{ParseStatus, RefKind, Resolution, SymbolKind};
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

    /// A use row carrying an explicit receiver hint JSON.
    #[allow(clippy::too_many_arguments)]
    fn use_row(
        file: &str,
        spelling: &str,
        ref_kind: RefKind,
        receiver: Option<&str>,
        containing_symbol: Option<&str>,
        scope_key: &str,
        hint_json: &str,
    ) -> UseRow {
        UseRow {
            use_id: None,
            file: file.to_string(),
            containing_symbol: containing_symbol.map(str::to_string),
            scope_key: scope_key.to_string(),
            spelling: spelling.to_string(),
            lookup_name: spelling.to_lowercase(),
            ref_kind,
            start_byte: 0,
            end_byte: spelling.len() as u32,
            line: 1,
            col: 1,
            receiver: receiver.map(str::to_string),
            hint_json: hint_json.to_string(),
        }
    }

    /// A scope row with `parent` and generated facts JSON.
    fn scope(
        file: &str,
        scope_key: &str,
        parent: Option<&str>,
        imports: &[(&str, &str, &str)],
        declares: &[&str],
    ) -> ScopeRow {
        let imports: Vec<String> = imports
            .iter()
            .map(|(alias, target, kind)| {
                format!(
                    "{{\"alias\":{alias:?},\"target_qualified\":{target:?},\"kind\":{kind:?},\
                     \"span\":{{\"start_byte\":0,\"end_byte\":0}}}}"
                )
            })
            .collect();
        let declares: Vec<String> = declares.iter().map(|id| format!("{id:?}")).collect();
        let facts_json = format!(
            "{{\"imports\":[{}],\"typed_bindings\":[],\"new_bindings\":[],\"declares\":[{}]}}",
            imports.join(","),
            declares.join(",")
        );
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
            .expect("publish receiver fixture");
        store
    }

    /// Returns the sole binding, or panics on a different count.
    fn only_binding(store: &Store) -> BindingRow {
        let bindings = resolve_all(store).expect("resolve");
        assert_eq!(
            bindings.len(),
            1,
            "expected exactly one binding: {bindings:?}"
        );
        bindings.into_iter().next().expect("one binding")
    }

    #[test]
    fn this_member_call_binds_scoped_to_the_enclosing_class() {
        let class = symbol("Widget.php", "App\\Widget", SymbolKind::Class, None);
        let method = symbol(
            "Widget.php",
            "App\\Widget::relaunch",
            SymbolKind::Method,
            Some("Widget.php#App\\Widget"),
        );
        let target = symbol(
            "Widget.php",
            "App\\Widget::launch",
            SymbolKind::Method,
            Some("Widget.php#App\\Widget"),
        );
        let store = seed(
            vec![class, method, target],
            vec![use_row(
                "Widget.php",
                "launch",
                RefKind::Call,
                Some("$this"),
                Some("Widget.php#App\\Widget::relaunch"),
                "2:0",
                "{\"kind\":\"this\"}",
            )],
            vec![scope("Widget.php", "2:0", None, &[], &[])],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "Widget.php#App\\Widget::launch");
        assert_eq!(binding.resolution, Resolution::Scoped);
    }

    #[test]
    fn self_const_read_binds_scoped_to_the_enclosing_class() {
        let class = symbol("Widget.php", "App\\Widget", SymbolKind::Class, None);
        let method = symbol(
            "Widget.php",
            "App\\Widget::run",
            SymbolKind::Method,
            Some("Widget.php#App\\Widget"),
        );
        let target = symbol(
            "Widget.php",
            "App\\Widget::DEFAULT_LABEL",
            SymbolKind::Const,
            Some("Widget.php#App\\Widget"),
        );
        let store = seed(
            vec![class, method, target],
            vec![use_row(
                "Widget.php",
                "DEFAULT_LABEL",
                RefKind::Read,
                Some("self"),
                Some("Widget.php#App\\Widget::run"),
                "2:0",
                "{\"kind\":\"self_or_static\"}",
            )],
            vec![scope("Widget.php", "2:0", None, &[], &[])],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "Widget.php#App\\Widget::DEFAULT_LABEL");
        assert_eq!(binding.resolution, Resolution::Scoped);
    }

    #[test]
    fn this_missing_member_records_nothing() {
        let class = symbol("Widget.php", "App\\Widget", SymbolKind::Class, None);
        let method = symbol(
            "Widget.php",
            "App\\Widget::run",
            SymbolKind::Method,
            Some("Widget.php#App\\Widget"),
        );
        let store = seed(
            vec![class, method],
            vec![use_row(
                "Widget.php",
                "missing",
                RefKind::Call,
                Some("$this"),
                Some("Widget.php#App\\Widget::run"),
                "2:0",
                "{\"kind\":\"this\"}",
            )],
            vec![scope("Widget.php", "2:0", None, &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn static_missing_member_records_nothing() {
        // `static::` resolves against the declaring class like `self::`, so a
        // member the class does not declare stays unbound.
        let class = symbol("Widget.php", "App\\Widget", SymbolKind::Class, None);
        let method = symbol(
            "Widget.php",
            "App\\Widget::run",
            SymbolKind::Method,
            Some("Widget.php#App\\Widget"),
        );
        let store = seed(
            vec![class, method],
            vec![use_row(
                "Widget.php",
                "launch",
                RefKind::Call,
                Some("static"),
                Some("Widget.php#App\\Widget::run"),
                "2:0",
                "{\"kind\":\"self_or_static\"}",
            )],
            vec![scope("Widget.php", "2:0", None, &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn typed_parameter_via_alias_import_binds_scoped() {
        let class = symbol(
            "SurveyService.php",
            "App\\Services\\SurveyService",
            SymbolKind::Class,
            None,
        );
        let target = symbol(
            "SurveyService.php",
            "App\\Services\\SurveyService::launch",
            SymbolKind::Method,
            Some("SurveyService.php#App\\Services\\SurveyService"),
        );
        let store = seed(
            vec![class, target],
            vec![use_row(
                "ReportService.php",
                "launch",
                RefKind::Call,
                Some("$svc"),
                None,
                "1:2",
                "{\"kind\":\"typed\",\"type_spelling\":\"SurveySvc\"}",
            )],
            vec![
                scope("ReportService.php", "1:2", Some("top:file"), &[], &[]),
                scope(
                    "ReportService.php",
                    "top:file",
                    None,
                    &[("SurveySvc", "App\\Services\\SurveyService", "class")],
                    &[],
                ),
            ],
        );
        let binding = only_binding(&store);
        assert_eq!(
            binding.target_id,
            "SurveyService.php#App\\Services\\SurveyService::launch"
        );
        assert_eq!(binding.resolution, Resolution::Scoped);
    }

    #[test]
    fn typed_receiver_with_unindexed_type_records_nothing() {
        let store = seed(
            Vec::new(),
            vec![use_row(
                "ReportService.php",
                "launch",
                RefKind::Call,
                Some("$svc"),
                None,
                "1:2",
                "{\"kind\":\"typed\",\"type_spelling\":\"NoSuchClass\"}",
            )],
            vec![scope("ReportService.php", "1:2", None, &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn parent_receiver_is_not_bound_in_v0_1() {
        // Inheritance traversal is not in v0.1, so `parent::launch()` must not
        // bind to the member of a class that merely declares it.
        let class = symbol("Child.php", "App\\Child", SymbolKind::Class, None);
        let method = symbol(
            "Child.php",
            "App\\Child::run",
            SymbolKind::Method,
            Some("Child.php#App\\Child"),
        );
        let store = seed(
            vec![class, method],
            vec![use_row(
                "Child.php",
                "launch",
                RefKind::Call,
                Some("parent"),
                Some("Child.php#App\\Child::run"),
                "2:0",
                "{\"kind\":\"self_or_static\"}",
            )],
            vec![scope("Child.php", "2:0", None, &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }
}
