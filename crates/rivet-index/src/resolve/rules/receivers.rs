//! `$this`/`self`/`static` member uses and explicit receiver types (spec §11.3
//! `scoped`; T20).
//!
//! Rule 1: a use whose hint is [`UseHint::This`] or [`UseHint::SelfOrStatic`]
//! names a member of the class that encloses its containing symbol. The member
//! binds only when that class declares it; inheritance is not traversed in
//! v0.1, so a missing member records nothing. `parent::` is deliberately not
//! bound because it names an ancestor class, and `static::` is not bound
//! because it is late static binding, which v0.1 does not resolve (spec §11.4;
//! AF4). Inside an anonymous class the extractor records no `This` or
//! `SelfOrStatic` hint at all, because there they name the anonymous class.
//!
//! Rule 3 (AF4): a [`UseHint::NamedClass`] receiver (`Foo::make()`,
//! `Foo::BAR`, `Foo::$prop`) resolves the class spelling through the same
//! scope chain as a `type` use, then binds the named member only when that
//! class declares it directly.
//!
//! Rule 2: a [`UseHint::Typed`] receiver resolves its type spelling through the
//! same alias/fully-qualified/namespace-relative scope chain as a `type` use,
//! then binds the named member of that class.
//!
//! AF3 splits rule 2 by where the type was declared, because PHP enforces the
//! two differently:
//!
//! - A typed **parameter** (including a promoted constructor parameter used
//!   as the local variable inside the constructor) is checked only when the
//!   function is called; the body may then assign anything to it. It binds
//!   only when the variable is never rebound anywhere in its scope, the scope
//!   is analysable, and no call argument may rebind it by reference, the same
//!   conditions that suppress a `new` receiver ([`rebinding`]).
//! - A typed **property** read through `$this->name` is checked on every
//!   assignment, so it can only hold that class or a subclass, and `scoped`
//!   already allows for subclass dispatch. Reassignment never suppresses it.
//!
//! Only a single class type, or a nullable one (`?A`), is recorded as a typed
//! receiver; a union, intersection, or DNF type binds nothing (AF3).
//!
//! [`rebinding`]: super::rebinding
//!
//! Both rules yield [`Resolution::Scoped`] only: late static binding and
//! runtime dispatch can select another implementation, so receiver evidence
//! never upgrades to `exact` (spec §11.3).

use rivet_core::extract::{TypedOrigin, UseHint};
use rivet_core::{Resolution, SymbolKind};
use rivet_store::{SymbolRow, UseRow};

use crate::resolve::rules::rebinding::local_untrustworthy;
use crate::resolve::rules::resolve_class_spelling;
use crate::resolve::{MemberUse, RuleCtx, ScopeFacts, SymbolId};

/// Resolves a `$this`/`self`/`static` member or an explicitly typed receiver.
pub(crate) fn resolve(
    ctx: &RuleCtx<'_>,
    use_row: &UseRow,
    facts: &ScopeFacts,
) -> Option<(SymbolId, Resolution)> {
    let hint: UseHint = serde_json::from_str(&use_row.hint_json).ok()?;
    // A method call binds only a method, a property access only a property,
    // and a class-constant read only a constant (AF2).
    let member = MemberUse::of(use_row, facts)?;
    let class = match hint {
        UseHint::This | UseHint::SelfOrStatic => {
            if !names_enclosing_class(use_row, &hint) {
                return None;
            }
            enclosing_class(ctx, use_row)?
        }
        UseHint::Typed {
            type_spelling,
            origin,
        } => {
            // A parameter type says nothing once the variable is rebound; a
            // property type holds on every assignment (AF3).
            if origin == TypedOrigin::Parameter {
                let variable = use_row.receiver.as_deref()?;
                if !is_bare_variable(variable) || local_untrustworthy(ctx, use_row, facts, variable)
                {
                    return None;
                }
            }
            resolve_class_spelling(ctx, &type_spelling, facts)?
        }
        UseHint::NamedClass { class_spelling } => {
            resolve_class_spelling(ctx, &class_spelling, facts)?
        }
        _ => return None,
    };
    ctx.unique_member(&class.id, &use_row.spelling, member)
        .map(|member| (member.id.clone(), Resolution::Scoped))
}

/// Whether a receiver text is one plain variable (`$svc`), the only form a
/// typed parameter receiver can take.
fn is_bare_variable(receiver: &str) -> bool {
    receiver.strip_prefix('$').is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric() || byte >= 0x80)
    })
}

/// Whether a `This`/`SelfOrStatic` hint names the enclosing class itself.
///
/// `$this` always does, and so does `self`. `parent` names the ancestor
/// class, which v0.1 does not traverse, and `static` is late static binding,
/// which may name a subclass (AF4; spec §11.4).
fn names_enclosing_class(use_row: &UseRow, hint: &UseHint) -> bool {
    match hint {
        UseHint::This => use_row.receiver.as_deref() == Some("$this"),
        UseHint::SelfOrStatic => use_row
            .receiver
            .as_deref()
            .is_some_and(|receiver| receiver.eq_ignore_ascii_case("self")),
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

    /// A scope row whose facts also list `constants` as class-constant access
    /// spans (AF2).
    fn scope_with_constants(file: &str, scope_key: &str, constants: &[(u32, u32)]) -> ScopeRow {
        let spans: Vec<String> = constants
            .iter()
            .map(|(start, end)| format!("{{\"start_byte\":{start},\"end_byte\":{end}}}"))
            .collect();
        ScopeRow {
            file: file.to_string(),
            scope_key: scope_key.to_string(),
            parent_scope_key: None,
            facts_json: format!(
                "{{\"imports\":[],\"typed_bindings\":[],\"new_bindings\":[],\"declares\":[],\
                 \"class_constant_accesses\":[{}]}}",
                spans.join(",")
            ),
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
            vec![scope_with_constants("Widget.php", "2:0", &[(0, 13)])],
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
    fn static_receiver_is_not_bound_even_when_the_member_exists() {
        // `static::` is late static binding and may name a subclass, which
        // v0.1 does not resolve (AF4; spec §11.4). Before AF4 this bound the
        // enclosing class's `run` scoped.
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
                "run",
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

    /// A `Foo::member` use under `namespace App` with a `NamedClass` hint.
    fn named_class_store(spelling: &str, ref_kind: RefKind, members: Vec<SymbolRow>) -> Store {
        let mut symbols = vec![
            symbol("Ns.php", "App", SymbolKind::Module, None),
            symbol("Foo.php", "App\\Foo", SymbolKind::Class, None),
        ];
        symbols.extend(members);
        seed(
            symbols,
            vec![use_row(
                "Ns.php",
                spelling,
                ref_kind,
                Some("Foo"),
                None,
                "ns0:file",
                "{\"kind\":\"named_class\",\"class_spelling\":\"Foo\"}",
            )],
            vec![scope("Ns.php", "ns0:file", None, &[], &["Ns.php#App"])],
        )
    }

    #[test]
    fn named_class_static_call_binds_a_declared_method_scoped() {
        let make = symbol(
            "Foo.php",
            "App\\Foo::make",
            SymbolKind::Method,
            Some("Foo.php#App\\Foo"),
        );
        let store = named_class_store("make", RefKind::Call, vec![make]);
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "Foo.php#App\\Foo::make");
        assert_eq!(binding.resolution, Resolution::Scoped);
    }

    #[test]
    fn named_class_static_call_to_an_undeclared_method_records_nothing() {
        // No inheritance traversal: a method the class does not declare
        // directly binds nothing, even when a same-name property exists.
        let items = symbol(
            "Foo.php",
            "App\\Foo::$make",
            SymbolKind::Property,
            Some("Foo.php#App\\Foo"),
        );
        let store = named_class_store("make", RefKind::Call, vec![items]);
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn named_class_with_an_unindexed_class_records_nothing() {
        let store = seed(
            vec![symbol("Ns.php", "App", SymbolKind::Module, None)],
            vec![use_row(
                "Ns.php",
                "make",
                RefKind::Call,
                Some("Missing"),
                None,
                "ns0:file",
                "{\"kind\":\"named_class\",\"class_spelling\":\"Missing\"}",
            )],
            vec![scope("Ns.php", "ns0:file", None, &[], &["Ns.php#App"])],
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

    /// A class `App\C` with a method `run` whose body holds the use, plus the
    /// given members as `(qualified suffix, kind)`.
    fn class_with(members: &[(&str, SymbolKind)]) -> Vec<SymbolRow> {
        let mut rows = vec![
            symbol("C.php", "App\\C", SymbolKind::Class, None),
            symbol(
                "C.php",
                "App\\C::run",
                SymbolKind::Method,
                Some("C.php#App\\C"),
            ),
        ];
        for (suffix, kind) in members {
            rows.push(symbol(
                "C.php",
                &format!("App\\C::{suffix}"),
                *kind,
                Some("C.php#App\\C"),
            ));
        }
        rows
    }

    /// A `$this`/`self` member use of `spelling` inside `App\C::run`.
    fn member_use(spelling: &str, ref_kind: RefKind, receiver: &str) -> UseRow {
        let hint = if receiver == "$this" {
            "{\"kind\":\"this\"}"
        } else {
            "{\"kind\":\"self_or_static\"}"
        };
        use_row(
            "C.php",
            spelling,
            ref_kind,
            Some(receiver),
            Some("C.php#App\\C::run"),
            "1:0",
            hint,
        )
    }

    #[test]
    fn method_call_with_only_a_same_name_property_records_nothing() {
        // `class C { public $items; function run(){ $this->items(); } }`
        let store = seed(
            class_with(&[("$items", SymbolKind::Property)]),
            vec![member_use("items", RefKind::Call, "$this")],
            vec![scope("C.php", "1:0", None, &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn property_read_with_only_a_same_name_method_records_nothing() {
        // `class C { function items(){} function run(){ $this->items; } }`
        let store = seed(
            class_with(&[("items", SymbolKind::Method)]),
            vec![member_use("items", RefKind::Read, "$this")],
            vec![scope("C.php", "1:0", None, &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn static_property_read_with_only_a_same_name_method_records_nothing() {
        // `self::$items` names a property, never the method `items()`.
        let store = seed(
            class_with(&[("items", SymbolKind::Method)]),
            vec![member_use("$items", RefKind::Read, "self")],
            vec![scope("C.php", "1:0", None, &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn property_write_with_only_a_same_name_method_records_nothing() {
        let store = seed(
            class_with(&[("items", SymbolKind::Method)]),
            vec![member_use("items", RefKind::Write, "$this")],
            vec![scope("C.php", "1:0", None, &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn class_constant_read_binds_only_a_constant() {
        // `self::MAX` with a property `$MAX` and a constant `MAX`: the read
        // recorded as a class-constant access binds the constant.
        let store = seed(
            class_with(&[("$MAX", SymbolKind::Property), ("MAX", SymbolKind::Const)]),
            vec![member_use("MAX", RefKind::Read, "self")],
            vec![scope_with_constants("C.php", "1:0", &[(0, 3)])],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "C.php#App\\C::MAX");
        assert_eq!(binding.resolution, Resolution::Scoped);
    }

    #[test]
    fn class_constant_read_with_only_a_same_name_property_records_nothing() {
        // `$this::items` is a constant access; the property `$items` is not it.
        let store = seed(
            class_with(&[("$items", SymbolKind::Property)]),
            vec![member_use("items", RefKind::Read, "$this")],
            vec![scope_with_constants("C.php", "1:0", &[(0, 5)])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn instance_property_read_does_not_bind_a_same_name_constant() {
        // `$this->MAX` (not a recorded constant access) names the property.
        let store = seed(
            class_with(&[("MAX", SymbolKind::Const)]),
            vec![member_use("MAX", RefKind::Read, "$this")],
            vec![scope("C.php", "1:0", None, &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn method_and_property_of_one_name_each_bind_their_own_kind() {
        let mut call = member_use("items", RefKind::Call, "$this");
        call.start_byte = 10;
        call.end_byte = 15;
        let mut read = member_use("items", RefKind::Read, "$this");
        read.start_byte = 30;
        read.end_byte = 35;
        let mut static_read = member_use("$items", RefKind::Read, "self");
        static_read.start_byte = 50;
        static_read.end_byte = 56;
        let store = seed(
            class_with(&[
                ("$items", SymbolKind::Property),
                ("items", SymbolKind::Method),
            ]),
            vec![call, read, static_read],
            vec![scope("C.php", "1:0", None, &[], &[])],
        );
        let bindings = resolve_all(&store).expect("resolve");
        let targets: Vec<&str> = bindings.iter().map(|b| b.target_id.as_str()).collect();
        assert_eq!(
            targets,
            vec![
                "C.php#App\\C::items",
                "C.php#App\\C::$items",
                "C.php#App\\C::$items"
            ]
        );
    }

    /// A scope row whose facts are the given JSON object.
    fn scope_json(file: &str, scope_key: &str, facts: serde_json::Value) -> ScopeRow {
        ScopeRow {
            file: file.to_string(),
            scope_key: scope_key.to_string(),
            parent_scope_key: None,
            facts_json: facts.to_string(),
        }
    }

    /// `A::m` plus a typed `$x->m()` use of the given origin in scope `1:0`.
    fn typed_case(origin: &str, receiver: &str, facts: serde_json::Value) -> Store {
        let class = symbol("A.php", "A", SymbolKind::Class, None);
        let method = symbol("A.php", "A::m", SymbolKind::Method, Some("A.php#A"));
        let hint =
            format!("{{\"kind\":\"typed\",\"type_spelling\":\"A\",\"origin\":\"{origin}\"}}");
        seed(
            vec![class, method],
            vec![use_row(
                "F.php",
                "m",
                RefKind::Call,
                Some(receiver),
                None,
                "1:0",
                &hint,
            )],
            vec![scope_json("F.php", "1:0", facts)],
        )
    }

    /// One non-`new` rebinding fact for `$x`.
    fn rebinding() -> serde_json::Value {
        serde_json::json!([{
            "variable": "$x",
            "class_spelling": "",
            "span": {"start_byte": 40, "end_byte": 42},
            "direct_new": false,
        }])
    }

    #[test]
    fn typed_parameter_never_rebound_binds_scoped() {
        let store = typed_case("parameter", "$x", serde_json::json!({}));
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "A.php#A::m");
        assert_eq!(binding.resolution, Resolution::Scoped);
    }

    #[test]
    fn typed_parameter_rebound_anywhere_records_nothing() {
        // AF3 finding 6: the rebinding may follow the use (a loop body).
        let store = typed_case(
            "parameter",
            "$x",
            serde_json::json!({ "new_bindings": rebinding() }),
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn typed_parameter_in_an_unanalysable_scope_records_nothing() {
        let store = typed_case(
            "parameter",
            "$x",
            serde_json::json!({ "unanalysable": true }),
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn typed_parameter_passed_to_an_unknown_function_records_nothing() {
        let store = typed_case(
            "parameter",
            "$x",
            serde_json::json!({ "call_args": [{
                "variable": "$x",
                "callee": "takes_ref",
                "kind": "function",
                "position": 0,
                "span": {"start_byte": 50, "end_byte": 52},
            }]}),
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn typed_parameter_hint_without_an_origin_is_treated_as_a_parameter() {
        // Facts written before AF3 carry no `origin`; the stricter reading
        // applies.
        let class = symbol("A.php", "A", SymbolKind::Class, None);
        let method = symbol("A.php", "A::m", SymbolKind::Method, Some("A.php#A"));
        let store = seed(
            vec![class, method],
            vec![use_row(
                "F.php",
                "m",
                RefKind::Call,
                Some("$x"),
                None,
                "1:0",
                "{\"kind\":\"typed\",\"type_spelling\":\"A\"}",
            )],
            vec![scope_json(
                "F.php",
                "1:0",
                serde_json::json!({ "new_bindings": rebinding() }),
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn typed_property_binds_despite_rebinding_facts() {
        // PHP checks a typed property on every assignment, so neither a
        // same-named local's rebinding nor an unanalysable scope unsettles it.
        let store = typed_case(
            "property",
            "$this->x",
            serde_json::json!({ "new_bindings": rebinding(), "unanalysable": true }),
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "A.php#A::m");
        assert_eq!(binding.resolution, Resolution::Scoped);
    }
}
