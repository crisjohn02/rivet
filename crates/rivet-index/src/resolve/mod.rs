//! Declaration resolution against one committed snapshot (spec §11; T19).
//!
//! [`Resolver`] loads every persisted symbol, use, and scope for a snapshot and
//! applies an ordered list of rule functions to each use in `(file bytes,
//! start_byte, end_byte, ref_kind)` order. The first rule that returns exactly
//! one candidate wins; a rule that sees more than one candidate returns `None`,
//! so conflicting lexical hints never produce a binding (spec §11.4).
//!
//! T19 provides the `imports` and `functions` rules under [`rules`]. T20 adds
//! `receivers.rs` and T21 adds `new_expr.rs` to the same ordered list without
//! changing this driver.
//!
//! [`resolve_all`] is the one-shot entry point for a committed snapshot. The
//! refresh path builds a [`Resolver`] directly over the rows it is about to
//! publish, so resolved links land in the same publication transaction.

mod rules;

use std::collections::HashMap;

use rivet_core::extract::{CallArg, NewBinding, ScopeImport};
use rivet_core::{Resolution, SymbolKind};
use rivet_store::{BindingRow, Error, ScopeRow, Store, SymbolRow, UseRow};
use serde::Deserialize;

/// The canonical ID of the one declaration a rule selected.
pub(crate) type SymbolId = String;

/// The signature every resolution rule implements.
pub(crate) type RuleFn = fn(&RuleCtx<'_>, &UseRow, &ScopeFacts) -> Option<(SymbolId, Resolution)>;

/// The ordered rule list. The first single-candidate rule wins. T20 registers
/// `receivers` and T21 registers `new_expr` after these entries.
const RULES: &[RuleFn] = &[
    rules::imports::resolve,
    rules::functions::resolve,
    rules::receivers::resolve,
    rules::new_expr::resolve,
];

/// Lexical facts visible from one use, gathered along its scope chain.
pub(crate) struct ScopeFacts {
    /// Import aliases visible from the use, nearest scope first.
    pub(crate) imports: Vec<ScopeImport>,
    /// Canonical IDs declared directly in the use's scope chain, nearest first.
    pub(crate) declares: Vec<SymbolId>,
    /// The enclosing namespace's qualified name, when the file declares one.
    pub(crate) namespace: Option<String>,
    /// Simple variable assignments recorded directly in the use's own scope.
    /// PHP variables do not cross function bodies, so only the use's own scope
    /// is considered; parent scopes are deliberately excluded.
    pub(crate) new_bindings: Vec<NewBinding>,
    /// Call arguments recorded directly in the use's own scope that pass a bare
    /// variable and may rebind it (T21b). Like `new_bindings`, only the use's
    /// own scope contributes.
    pub(crate) call_args: Vec<CallArg>,
    /// Whether the use's own scope contains a construct whose effect on local
    /// variables cannot be bounded (T21b): a dynamic variable write, a
    /// `$GLOBALS` write, `extract`, `eval`, or a dynamic-callee call. When set,
    /// no `new`-receiver binding is recorded anywhere in that scope.
    pub(crate) unanalysable: bool,
}

/// One parsed `scopes` row's facts that resolution needs.
struct ScopeData<'a> {
    parent: Option<&'a str>,
    imports: Vec<ScopeImport>,
    declares: Vec<SymbolId>,
    new_bindings: Vec<NewBinding>,
    call_args: Vec<CallArg>,
    unanalysable: bool,
}

/// The persisted `scopes.facts_json` shape, read back without reparsing.
///
/// `typed_bindings` is ignored in T19; T20/T21 extend this struct when their
/// rules need more facts. T21b adds `call_args` and `unanalysable`.
#[derive(Deserialize, Default)]
struct PersistedScopeFacts {
    #[serde(default)]
    imports: Vec<ScopeImport>,
    #[serde(default)]
    declares: Vec<SymbolId>,
    #[serde(default)]
    new_bindings: Vec<NewBinding>,
    #[serde(default)]
    call_args: Vec<CallArg>,
    #[serde(default)]
    unanalysable: bool,
}

/// Symbol lookups shared by every rule.
///
/// Qualified names are indexed both exactly and case-folded so a rule can ask
/// for PHP's case-insensitive class/function comparison without re-scanning.
pub(crate) struct RuleCtx<'a> {
    symbols: &'a [SymbolRow],
    by_id: HashMap<&'a str, usize>,
    by_qname_exact: HashMap<&'a str, Vec<usize>>,
    by_qname_folded: HashMap<String, Vec<usize>>,
}

impl<'a> RuleCtx<'a> {
    /// Builds the symbol indexes for one snapshot.
    fn new(symbols: &'a [SymbolRow]) -> RuleCtx<'a> {
        let mut by_id = HashMap::with_capacity(symbols.len());
        let mut by_qname_exact: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut by_qname_folded: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, symbol) in symbols.iter().enumerate() {
            by_id.insert(symbol.id.as_str(), index);
            by_qname_exact
                .entry(symbol.qualified_name.as_str())
                .or_default()
                .push(index);
            by_qname_folded
                .entry(symbol.qualified_name.to_lowercase())
                .or_default()
                .push(index);
        }
        RuleCtx {
            symbols,
            by_id,
            by_qname_exact,
            by_qname_folded,
        }
    }

    /// Returns the symbol with canonical ID `id`, if present.
    pub(crate) fn symbol_by_id(&self, id: &str) -> Option<&'a SymbolRow> {
        self.by_id.get(id).map(|&index| &self.symbols[index])
    }

    /// Returns the sole symbol matching `qname` under `kinds`.
    ///
    /// `case_insensitive` lowercases both sides. Multiple matches return `None`
    /// rather than guessing, so only unique candidates can bind.
    fn unique_by_qname(
        &self,
        qname: &str,
        case_insensitive: bool,
        kinds: &[SymbolKind],
    ) -> Option<&'a SymbolRow> {
        let indices = if case_insensitive {
            self.by_qname_folded.get(&qname.to_lowercase())
        } else {
            self.by_qname_exact.get(qname)
        }?;
        let mut found: Option<&SymbolRow> = None;
        for &index in indices {
            let row = &self.symbols[index];
            if !kinds.is_empty() && !kinds.contains(&row.kind) {
                continue;
            }
            match found {
                None => found = Some(row),
                Some(existing) if existing.id == row.id => {}
                Some(_) => return None,
            }
        }
        found
    }

    /// The sole function declaration whose qualified name is `qname`.
    pub(crate) fn unique_function(&self, qname: &str) -> Option<&'a SymbolRow> {
        self.unique_by_qname(qname, true, &[SymbolKind::Function])
    }

    /// The sole class-like declaration whose qualified name is `qname`.
    pub(crate) fn unique_class_like(&self, qname: &str) -> Option<&'a SymbolRow> {
        self.unique_by_qname(
            qname,
            true,
            &[SymbolKind::Class, SymbolKind::Interface, SymbolKind::Enum],
        )
    }

    /// The sole constant declaration whose exact qualified name is `qname`.
    pub(crate) fn unique_const(&self, qname: &str) -> Option<&'a SymbolRow> {
        self.unique_by_qname(qname, false, &[SymbolKind::Const])
    }

    /// The sole symbol matching a fully qualified name.
    ///
    /// Exact matches of any kind are candidates; case-folded matches are
    /// allowed only for PHP's case-insensitive kinds, so a wrong-case constant
    /// never binds.
    pub(crate) fn unique_resolved(&self, qname: &str) -> Option<&'a SymbolRow> {
        let folded = qname.to_lowercase();
        let mut found: Option<&SymbolRow> = None;
        let exact = self.by_qname_exact.get(qname).into_iter().flatten();
        let folded_matches = self.by_qname_folded.get(&folded).into_iter().flatten();
        for &index in exact.chain(folded_matches) {
            let row = &self.symbols[index];
            if !case_insensitive_kind(row.kind) && row.qualified_name != qname {
                continue;
            }
            match found {
                None => found = Some(row),
                Some(existing) if existing.id == row.id => {}
                Some(_) => return None,
            }
        }
        found
    }

    /// The sole member declared directly in `class_id` whose name matches
    /// `spelling` under PHP's kind-dependent case rules (T20).
    ///
    /// Only methods, properties, and constants can be members. A missing member
    /// returns `None`: v0.1 never traverses inheritance (docs/ADDING-A-LANGUAGE
    /// "Not resolved in v0.1").
    pub(crate) fn unique_member(&self, class_id: &str, spelling: &str) -> Option<&'a SymbolRow> {
        let mut found: Option<&SymbolRow> = None;
        for row in self.symbols {
            if row.parent_id.as_deref() != Some(class_id)
                || !member_name_matches(row.kind, &row.name, spelling)
            {
                continue;
            }
            match found {
                None => found = Some(row),
                Some(existing) if existing.id == row.id => {}
                Some(_) => return None,
            }
        }
        found
    }
}

/// Whether PHP treats `kind` case-insensitively for lookup.
fn case_insensitive_kind(kind: SymbolKind) -> bool {
    !matches!(kind, SymbolKind::Property | SymbolKind::Const)
}

/// PHP's member-name comparison.
///
/// Methods are case-insensitive. Properties and constants are case-sensitive;
/// a property declaration keeps its `$`, while a `$this->name` spelling omits
/// it, so both sides are compared without the leading `$`.
fn member_name_matches(kind: SymbolKind, name: &str, spelling: &str) -> bool {
    match kind {
        SymbolKind::Method => name.eq_ignore_ascii_case(spelling),
        SymbolKind::Property => {
            name.strip_prefix('$').unwrap_or(name) == spelling.strip_prefix('$').unwrap_or(spelling)
        }
        SymbolKind::Const => name == spelling,
        _ => false,
    }
}

/// Resolves every persisted use for one snapshot.
pub struct Resolver<'a> {
    ctx: RuleCtx<'a>,
    uses: &'a [UseRow],
    scopes: HashMap<(&'a str, &'a str), ScopeData<'a>>,
}

impl<'a> Resolver<'a> {
    /// Builds a resolver over already-loaded snapshot rows.
    pub fn new(
        symbols: &'a [SymbolRow],
        uses: &'a [UseRow],
        scopes: &'a [ScopeRow],
    ) -> Resolver<'a> {
        let ctx = RuleCtx::new(symbols);
        let mut scope_map = HashMap::with_capacity(scopes.len());
        for row in scopes {
            let parsed =
                serde_json::from_str::<PersistedScopeFacts>(&row.facts_json).unwrap_or_default();
            scope_map.insert(
                (row.file.as_str(), row.scope_key.as_str()),
                ScopeData {
                    parent: row.parent_scope_key.as_deref(),
                    imports: parsed.imports,
                    declares: parsed.declares,
                    new_bindings: parsed.new_bindings,
                    call_args: parsed.call_args,
                    unanalysable: parsed.unanalysable,
                },
            );
        }
        Resolver {
            ctx,
            uses,
            scopes: scope_map,
        }
    }

    /// Applies the ordered rules to every use and returns the bindings.
    ///
    /// Uses are visited in `(file bytes, start_byte, end_byte, ref_kind)` order
    /// so the result is deterministic and independent of input order. A use
    /// with no SQLite ID is skipped: it is not a committed row and cannot be
    /// the target of a foreign key.
    pub fn resolve(&self) -> Vec<BindingRow> {
        let mut ordered: Vec<&UseRow> = self.uses.iter().collect();
        ordered.sort_by(|a, b| {
            a.file
                .as_bytes()
                .cmp(b.file.as_bytes())
                .then(a.start_byte.cmp(&b.start_byte))
                .then(a.end_byte.cmp(&b.end_byte))
                .then_with(|| a.ref_kind.cmp(&b.ref_kind))
        });

        let mut bindings = Vec::new();
        for use_row in ordered {
            let Some(use_id) = use_row.use_id else {
                continue;
            };
            let facts = self.scope_facts_for(use_row);
            for rule in RULES {
                if let Some((target_id, resolution)) = rule(&self.ctx, use_row, &facts) {
                    bindings.push(BindingRow {
                        use_id,
                        target_id,
                        resolution,
                    });
                    break;
                }
            }
        }
        bindings
    }

    /// Gathers the lexical facts visible from `use_row`.
    fn scope_facts_for(&self, use_row: &UseRow) -> ScopeFacts {
        let mut facts = ScopeFacts {
            imports: Vec::new(),
            declares: Vec::new(),
            namespace: None,
            new_bindings: Vec::new(),
            call_args: Vec::new(),
            unanalysable: false,
        };
        let mut key = Some(use_row.scope_key.as_str());
        let mut own_scope = true;
        while let Some(current) = key {
            let Some(scope) = self.scopes.get(&(use_row.file.as_str(), current)) else {
                break;
            };
            facts.imports.extend(scope.imports.iter().cloned());
            facts.declares.extend(scope.declares.iter().cloned());
            // Variable assignments do not cross function bodies in PHP, so only
            // the use's own scope contributes them.
            if own_scope {
                facts.new_bindings = scope.new_bindings.clone();
                facts.call_args = scope.call_args.clone();
                facts.unanalysable = scope.unanalysable;
                own_scope = false;
            }
            if facts.namespace.is_none() {
                for id in &scope.declares {
                    if let Some(symbol) = self.ctx.symbol_by_id(id)
                        && symbol.kind == SymbolKind::Module
                    {
                        facts.namespace = Some(symbol.qualified_name.clone());
                        break;
                    }
                }
            }
            key = scope.parent;
        }
        facts
    }
}

/// Loads one committed snapshot and resolves every persisted use.
pub fn resolve_all(store: &Store) -> Result<Vec<BindingRow>, Error> {
    let symbols = store.list_symbols()?;
    let files = store.list_files()?;
    let mut uses = Vec::new();
    let mut scopes = Vec::new();
    for file in &files {
        uses.extend(store.list_uses_for_file(&file.path)?);
        scopes.extend(store.list_scopes_for_file(&file.path)?);
    }
    let resolver = Resolver::new(&symbols, &uses, &scopes);
    Ok(resolver.resolve())
}

#[cfg(test)]
mod tests {
    use super::resolve_all;
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

    /// A declaration row whose short name is the last qualified segment.
    fn symbol(file: &str, qname: &str, kind: SymbolKind) -> SymbolRow {
        let name = qname
            .rsplit(['\\', ':'])
            .next()
            .unwrap_or(qname)
            .trim_start_matches('$');
        SymbolRow {
            id: format!("{file}#{qname}"),
            file: file.to_string(),
            name: name.to_string(),
            lookup_name: name.to_lowercase(),
            qualified_name: qname.to_string(),
            kind,
            parent_id: None,
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            end_line: 1,
            signature: None,
            doc_comment: None,
        }
    }

    /// A use row with an unresolved hint in `scope_key`.
    fn use_row(
        file: &str,
        spelling: &str,
        ref_kind: RefKind,
        receiver: Option<&str>,
        scope_key: &str,
        start: u32,
    ) -> UseRow {
        UseRow {
            use_id: None,
            file: file.to_string(),
            containing_symbol: None,
            scope_key: scope_key.to_string(),
            spelling: spelling.to_string(),
            lookup_name: spelling.to_lowercase(),
            ref_kind,
            start_byte: start,
            end_byte: start + spelling.len() as u32,
            line: 1,
            col: 1,
            receiver: receiver.map(str::to_string),
            hint_json: "{\"kind\":\"unresolved\"}".to_string(),
        }
    }

    /// A scope row with no parent and generated facts JSON.
    fn scope(
        file: &str,
        scope_key: &str,
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
            parent_scope_key: None,
            facts_json,
        }
    }

    /// Publishes the given rows into an in-memory store and returns it.
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
            .expect("publish resolver fixture");
        store
    }

    /// The single binding produced by one seeded use.
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
    fn type_alias_binds_to_the_imported_class() {
        let store = seed(
            vec![symbol(
                "SurveyService.php",
                "App\\Services\\SurveyService",
                SymbolKind::Class,
            )],
            vec![use_row(
                "ReportService.php",
                "SurveySvc",
                RefKind::Type,
                None,
                "top:file",
                564,
            )],
            vec![scope(
                "ReportService.php",
                "top:file",
                &[("SurveySvc", "App\\Services\\SurveyService", "class")],
                &[],
            )],
        );
        let binding = only_binding(&store);
        assert_eq!(
            binding.target_id,
            "SurveyService.php#App\\Services\\SurveyService"
        );
        assert_eq!(binding.resolution, Resolution::Exact);
    }

    #[test]
    fn fully_qualified_type_binds_directly() {
        let store = seed(
            vec![symbol(
                "SurveyService.php",
                "App\\Services\\SurveyService",
                SymbolKind::Class,
            )],
            vec![use_row(
                "boot.php",
                "\\App\\Services\\SurveyService",
                RefKind::Type,
                None,
                "top:file",
                230,
            )],
            vec![scope("boot.php", "top:file", &[], &[])],
        );
        let binding = only_binding(&store);
        assert_eq!(
            binding.target_id,
            "SurveyService.php#App\\Services\\SurveyService"
        );
    }

    #[test]
    fn unindexed_alias_target_yields_no_binding() {
        let store = seed(
            Vec::new(),
            vec![use_row(
                "ReportService.php",
                "SurveySvc",
                RefKind::Type,
                None,
                "top:file",
                564,
            )],
            vec![scope(
                "ReportService.php",
                "top:file",
                &[("SurveySvc", "App\\Services\\SurveyService", "class")],
                &[],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn namespace_relative_class_binds_without_an_import() {
        let module = symbol("Foo.php", "App\\Reporting", SymbolKind::Module);
        let class = symbol("Foo.php", "App\\Reporting\\Foo", SymbolKind::Class);
        let store = seed(
            vec![module.clone(), class],
            vec![use_row(
                "Foo.php",
                "Foo",
                RefKind::Type,
                None,
                "top:file",
                10,
            )],
            vec![scope("Foo.php", "top:file", &[], &[&module.id])],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "Foo.php#App\\Reporting\\Foo");
    }

    #[test]
    fn namespaced_function_call_binds() {
        let module = symbol("boot.php", "App\\Boot", SymbolKind::Module);
        let function = symbol("boot.php", "App\\Boot\\launch", SymbolKind::Function);
        let store = seed(
            vec![module.clone(), function],
            vec![use_row(
                "boot.php",
                "launch",
                RefKind::Call,
                None,
                "top:file",
                179,
            )],
            vec![scope(
                "boot.php",
                "top:file",
                &[],
                &[&module.id, "boot.php#App\\Boot\\launch"],
            )],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "boot.php#App\\Boot\\launch");
        assert_eq!(binding.resolution, Resolution::Exact);
    }

    #[test]
    fn global_function_fallback_binds() {
        let store = seed(
            vec![symbol("util.php", "launch", SymbolKind::Function)],
            vec![use_row(
                "c.php",
                "launch",
                RefKind::Call,
                None,
                "top:file",
                40,
            )],
            vec![scope("c.php", "top:file", &[], &[])],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "util.php#launch");
    }

    #[test]
    fn namespaced_function_wins_over_the_global_one() {
        let module = symbol("boot.php", "App\\Boot", SymbolKind::Module);
        let namespaced = symbol("boot.php", "App\\Boot\\launch", SymbolKind::Function);
        let global = symbol("util.php", "launch", SymbolKind::Function);
        let store = seed(
            vec![module.clone(), namespaced, global],
            vec![use_row(
                "boot.php",
                "launch",
                RefKind::Call,
                None,
                "top:file",
                179,
            )],
            vec![scope("boot.php", "top:file", &[], &[&module.id])],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "boot.php#App\\Boot\\launch");
    }

    #[test]
    fn receiver_call_is_not_bound_by_t19() {
        let module = symbol("boot.php", "App\\Boot", SymbolKind::Module);
        let function = symbol("boot.php", "App\\Boot\\launch", SymbolKind::Function);
        let store = seed(
            vec![module.clone(), function],
            vec![use_row(
                "boot.php",
                "launch",
                RefKind::Call,
                Some("$svc"),
                "top:file",
                267,
            )],
            vec![scope("boot.php", "top:file", &[], &[&module.id])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn ambiguous_case_folded_class_candidates_stay_unresolved() {
        let store = seed(
            vec![
                symbol("a.php", "App\\Foo", SymbolKind::Class),
                symbol("b.php", "app\\foo", SymbolKind::Class),
            ],
            vec![use_row(
                "boot.php",
                "\\App\\Foo",
                RefKind::Type,
                None,
                "top:file",
                230,
            )],
            vec![scope("boot.php", "top:file", &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn function_import_alias_binds_a_call() {
        let store = seed(
            vec![
                symbol("A.php", "A\\launch", SymbolKind::Function),
                symbol("B.php", "B\\launch", SymbolKind::Function),
            ],
            vec![use_row(
                "c.php",
                "launch",
                RefKind::Call,
                None,
                "top:file",
                40,
            )],
            vec![scope(
                "c.php",
                "top:file",
                &[("launch", "A\\launch", "function")],
                &[],
            )],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "A.php#A\\launch");
    }

    #[test]
    fn unindexed_function_import_blocks_the_name_fallback() {
        // `C\launch` exists and C is the use's namespace, but the visible
        // `use function A\launch;` owns the name and its target is unindexed.
        let module = symbol("c.php", "C", SymbolKind::Module);
        let namespaced = symbol("c.php", "C\\launch", SymbolKind::Function);
        let store = seed(
            vec![module.clone(), namespaced],
            vec![use_row(
                "c.php",
                "launch",
                RefKind::Call,
                None,
                "top:file",
                40,
            )],
            vec![scope(
                "c.php",
                "top:file",
                &[("launch", "A\\launch", "function")],
                &[&module.id],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn conflicting_function_imports_stay_unresolved() {
        let store = seed(
            vec![
                symbol("A.php", "A\\launch", SymbolKind::Function),
                symbol("B.php", "B\\launch", SymbolKind::Function),
            ],
            vec![use_row(
                "c.php",
                "launch",
                RefKind::Call,
                None,
                "top:file",
                40,
            )],
            vec![scope(
                "c.php",
                "top:file",
                &[
                    ("launch", "A\\launch", "function"),
                    ("launch", "B\\launch", "function"),
                ],
                &[],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }
}
