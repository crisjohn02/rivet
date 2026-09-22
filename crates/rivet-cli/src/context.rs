//! Direct context candidate collection and the fixed integer ranking (spec
//! §16.3; T26).
//!
//! [`collect_candidates`] gathers the depth-1 relationships a `rivet context`
//! query will render and orders them by the spec's ascending tuple
//! `(depth, reason_priority, resolution_priority, file_utf8_bytes, start_byte,
//! id)`. The target is always emitted first, and a symbol reachable through
//! several relationships is deduplicated by canonical ID retaining its best
//! tuple. Depth-two traversal, relationship flags, work caps, source
//! estimation, budget fitting, overlap suppression, and command wiring belong
//! to T27 through T30 and are deliberately absent here.
//!
//! Callees, type uses, and callers are selected through [`crate::references`],
//! the same shared pipeline that backs `rivet refs` and the `rivet symbol` call
//! lists; this module never re-implements use matching. Imports are the one
//! relationship that pipeline cannot select directly: a PHP `use` sits at file
//! scope, so its containing symbol is null and it is not contained by the
//! target. Import-kind uses contained by the target therefore yield nothing,
//! and taking every import in the file is explicitly forbidden. Instead the
//! derivation follows each use inside the target to its binding target and
//! keeps the targets that are also the binding target of a file-level import
//! use; that imported symbol is the `import` candidate.
//!
//! Name-only links (a reference whose use has no stored binding) still produce
//! a depth-1 candidate, such as a caller with `resolution: name_match`; they
//! only fail to open a further hop, which is T27's concern. This is the reading
//! of spec §16.3's "name-only links do not expand context by default" that
//! keeps the tuple's `resolution_priority: name_match = 2` reachable.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rivet_core::{ContextConfig, RefKind, Resolution};
use rivet_store::{Store, SymbolRow};

use crate::index;
use crate::references::{self, Mode, Selection};
use crate::transport::CliError;

/// Why one symbol is a context candidate (spec §16.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// The queried symbol itself (depth 0).
    Target,
    /// A type definition the target uses.
    Type,
    /// A symbol the target calls.
    Callee,
    /// A caller declared in a file matching the configured test globs.
    Test,
    /// A symbol that calls the target.
    Caller,
    /// A symbol the target consumes through a file-level import.
    Import,
    /// The target's containing declaration.
    Parent,
    /// A depth-two traversal candidate; never emitted by T26.
    SecondDegree,
}

impl Reason {
    /// The contract spelling emitted as a segment's `reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Target => "target",
            Reason::Type => "type",
            Reason::Callee => "callee",
            Reason::Test => "test",
            Reason::Caller => "caller",
            Reason::Import => "import",
            Reason::Parent => "parent",
            Reason::SecondDegree => "second_degree",
        }
    }

    /// The spec §16.3 `reason_priority` integer.
    pub fn priority(self) -> u8 {
        match self {
            Reason::Target => 0,
            Reason::Type => 1,
            Reason::Callee => 2,
            Reason::Test => 3,
            Reason::Caller => 4,
            Reason::Import => 5,
            Reason::Parent => 6,
            Reason::SecondDegree => 7,
        }
    }

    /// The traversal depth this reason belongs to.
    pub fn depth(self) -> u8 {
        match self {
            Reason::Target => 0,
            Reason::SecondDegree => 2,
            _ => 1,
        }
    }
}

/// The spec §16.3 `resolution_priority` integer.
///
/// Written out here rather than derived so the ordering is auditable against
/// the spec, even though [`Resolution`]'s own ordering happens to agree.
fn resolution_priority(resolution: Resolution) -> u8 {
    match resolution {
        Resolution::Exact => 0,
        Resolution::Scoped => 1,
        Resolution::NameMatch => 2,
    }
}

/// One collected context candidate before reachability, caps, estimation, and
/// budget fitting (T27–T29).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The full declaration row the candidate names.
    pub symbol: SymbolRow,
    /// Why the symbol was selected.
    pub reason: Reason,
    /// The evidence tier of the link/path that selected it. The target is
    /// always `exact`.
    pub resolution: Resolution,
}

impl Candidate {
    /// The candidate's canonical symbol ID.
    pub fn id(&self) -> &str {
        &self.symbol.id
    }

    /// The spec §16.3 ascending tuple, excluding the `id` tie-breaker that the
    /// caller applies last.
    fn rank_key(&self) -> (u8, u8, u8, &[u8], u32) {
        (
            self.reason.depth(),
            self.reason.priority(),
            resolution_priority(self.resolution),
            self.symbol.file.as_bytes(),
            self.symbol.start_byte,
        )
    }
}

/// Collects the depth-1 candidates for `target` and returns them in rank order.
///
/// `config` supplies the configured test globs for the `test` reason. Every
/// other context setting (`include_tests`, `max_depth`, `collapse`) is consumed
/// by later tasks, so only `test_globs` is read here.
pub fn collect_candidates(
    store: &Store,
    target: &SymbolRow,
    config: &ContextConfig,
) -> Result<Vec<Candidate>, CliError> {
    let symbols = store.list_symbols().map_err(index::store_error)?;
    let by_id: HashMap<&str, &SymbolRow> =
        symbols.iter().map(|row| (row.id.as_str(), row)).collect();

    // The raw rows are needed for the import derivation, which reads bindings
    // directly; the callee/type/caller lists still go through the shared
    // reference pipeline so there is exactly one matcher.
    let uses = references::all_uses(store)?;
    let bindings = references::bindings_by_use_id(store)?;

    let call_kinds: HashSet<RefKind> = HashSet::from([RefKind::Call]);
    let type_kinds: HashSet<RefKind> = HashSet::from([RefKind::Type]);
    let mut candidates = Vec::new();

    // The target is always first (reason_priority 0, depth 0).
    candidates.push(Candidate {
        symbol: target.clone(),
        reason: Reason::Target,
        resolution: Resolution::Exact,
    });

    // Callees: contained call matches followed to their binding target. An
    // unresolved call has no declaration target, so it yields no candidate.
    for reference in references::collect_matches(
        &uses,
        &bindings,
        target,
        Selection::Contained,
        Some(&call_kinds),
        Resolution::NameMatch,
    ) {
        push_bound(
            &mut candidates,
            &by_id,
            reference.resolved_target.as_deref(),
            Reason::Callee,
            reference.resolution,
        );
    }

    // Types: contained type uses followed to their binding target.
    for reference in references::collect_matches(
        &uses,
        &bindings,
        target,
        Selection::Contained,
        Some(&type_kinds),
        Resolution::NameMatch,
    ) {
        push_bound(
            &mut candidates,
            &by_id,
            reference.resolved_target.as_deref(),
            Reason::Type,
            reference.resolution,
        );
    }

    // Callers: reference-mode call matches followed to their containing
    // symbol. A top-level call has no containing symbol and yields no caller
    // candidate rather than a guessed container.
    let test_globs = TestGlobs::new(&config.test_globs);
    for reference in references::collect_matches(
        &uses,
        &bindings,
        target,
        Selection::Query(Mode::References),
        Some(&call_kinds),
        Resolution::NameMatch,
    ) {
        let Some(container_id) = reference.containing_symbol.as_deref() else {
            continue;
        };
        let Some(container) = by_id.get(container_id) else {
            continue;
        };
        let reason = if test_globs.matches(&container.file) {
            Reason::Test
        } else {
            Reason::Caller
        };
        candidates.push(Candidate {
            symbol: (*container).clone(),
            reason,
            resolution: reference.resolution,
        });
    }

    collect_imports(&uses, &bindings, &by_id, target, &mut candidates);

    // Parent: the target's containing declaration, when it has one. A
    // top-level declaration has `parent_id: None` and is simply absent.
    if let Some(parent_id) = target.parent_id.as_deref()
        && let Some(parent) = by_id.get(parent_id)
    {
        candidates.push(Candidate {
            symbol: (*parent).clone(),
            reason: Reason::Parent,
            resolution: Resolution::Exact,
        });
    }

    Ok(rank(candidates))
}

/// Adds a candidate for `id` when it names a symbol in the snapshot.
fn push_bound(
    candidates: &mut Vec<Candidate>,
    by_id: &HashMap<&str, &SymbolRow>,
    id: Option<&str>,
    reason: Reason,
    resolution: Resolution,
) {
    if let Some(id) = id
        && let Some(symbol) = by_id.get(id)
    {
        candidates.push(Candidate {
            symbol: (*symbol).clone(),
            reason,
            resolution,
        });
    }
}

/// Derives the `import` candidates for `target` (spec §16.3).
///
/// A file-level import use binds to the imported declaration. The target's own
/// uses are followed to their binding targets; a target that is the binding
/// target of one of the target file's file-level imports is consumed through
/// that import and becomes an `import` candidate. The candidate's resolution is
/// the import binding's resolution (imports are always `exact`).
///
/// This deliberately does not treat a member reached through an imported
/// receiver (for example `$svc->launch()` where `$svc` is typed by an import)
/// as an import candidate: that use's binding target is the member, not the
/// imported declaration, and the type use that carries the import is a `type`
/// candidate in its own right.
fn collect_imports(
    uses: &[rivet_store::UseRow],
    bindings: &HashMap<i64, rivet_store::BindingRow>,
    by_id: &HashMap<&str, &SymbolRow>,
    target: &SymbolRow,
    candidates: &mut Vec<Candidate>,
) {
    // The imported declarations this file's file-scope imports bind to. A PHP
    // `use` has no containing symbol, so it can never be contained by the
    // target; selecting import-kind uses contained by the target is empty.
    let mut import_targets: HashMap<String, Resolution> = HashMap::new();
    for row in uses {
        if row.file != target.file
            || row.ref_kind != RefKind::Import
            || row.containing_symbol.is_some()
        {
            continue;
        }
        let Some(use_id) = row.use_id else {
            continue;
        };
        if let Some(binding) = bindings.get(&use_id) {
            import_targets
                .entry(binding.target_id.clone())
                .or_insert(binding.resolution);
        }
    }
    if import_targets.is_empty() {
        return;
    }

    for row in uses {
        if row.containing_symbol.as_deref() != Some(target.id.as_str()) {
            continue;
        }
        let Some(use_id) = row.use_id else {
            continue;
        };
        let Some(binding) = bindings.get(&use_id) else {
            continue;
        };
        let Some(resolution) = import_targets.get(binding.target_id.as_str()) else {
            continue;
        };
        if let Some(symbol) = by_id.get(binding.target_id.as_str()) {
            candidates.push(Candidate {
                symbol: (*symbol).clone(),
                reason: Reason::Import,
                resolution: *resolution,
            });
        }
    }
}

/// Sorts by the spec §16.3 tuple and deduplicates by ID, keeping the best
/// tuple's candidate.
///
/// The sort is explicit and total, so no `HashMap` iteration order reaches the
/// result. Because the tuple orders every entry of one ID relative to the
/// others, the first occurrence of an ID in the sorted list is its best tuple;
/// deduplication keeps that one. A symbol reachable as both a callee and a
/// caller therefore keeps `callee`.
fn rank(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by(|a, b| {
        a.rank_key()
            .cmp(&b.rank_key())
            .then_with(|| a.symbol.id.as_bytes().cmp(b.symbol.id.as_bytes()))
    });

    let mut seen: HashSet<String> = HashSet::new();
    let mut ranked = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if seen.insert(candidate.symbol.id.clone()) {
            ranked.push(candidate);
        }
    }
    ranked
}

/// Matches a repository-relative path against the configured test globs.
///
/// The globs are gitignore-style path patterns (`tests/**`, `**/*.test.ts`),
/// so the same pinned `ignore` matcher the traversal uses is reused rather
/// than a second pattern language. A path is a test file when any glob matches
/// it or one of its parent directories.
struct TestGlobs {
    matcher: Option<Gitignore>,
}

impl TestGlobs {
    fn new(globs: &[String]) -> TestGlobs {
        let mut builder = GitignoreBuilder::new("");
        let mut any = false;
        for glob in globs {
            if builder.add_line(None, glob).is_ok() {
                any = true;
            }
        }
        TestGlobs {
            matcher: any.then(|| builder.build().ok()).flatten(),
        }
    }

    fn matches(&self, file: &str) -> bool {
        self.matcher.as_ref().is_some_and(|matcher| {
            matcher
                .matched_path_or_any_parents(Path::new(file), false)
                .is_ignore()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Reason, TestGlobs, resolution_priority};
    use rivet_core::Resolution;

    #[test]
    fn reason_priorities_are_the_spec_integers() {
        let ordered = [
            (Reason::Target, 0),
            (Reason::Type, 1),
            (Reason::Callee, 2),
            (Reason::Test, 3),
            (Reason::Caller, 4),
            (Reason::Import, 5),
            (Reason::Parent, 6),
            (Reason::SecondDegree, 7),
        ];
        for (reason, priority) in ordered {
            assert_eq!(reason.priority(), priority, "{reason:?}");
            let depth = match reason {
                Reason::Target => 0,
                Reason::SecondDegree => 2,
                _ => 1,
            };
            assert_eq!(reason.depth(), depth, "{reason:?}");
        }
    }

    #[test]
    fn resolution_priorities_are_the_spec_integers() {
        assert_eq!(resolution_priority(Resolution::Exact), 0);
        assert_eq!(resolution_priority(Resolution::Scoped), 1);
        assert_eq!(resolution_priority(Resolution::NameMatch), 2);
    }

    #[test]
    fn test_globs_match_repository_relative_paths() {
        let globs = TestGlobs::new(&["tests/**".to_string(), "**/*.test.ts".to_string()]);
        assert!(globs.matches("tests/SurveyTest.php"));
        assert!(globs.matches("tests/Unit/DeepTest.php"));
        assert!(globs.matches("src/survey.test.ts"));
        assert!(!globs.matches("src/Survey.php"));
        assert!(!globs.matches("Tests/Survey.php"));

        let empty = TestGlobs::new(&[]);
        assert!(!empty.matches("tests/SurveyTest.php"));
    }
}
