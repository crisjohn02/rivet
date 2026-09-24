//! Declared class hierarchy of one committed snapshot (T36d).
//!
//! The PHP adapter records every name in a named class's, interface's, or
//! enum's `extends`/`implements` clauses twice: as an ordinary `type` use,
//! which the resolver binds (or not) through the import and namespace rules
//! like any other class name, and as a `supertypes` scope fact keyed by the
//! declaring symbol's canonical ID and the use's span. This module joins the
//! two, so a supertype is resolved exactly when its `type` use is bound, at
//! the tier the rules gave it, and never otherwise.
//!
//! An unbound supertype (a vendor class, an unindexed file, an ambiguous or
//! conflicting import) is kept with `resolved: None` and the qualified name
//! PHP's compile-time name resolution gives the spelling. That name is a
//! deterministic function of the spelling, the file's `use` imports, and its
//! namespace, all of which are already stored per scope; it is never a guess at
//! an indexed declaration. Where the namespace itself cannot be attributed
//! (AF1), or two imports claim the same alias, the qualified name is `None`.
//!
//! Traits are out of scope: `use Trait;` inside a class body is not a
//! supertype clause and records no fact.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rivet_core::extract::SupertypeRelation;
use rivet_core::{RefKind, Span, SymbolKind};
use rivet_store::{BindingRow, Error, ScopeRow, Store, SymbolRow, UseRow};
use serde::Deserialize;

use crate::resolve::Resolver;
use crate::resolve::rules::php_qualified_name;

/// One declared supertype of a class-like symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supertype {
    /// Whether the header names it in `extends` or `implements`. For an
    /// [`ancestors`](Hierarchy::ancestors) entry, the relation of the edge that
    /// first reached it (shallowest level, then sorted order).
    pub relation: SupertypeRelation,
    /// The name exactly as written in the clause that introduced it.
    pub spelling: String,
    /// The fully qualified name, without a leading `\`: the indexed symbol's
    /// qualified name when resolved, otherwise PHP's compile-time resolution
    /// of `spelling` against the file's imports and namespace. `None` when
    /// that lexical context is not trustworthy.
    pub qualified_name: Option<String>,
    /// The canonical ID of the indexed class, interface, or enum the
    /// supertype's `type` use is bound to, or `None` when it is not bound.
    pub resolved: Option<String>,
}

impl Supertype {
    /// The identity two entries share when they name the same supertype:
    /// the resolved ID, else the ASCII-folded qualified name (PHP class names
    /// compare case-insensitively), else the spelling.
    fn identity(&self) -> (u8, String) {
        match (&self.resolved, &self.qualified_name) {
            (Some(id), _) => (0, id.clone()),
            (None, Some(qname)) => (1, qname.to_ascii_lowercase()),
            (None, None) => (2, self.spelling.clone()),
        }
    }

    /// The total sort key of one entry.
    fn sort_key(&self) -> (SupertypeRelation, String, &str, &str, &str) {
        let qname = self.qualified_name.as_deref().unwrap_or("");
        (
            self.relation,
            qname.to_ascii_lowercase(),
            qname,
            self.resolved.as_deref().unwrap_or(""),
            self.spelling.as_str(),
        )
    }
}

/// The persisted `supertypes` entry of `scopes.facts_json`.
#[derive(Deserialize)]
struct PersistedSupertype {
    symbol: String,
    relation: SupertypeRelation,
    spelling: String,
    span: Span,
}

/// The persisted `anonymous_supertypes` entry of `scopes.facts_json` (LR2).
#[derive(Deserialize)]
struct PersistedAnonymousSupertype {
    class_start: u32,
    relation: SupertypeRelation,
    spelling: String,
    span: Span,
}

/// The only part of `scopes.facts_json` this module reads.
#[derive(Deserialize, Default)]
struct PersistedHierarchyFacts {
    #[serde(default)]
    supertypes: Vec<PersistedSupertype>,
    #[serde(default)]
    anonymous_supertypes: Vec<PersistedAnonymousSupertype>,
}

/// Every class-like's declared direct supertypes in one snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hierarchy {
    /// Canonical class-like ID -> sorted, deduplicated direct supertypes.
    direct: BTreeMap<String, Vec<Supertype>>,
    /// Anonymous class key (`file@start_byte`) -> its sorted, deduplicated
    /// direct supertypes (LR2). Anonymous classes are not symbols; they are
    /// kept only as possible subtypes of what they name.
    anonymous: BTreeMap<String, Vec<Supertype>>,
}

impl Hierarchy {
    /// Loads the hierarchy of the snapshot committed in `store`.
    pub fn load(store: &Store) -> Result<Hierarchy, Error> {
        let symbols = store.list_symbols()?;
        let uses = store.list_uses()?;
        let scopes = store.list_scopes()?;
        let bindings = store.list_bindings()?;
        Ok(Hierarchy::from_rows(&symbols, &uses, &scopes, &bindings))
    }

    /// Builds the hierarchy from already-loaded snapshot rows. The result
    /// does not depend on the order of any input slice.
    pub fn from_rows(
        symbols: &[SymbolRow],
        uses: &[UseRow],
        scopes: &[ScopeRow],
        bindings: &[BindingRow],
    ) -> Hierarchy {
        let class_like: HashMap<&str, &SymbolRow> = symbols
            .iter()
            .filter(|row| is_class_like(row.kind))
            .map(|row| (row.id.as_str(), row))
            .collect();
        let type_uses: HashMap<(&str, u32, u32), &UseRow> = uses
            .iter()
            .filter(|row| row.ref_kind == RefKind::Type)
            .map(|row| ((row.file.as_str(), row.start_byte, row.end_byte), row))
            .collect();
        let bound: HashMap<i64, &str> = bindings
            .iter()
            .map(|row| (row.use_id, row.target_id.as_str()))
            .collect();
        // Only scope facts are read (to name a supertype PHP's way); nothing
        // is bound here.
        let resolver = Resolver::scope_reader(symbols, uses, scopes);

        // A supertype is resolved exactly when its `type` use (same file and
        // span) is bound to an indexed class-like; otherwise it keeps PHP's
        // compile-time qualified name of the spelling, when trustworthy.
        let resolve_name = |file: &str, span: Span, spelling: &str| {
            let use_row = type_uses
                .get(&(file, span.start_byte(), span.end_byte()))
                .copied();
            let target = use_row
                .and_then(|row| row.use_id)
                .and_then(|use_id| bound.get(&use_id).copied())
                .and_then(|id| class_like.get(id).copied());
            match target {
                Some(row) => (Some(row.qualified_name.clone()), Some(row.id.clone())),
                None => {
                    let qname = use_row.and_then(|row| {
                        php_qualified_name(spelling, &resolver.scope_facts_for(row))
                    });
                    (qname, None)
                }
            }
        };

        let mut direct: BTreeMap<String, Vec<Supertype>> = BTreeMap::new();
        let mut anonymous: BTreeMap<String, Vec<Supertype>> = BTreeMap::new();
        for scope in scopes {
            let parsed = serde_json::from_str::<PersistedHierarchyFacts>(&scope.facts_json)
                .unwrap_or_default();
            for fact in parsed.supertypes {
                // A fact whose declaring symbol is not an indexed class-like
                // in the same file is inconsistent; never attribute it.
                if !class_like
                    .get(fact.symbol.as_str())
                    .is_some_and(|row| row.file == scope.file)
                {
                    continue;
                }
                let (qualified_name, resolved) =
                    resolve_name(scope.file.as_str(), fact.span, &fact.spelling);
                direct.entry(fact.symbol).or_default().push(Supertype {
                    relation: fact.relation,
                    spelling: fact.spelling,
                    qualified_name,
                    resolved,
                });
            }
            for fact in parsed.anonymous_supertypes {
                let (qualified_name, resolved) =
                    resolve_name(scope.file.as_str(), fact.span, &fact.spelling);
                anonymous
                    .entry(format!("{}@{}", scope.file, fact.class_start))
                    .or_default()
                    .push(Supertype {
                        relation: fact.relation,
                        spelling: fact.spelling,
                        qualified_name,
                        resolved,
                    });
            }
        }
        for list in direct.values_mut().chain(anonymous.values_mut()) {
            sort_dedup(list);
        }
        Hierarchy { direct, anonymous }
    }

    /// Declared direct supertypes of `class`, each resolved to the indexed
    /// class-like symbol its `type` use is bound to, otherwise the qualified
    /// name only. Sorted by relation (`extends` first) and qualified name,
    /// deduplicated per relation, deterministic. Empty for an unknown ID or a
    /// class-like with no clause.
    pub fn direct_supertypes(&self, class: &str) -> Vec<Supertype> {
        self.direct.get(class).cloned().unwrap_or_default()
    }

    /// Transitive closure of [`direct_supertypes`](Self::direct_supertypes)
    /// over resolved supertypes, cycle-safe, sorted like it.
    ///
    /// Each ancestor appears once, keyed by its resolved ID or else its
    /// qualified name; `class` itself is never listed, even when a cycle
    /// (`A extends B`, `B extends A`, which PHP rejects but the parser
    /// accepts) leads back to it. An unresolved ancestor is kept as a name
    /// and not traversed further.
    pub fn ancestors(&self, class: &str) -> Vec<Supertype> {
        self.closure(
            Some(class),
            self.direct.get(class).map_or(&[][..], Vec::as_slice),
        )
    }

    /// The anonymous classes of the snapshot (LR2), each as its key
    /// (`file@start_byte`, sorted) and its transitive supertypes, computed
    /// like [`ancestors`](Self::ancestors) from its header.
    pub fn anonymous_ancestors(&self) -> Vec<(String, Vec<Supertype>)> {
        self.anonymous
            .iter()
            .map(|(key, direct)| (key.clone(), self.closure(None, direct)))
            .collect()
    }

    /// Whether any declared supertype, of a named or an anonymous class, has
    /// no qualified name (LR2): such a link could name any class.
    pub fn has_unknown_link(&self) -> bool {
        self.direct
            .values()
            .chain(self.anonymous.values())
            .flatten()
            .any(|entry| entry.qualified_name.is_none())
    }

    /// The transitive closure starting from `direct`, the direct supertypes
    /// of `class` (`None` for an anonymous class), sorted, cycle-safe, and
    /// never listing `class` itself.
    fn closure(&self, class: Option<&str>, direct: &[Supertype]) -> Vec<Supertype> {
        let mut seen: BTreeSet<(u8, String)> = BTreeSet::new();
        let mut expanded: BTreeSet<String> = BTreeSet::new();
        if let Some(class) = class {
            let start = Supertype {
                relation: SupertypeRelation::Extends,
                spelling: String::new(),
                qualified_name: None,
                resolved: Some(class.to_string()),
            };
            seen.insert(start.identity());
            expanded.insert(class.to_string());
        }
        let mut result = Vec::new();
        let mut level: Vec<&Supertype> = direct.iter().collect();
        while !level.is_empty() {
            let mut next = Vec::new();
            for supertype in level {
                if !seen.insert(supertype.identity()) {
                    continue;
                }
                if let Some(id) = &supertype.resolved
                    && expanded.insert(id.clone())
                {
                    next.push(id.clone());
                }
                result.push(supertype.clone());
            }
            next.sort();
            level = next
                .iter()
                .flat_map(|id| self.direct.get(id).into_iter().flatten())
                .collect();
        }
        result.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        result
    }
}

/// Declared direct supertypes of `class` in the snapshot committed in
/// `store`. See [`Hierarchy::direct_supertypes`].
pub fn direct_supertypes(store: &Store, class: &str) -> Result<Vec<Supertype>, Error> {
    Ok(Hierarchy::load(store)?.direct_supertypes(class))
}

/// Transitive supertypes of `class` in the snapshot committed in `store`.
/// See [`Hierarchy::ancestors`].
pub fn ancestors(store: &Store, class: &str) -> Result<Vec<Supertype>, Error> {
    Ok(Hierarchy::load(store)?.ancestors(class))
}

/// Sorts `list` and removes entries naming the same supertype under the
/// same relation (`implements I, I` is a PHP error the parser accepts).
fn sort_dedup(list: &mut Vec<Supertype>) {
    list.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    let mut seen: BTreeSet<(SupertypeRelation, (u8, String))> = BTreeSet::new();
    list.retain(|entry| seen.insert((entry.relation, entry.identity())));
}

fn is_class_like(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
    )
}

#[cfg(test)]
mod tests {
    use crate::resolve::ScopeFacts;
    use crate::resolve::rules::php_qualified_name;
    use rivet_core::extract::ScopeImport;
    use rivet_core::{ImportKind, Span};

    fn facts(namespace: Option<&str>, imports: &[(&str, &str, ImportKind)]) -> ScopeFacts {
        ScopeFacts {
            imports: imports
                .iter()
                .map(|(alias, target, kind)| ScopeImport {
                    alias: alias.to_string(),
                    target_qualified: target.to_string(),
                    kind: *kind,
                    span: Span::new(0, 1).expect("span"),
                })
                .collect(),
            declares: Vec::new(),
            namespace: namespace.map(str::to_string),
            new_bindings: Vec::new(),
            call_args: Vec::new(),
            unanalysable: false,
            namespace_unattributed: false,
            class_constant_access: false,
            global_scope: false,
            call_sites: Vec::new(),
            goto_present: false,
        }
    }

    #[test]
    fn php_name_resolution_rules() {
        let app = facts(
            Some("App"),
            &[
                ("Lib", "Vendor\\Lib", ImportKind::Class),
                ("helper", "Vendor\\helper", ImportKind::Function),
            ],
        );
        let q = |spelling: &str, facts: &ScopeFacts| php_qualified_name(spelling, facts);
        assert_eq!(q("\\A\\B", &app).as_deref(), Some("A\\B"));
        assert_eq!(q("\\", &app), None);
        assert_eq!(q("Base", &app).as_deref(), Some("App\\Base"));
        assert_eq!(q("namespace\\Base", &app).as_deref(), Some("App\\Base"));
        // An alias names the first segment, case-insensitively.
        assert_eq!(q("lib", &app).as_deref(), Some("Vendor\\Lib"));
        assert_eq!(
            q("Lib\\Sub\\X", &app).as_deref(),
            Some("Vendor\\Lib\\Sub\\X")
        );
        // A function import never names a class.
        assert_eq!(q("helper", &app).as_deref(), Some("App\\helper"));
        // The global namespace prefixes nothing.
        assert_eq!(q("Base", &facts(None, &[])).as_deref(), Some("Base"));
        // Two imports claiming one alias: never guess between them.
        let conflict = facts(
            Some("App"),
            &[
                ("X", "One\\X", ImportKind::Class),
                ("X", "Two\\X", ImportKind::Class),
            ],
        );
        assert_eq!(q("X", &conflict), None);
        // An unattributed namespace has no trustworthy qualified name.
        let mut orphan = facts(Some("App"), &[]);
        orphan.namespace_unattributed = true;
        assert_eq!(q("Base", &orphan), None);
        assert_eq!(q("\\Lib\\Base", &orphan), None);
    }
}
