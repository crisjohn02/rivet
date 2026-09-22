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

use std::collections::{HashMap, HashSet};

use rivet_core::extract::{CallArg, NewBinding, ScopeImport};
use rivet_core::{ParseStatus, RefKind, Resolution, Span, SymbolKind};
use rivet_store::{BindingRow, Error, FileRow, ScopeRow, Store, SymbolRow, UseRow};
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
    /// The qualified name of the namespace block that owns the use, found as
    /// the `module` declared in its block's top-level scope. `None` in a file
    /// with no namespace and in a global `namespace { }` block (AF1).
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
    /// Whether any scope on the use's chain cannot be attributed to exactly
    /// one namespace block (AF1). Every rule depends on the namespace, so the
    /// driver records no binding for such a use.
    pub(crate) namespace_unattributed: bool,
    /// Whether the use's own scope records it as a class-constant access
    /// (`Foo::NAME`, `$x::NAME`) rather than an instance property access
    /// (AF2). See [`MemberUse::of`].
    pub(crate) class_constant_access: bool,
}

/// The member kind a receiver use can name (AF2).
///
/// PHP keeps methods, properties, and class constants in separate namespaces,
/// so `$this->items()` never names the property `$items`, and `$this->items`
/// never names a method `items()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberUse {
    /// A method call: `$x->name()`, `Foo::name()`.
    Method,
    /// An instance or static property access: `$x->name`, `Foo::$name`.
    Property,
    /// A class-constant access: `Foo::NAME`, `$x::NAME`.
    Constant,
}

impl MemberUse {
    /// The member kind `use_row` names, from what the extractor recorded.
    ///
    /// - A `call` names a method.
    /// - A `read`/`write` whose span the use's scope lists in
    ///   `class_constant_accesses` names a class constant; a constant name never
    ///   starts with `$`, so a `$` spelling there is contradictory and yields
    ///   `None`.
    /// - Any other `read`/`write` names a property: a `$`-prefixed spelling is a
    ///   static property access (`Foo::$name`), and an unprefixed one an
    ///   instance property access (`$x->name`).
    ///
    /// Every other use kind names no member.
    pub(crate) fn of(use_row: &UseRow, facts: &ScopeFacts) -> Option<MemberUse> {
        match use_row.ref_kind {
            RefKind::Call => Some(MemberUse::Method),
            RefKind::Read | RefKind::Write => {
                let dollar = use_row.spelling.starts_with('$');
                match (facts.class_constant_access, dollar) {
                    (true, false) => Some(MemberUse::Constant),
                    (true, true) => None,
                    (false, _) => Some(MemberUse::Property),
                }
            }
            _ => None,
        }
    }

    /// Whether a declaration of `kind` can be the member this use names.
    fn accepts(self, kind: SymbolKind) -> bool {
        matches!(
            (self, kind),
            (MemberUse::Method, SymbolKind::Method)
                | (MemberUse::Property, SymbolKind::Property)
                | (MemberUse::Constant, SymbolKind::Const)
        )
    }
}

/// One parsed `scopes` row's facts that resolution needs.
struct ScopeData<'a> {
    parent: Option<&'a str>,
    imports: Vec<ScopeImport>,
    declares: Vec<SymbolId>,
    new_bindings: Vec<NewBinding>,
    call_args: Vec<CallArg>,
    unanalysable: bool,
    namespace_unattributed: bool,
    class_constant_accesses: HashSet<(u32, u32)>,
}

/// The persisted `scopes.facts_json` shape, read back without reparsing.
///
/// `typed_bindings` is ignored in T19; T20/T21 extend this struct when their
/// rules need more facts. T21b adds `call_args` and `unanalysable`; AF1 adds
/// `namespace_unattributed`; AF2 adds `class_constant_accesses`.
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
    #[serde(default)]
    namespace_unattributed: bool,
    #[serde(default)]
    class_constant_accesses: Vec<Span>,
}

/// Symbol lookups shared by every rule.
///
/// Qualified names are indexed both exactly and case-folded so a rule can ask
/// for PHP's case-insensitive class/function comparison without re-scanning.
/// Folding is ASCII-only, as PHP folds identifiers (AF2): `Ä` and `ä` are
/// different names.
pub(crate) struct RuleCtx<'a> {
    symbols: &'a [SymbolRow],
    by_id: HashMap<&'a str, usize>,
    by_qname_exact: HashMap<&'a str, Vec<usize>>,
    by_qname_folded: HashMap<String, Vec<usize>>,
    /// Whether some PHP file of the snapshot should have been indexed but was
    /// not (AF2). An unindexed file may declare a namespaced function, so the
    /// global function fallback is not trustworthy. See
    /// [`unindexed_php_files`].
    pub(crate) php_files_unindexed: bool,
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
                .entry(symbol.qualified_name.to_ascii_lowercase())
                .or_default()
                .push(index);
        }
        RuleCtx {
            symbols,
            by_id,
            by_qname_exact,
            by_qname_folded,
            php_files_unindexed: false,
        }
    }

    /// Returns the symbol with canonical ID `id`, if present.
    pub(crate) fn symbol_by_id(&self, id: &str) -> Option<&'a SymbolRow> {
        self.by_id.get(id).map(|&index| &self.symbols[index])
    }

    /// Returns the sole symbol matching `qname` under `kinds`.
    ///
    /// `case_insensitive` ASCII-lowercases both sides. Multiple matches return `None`
    /// rather than guessing, so only unique candidates can bind.
    fn unique_by_qname(
        &self,
        qname: &str,
        case_insensitive: bool,
        kinds: &[SymbolKind],
    ) -> Option<&'a SymbolRow> {
        let indices = if case_insensitive {
            self.by_qname_folded.get(&qname.to_ascii_lowercase())
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

    /// Whether any class-like declaration has the qualified name `qname`
    /// under PHP's (ASCII) case-insensitive class comparison.
    pub(crate) fn any_class_like(&self, qname: &str) -> bool {
        self.by_qname_folded
            .get(&qname.to_ascii_lowercase())
            .is_some_and(|indices| {
                indices.iter().any(|&index| {
                    matches!(
                        self.symbols[index].kind,
                        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
                    )
                })
            })
    }

    /// The sole constant declaration whose exact qualified name is `qname`.
    pub(crate) fn unique_const(&self, qname: &str) -> Option<&'a SymbolRow> {
        self.unique_by_qname(qname, false, &[SymbolKind::Const])
    }

    /// The sole member of kind `member` declared directly in `class_id` whose
    /// name matches `spelling` under PHP's kind-dependent case rules (T20;
    /// AF2).
    ///
    /// Only methods, properties, and constants can be members, and a use
    /// matches only its own member kind: a method call never binds a
    /// same-name property, and a property access never binds a method. A
    /// missing member returns `None`: v0.1 never traverses inheritance
    /// (docs/ADDING-A-LANGUAGE "Not resolved in v0.1").
    pub(crate) fn unique_member(
        &self,
        class_id: &str,
        spelling: &str,
        member: MemberUse,
    ) -> Option<&'a SymbolRow> {
        let mut found: Option<&SymbolRow> = None;
        for row in self.symbols {
            if row.parent_id.as_deref() != Some(class_id)
                || !member.accepts(row.kind)
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

/// PHP's member-name comparison.
///
/// Methods are case-insensitive by ASCII folding, as PHP compares them.
/// Properties and constants are case-sensitive;
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
                    namespace_unattributed: parsed.namespace_unattributed,
                    class_constant_accesses: parsed
                        .class_constant_accesses
                        .iter()
                        .map(|span| (span.start_byte(), span.end_byte()))
                        .collect(),
                },
            );
        }
        Resolver {
            ctx,
            uses,
            scopes: scope_map,
        }
    }

    /// Records whether some PHP file of the snapshot should have been indexed
    /// but was not (AF2), computed by [`unindexed_php_files`].
    ///
    /// When set, an unqualified function call in a namespace whose namespaced
    /// function is not indexed records no binding instead of falling back to
    /// the global function: the unindexed file may declare the namespaced one.
    pub fn with_unindexed_php_files(mut self, unindexed: bool) -> Resolver<'a> {
        self.ctx.php_files_unindexed = unindexed;
        self
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
            // A use no single namespace block owns has no trustworthy lexical
            // context; never guess one (AF1).
            if facts.namespace_unattributed {
                continue;
            }
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
            namespace_unattributed: false,
            class_constant_access: false,
        };
        let mut key = Some(use_row.scope_key.as_str());
        let mut own_scope = true;
        while let Some(current) = key {
            let Some(scope) = self.scopes.get(&(use_row.file.as_str(), current)) else {
                break;
            };
            facts.imports.extend(scope.imports.iter().cloned());
            facts.declares.extend(scope.declares.iter().cloned());
            facts.namespace_unattributed |= scope.namespace_unattributed;
            // Variable assignments do not cross function bodies in PHP, so only
            // the use's own scope contributes them.
            if own_scope {
                facts.new_bindings = scope.new_bindings.clone();
                facts.call_args = scope.call_args.clone();
                facts.unanalysable = scope.unanalysable;
                facts.class_constant_access = scope
                    .class_constant_accesses
                    .contains(&(use_row.start_byte, use_row.end_byte));
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

/// Whether any file assigned the PHP language has no published facts (AF2).
///
/// Such a file was walked and recognized as PHP but skipped: for a parse
/// error, a parser resource limit, the size limit, invalid UTF-8, or a NUL byte
/// (`binary`). PHP itself would still load every one of those, so each may
/// declare a namespaced function the index cannot see. Every status other than
/// `ok` therefore counts. A file with no language or another language (a
/// README, a `.ts` file) never counts, so this is not the snapshot's overall
/// `coverage.complete` flag.
pub fn unindexed_php_files(files: &[FileRow]) -> bool {
    files
        .iter()
        .any(|file| file.language.as_deref() == Some("php") && file.parse_status != ParseStatus::Ok)
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
    let resolver = Resolver::new(&symbols, &uses, &scopes)
        .with_unindexed_php_files(unindexed_php_files(&files));
    Ok(resolver.resolve())
}

#[cfg(test)]
mod tests {
    use super::{Resolver, resolve_all, unindexed_php_files};
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

    #[test]
    fn fully_qualified_call_to_a_class_name_records_nothing() {
        // `class Maker {} \Maker();` (audit finding 4).
        let store = seed(
            vec![symbol("a.php", "Maker", SymbolKind::Class)],
            vec![use_row(
                "a.php",
                "\\Maker",
                RefKind::Call,
                None,
                "top:file",
                20,
            )],
            vec![scope("a.php", "top:file", &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    #[test]
    fn fully_qualified_call_binds_the_function_beside_a_same_name_class() {
        let store = seed(
            vec![
                symbol("a.php", "App\\Maker", SymbolKind::Class),
                symbol("b.php", "App\\maker", SymbolKind::Function),
            ],
            vec![use_row(
                "c.php",
                "\\App\\MAKER",
                RefKind::Call,
                None,
                "top:file",
                20,
            )],
            vec![scope("c.php", "top:file", &[], &[])],
        );
        let binding = only_binding(&store);
        assert_eq!(binding.target_id, "b.php#App\\maker");
        assert_eq!(binding.resolution, Resolution::Exact);
    }

    #[test]
    fn fully_qualified_constant_read_binds_only_a_constant() {
        let only_class = seed(
            vec![symbol("a.php", "App\\LIMIT", SymbolKind::Class)],
            vec![use_row(
                "c.php",
                "\\App\\LIMIT",
                RefKind::Unknown,
                None,
                "top:file",
                20,
            )],
            vec![scope("c.php", "top:file", &[], &[])],
        );
        assert!(resolve_all(&only_class).expect("resolve").is_empty());

        let with_const = seed(
            vec![
                symbol("a.php", "App\\Limits", SymbolKind::Class),
                symbol("b.php", "App\\LIMIT", SymbolKind::Const),
            ],
            vec![use_row(
                "c.php",
                "\\App\\LIMIT",
                RefKind::Unknown,
                None,
                "top:file",
                20,
            )],
            vec![scope("c.php", "top:file", &[], &[])],
        );
        assert_eq!(only_binding(&with_const).target_id, "b.php#App\\LIMIT");

        // An `Unknown` use may be a class name (`instanceof \App\LIMIT`), so a
        // constant and a class-like of one name leave it unresolved.
        let both = seed(
            vec![
                symbol("a.php", "App\\Limit", SymbolKind::Class),
                symbol("b.php", "App\\LIMIT", SymbolKind::Const),
            ],
            vec![use_row(
                "c.php",
                "\\App\\LIMIT",
                RefKind::Unknown,
                None,
                "top:file",
                20,
            )],
            vec![scope("c.php", "top:file", &[], &[])],
        );
        assert!(resolve_all(&both).expect("resolve").is_empty());
    }

    #[test]
    fn non_ascii_class_names_do_not_fold() {
        // PHP folds ASCII only: `class Ä {}` is not `new ä()` (finding 10).
        let store = seed(
            vec![symbol("a.php", "App\\Ä", SymbolKind::Class)],
            vec![use_row(
                "c.php",
                "\\App\\ä",
                RefKind::Type,
                None,
                "top:file",
                20,
            )],
            vec![scope("c.php", "top:file", &[], &[])],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());

        let ascii = seed(
            vec![symbol("a.php", "App\\Foo", SymbolKind::Class)],
            vec![use_row(
                "c.php",
                "\\app\\FOO",
                RefKind::Type,
                None,
                "top:file",
                20,
            )],
            vec![scope("c.php", "top:file", &[], &[])],
        );
        assert_eq!(only_binding(&ascii).target_id, "a.php#App\\Foo");
    }

    #[test]
    fn non_ascii_import_alias_does_not_fold() {
        let store = seed(
            vec![symbol("a.php", "App\\Ärger", SymbolKind::Class)],
            vec![use_row(
                "c.php",
                "ärger",
                RefKind::Type,
                None,
                "top:file",
                40,
            )],
            vec![scope(
                "c.php",
                "top:file",
                &[("Ärger", "App\\Ärger", "class")],
                &[],
            )],
        );
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }

    /// The rows of a namespaced `launch()` call in `App` with a global
    /// `launch` indexed and, optionally, `App\launch` indexed too.
    fn fallback_rows(with_namespaced: bool) -> (Vec<SymbolRow>, Vec<UseRow>, Vec<ScopeRow>) {
        let module = symbol("c.php", "App", SymbolKind::Module);
        let mut symbols = vec![
            module.clone(),
            symbol("util.php", "launch", SymbolKind::Function),
        ];
        if with_namespaced {
            symbols.push(symbol("fns.php", "App\\launch", SymbolKind::Function));
        }
        let mut call = use_row("c.php", "launch", RefKind::Call, None, "top:file", 40);
        call.use_id = Some(1);
        let scopes = vec![scope("c.php", "top:file", &[], &[&module.id])];
        (symbols, vec![call], scopes)
    }

    #[test]
    fn global_fallback_is_suppressed_when_a_php_file_is_unindexed() {
        let (symbols, uses, scopes) = fallback_rows(false);
        let complete = Resolver::new(&symbols, &uses, &scopes).resolve();
        assert_eq!(complete.len(), 1);
        assert_eq!(complete[0].target_id, "util.php#launch");

        let partial = Resolver::new(&symbols, &uses, &scopes)
            .with_unindexed_php_files(true)
            .resolve();
        assert!(partial.is_empty(), "{partial:?}");
    }

    #[test]
    fn indexed_namespaced_function_still_wins_when_a_php_file_is_unindexed() {
        let (symbols, uses, scopes) = fallback_rows(true);
        let partial = Resolver::new(&symbols, &uses, &scopes)
            .with_unindexed_php_files(true)
            .resolve();
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].target_id, "fns.php#App\\launch");
        assert_eq!(partial[0].resolution, Resolution::Exact);
    }

    #[test]
    fn global_call_outside_a_namespace_is_not_a_fallback() {
        let mut call = use_row("c.php", "launch", RefKind::Call, None, "top:file", 40);
        call.use_id = Some(1);
        let symbols = vec![symbol("util.php", "launch", SymbolKind::Function)];
        let uses = vec![call];
        let scopes = vec![scope("c.php", "top:file", &[], &[])];
        let bindings = Resolver::new(&symbols, &uses, &scopes)
            .with_unindexed_php_files(true)
            .resolve();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].target_id, "util.php#launch");
    }

    #[test]
    fn only_skipped_php_files_count_as_unindexed() {
        let mut readme = file("README.md");
        readme.language = None;
        readme.parse_status = ParseStatus::Unsupported;
        let mut ts = file("a.ts");
        ts.language = Some("typescript".to_string());
        ts.parse_status = ParseStatus::ParseError;
        let ok = file("a.php");
        assert!(!unindexed_php_files(&[
            readme.clone(),
            ts.clone(),
            ok.clone()
        ]));
        for status in [
            ParseStatus::ParseError,
            ParseStatus::ResourceLimit,
            ParseStatus::Size,
            ParseStatus::Encoding,
            ParseStatus::Binary,
        ] {
            let mut broken = file("b.php");
            broken.parse_status = status;
            assert!(
                unindexed_php_files(&[readme.clone(), ok.clone(), broken]),
                "{status:?}"
            );
        }
    }

    #[test]
    fn resolve_all_reads_unindexed_php_files_from_the_snapshot() {
        let (symbols, mut uses, scopes) = fallback_rows(false);
        uses[0].use_id = None;
        let mut store = Store::open_in_memory().expect("open in-memory store");
        let mut broken = file("fns.php");
        broken.parse_status = ParseStatus::ParseError;
        store
            .publish_inventory(InventoryInput {
                fingerprint: Fingerprint {
                    index_format_version: INDEX_FORMAT_VERSION.to_string(),
                    effective_config: "test".to_string(),
                    extractor: "test".to_string(),
                    resolver: "php-rules-v1".to_string(),
                },
                files: vec![file("c.php"), broken, file("util.php")],
                symbols,
                uses,
                scopes,
                bindings: Vec::new(),
                force: false,
            })
            .expect("publish");
        assert!(resolve_all(&store).expect("resolve").is_empty());
    }
}
