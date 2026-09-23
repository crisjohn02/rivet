//! Context candidate collection and the fixed integer ranking (spec §16.3;
//! T26), plus depth-two traversal, relationship flags, and the fixed work caps
//! (spec §16.3, §16.4 item 4; T27).
//!
//! [`collect_candidates`] gathers the depth-1 relationships a `rivet context`
//! query will render and orders them by the spec's ascending tuple
//! `(depth, reason_priority, resolution_priority, file_utf8_bytes, start_byte,
//! id)`. The target is always emitted first, and a symbol reachable through
//! several relationships is deduplicated by canonical ID retaining its best
//! tuple. [`collect_ranked`] is the T27 traversal: it applies
//! [`ContextOptions`] (depth, relationship flags, and the candidate and
//! examined-use caps) and reports `candidate_limit_reached`. With depth 1,
//! every relationship included, and caps that are not reached it returns
//! exactly [`collect_candidates`]'s list, which stays as the T26 reference.
//! Source estimation, budget fitting, and overlap suppression live in
//! [`crate::budget`] (T28/T29); the `rivet context` command wiring lives in
//! [`crate::context_cmd`] (T30).
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
//! only fail to open a further hop (see [`collect_ranked`]). This is the reading
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

/// One collected context candidate before source estimation and budget
/// fitting (T28–T29).
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
    let evidence = references::Evidence::load(store, &uses, &bindings)?;

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
        &evidence,
        target,
        Selection::Contained,
        Some(&call_kinds),
        Resolution::NameMatch,
    )
    .matches
    {
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
        &evidence,
        target,
        Selection::Contained,
        Some(&type_kinds),
        Resolution::NameMatch,
    )
    .matches
    {
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
        &evidence,
        target,
        Selection::Query(Mode::References),
        Some(&call_kinds),
        Resolution::NameMatch,
    )
    .matches
    {
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

/// Hard cap on unique context candidates, the target included (spec §16.4
/// item 4).
pub const MAX_CANDIDATES: usize = 1_000;

/// Hard cap on examined uses across the whole traversal (spec §16.4 item 4).
/// See [`ContextOptions::max_examined_uses`] for what counts as one.
pub const MAX_EXAMINED_USES: usize = 10_000;

/// The traversal options for [`collect_ranked`] (spec §16.3–§16.5).
///
/// Relationship flags are applied before traversal: a disabled relationship's
/// uses are never selected, so they can neither yield a candidate nor open a
/// further hop, and they do not count toward the examined-use cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextOptions {
    /// Traversal depth, 1 or 2 (`--depth`).
    pub depth: u8,
    /// When false, every caller use located in a file matching the configured
    /// test globs is dropped at both depths, so no candidate is reached
    /// through a caller edge from a test file.
    pub include_tests: bool,
    /// When false, reference-mode caller uses are never selected, at either
    /// depth.
    pub include_callers: bool,
    /// When false, contained `call` uses are never selected, at either depth.
    /// Because the whole use is dropped, a function consumed through an
    /// import *by a call* does not reappear as an `import` candidate either.
    pub include_callees: bool,
    /// Maximum unique candidates, the target included. Must be at least 1.
    pub max_candidates: usize,
    /// Maximum examined uses across the whole traversal.
    ///
    /// An *examined use* is one distinct use row, keyed by `(file,
    /// start_byte, end_byte, ref_kind)`, that an enabled relationship
    /// selection returns for one frontier symbol `S` (the target, then each
    /// expandable depth-1 candidate):
    ///
    /// - a use whose `containing_symbol` is `S` (every `ref_kind`, except
    ///   `call` when callees are excluded), which can yield a `type`,
    ///   `callee`, or `import` link; and
    /// - when callers are included, a reference-mode `call` use of `S` (bound
    ///   to `S`, or unresolved with `S`'s lookup name), excluding uses in
    ///   test-glob files when tests are excluded, which can yield a `caller`
    ///   or `test` link.
    ///
    /// A use selected by both directions for the same `S` counts once. The
    /// same use examined for two different frontier symbols counts twice,
    /// because it is examined twice. An examined use that yields no candidate
    /// (unresolved, a duplicate, a top-level caller, a name-only link at
    /// depth two) still counts. File-level import rows consulted to decide
    /// whether a contained use is consumed through an import, and the parent
    /// link (which reads no use), are lookups and do not count.
    pub max_examined_uses: usize,
}

impl ContextOptions {
    /// The defaults drawn from configuration: depth from `max_depth`, tests
    /// from `include_tests`, callers and callees included, and the spec caps.
    pub fn from_config(config: &ContextConfig) -> ContextOptions {
        ContextOptions {
            depth: config.max_depth,
            include_tests: config.include_tests,
            include_callers: true,
            include_callees: true,
            max_candidates: MAX_CANDIDATES,
            max_examined_uses: MAX_EXAMINED_USES,
        }
    }
}

/// The ranked candidates and the cap metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    /// Candidates in spec §16.3 rank order, the target first.
    pub candidates: Vec<Candidate>,
    /// True only when a cap stopped traversal with work remaining (the
    /// contract's `candidate_limit_reached`).
    pub candidate_limit_reached: bool,
}

/// Collects and ranks candidates to `options.depth` hops under the fixed caps.
///
/// Traversal order is fully determined: the target's examined uses are
/// visited in `(file bytes, start_byte, end_byte, ref_kind)` order, then the
/// target's parent; then, at depth 2, each expandable depth-1 candidate in
/// tuple order, each with its uses in the same order. Before a use is
/// examined the examined-use cap is checked, and before a new unique ID is
/// admitted the candidate cap is checked; hitting either stops the whole
/// traversal and sets `candidate_limit_reached`. A candidate already present
/// only has its tuple improved, which never counts against the cap, so a
/// graph with exactly `max_candidates` unique candidates reports false.
///
/// Readings of spec §16.3 applied here:
///
/// - Name-only links: a `name_match` link yields a depth-1 candidate (T26),
///   but a `name_match` candidate never seeds depth 2, and a `name_match`
///   link found at depth 2 produces no candidate. Depth two *is* expansion
///   beyond the target's direct links, and "name-only links do not expand
///   context by default".
/// - Containment: the parent is a depth-1-only relationship. A parent
///   candidate does not seed depth 2 (so its other members, the target's
///   siblings, are never reached through it), and the parent of a depth-1
///   candidate is not added at depth 2.
/// - Depth-2 candidates carry reason `second_degree` and the weakest of the
///   two hops' resolutions; deduplication by ID keeps the best tuple, so a
///   depth-1 symbol or the target never reappears as `second_degree`.
pub fn collect_ranked(
    store: &Store,
    target: &SymbolRow,
    config: &ContextConfig,
    options: &ContextOptions,
) -> Result<Collection, CliError> {
    if !(1..=2).contains(&options.depth) {
        return Err(CliError::invalid_arguments(
            format!("invalid context depth {} (expected 1 or 2)", options.depth),
            "Pass `--depth 1` or `--depth 2`.",
        ));
    }
    if options.max_candidates == 0 {
        return Err(CliError::invalid_arguments(
            "the context candidate cap must admit at least the target",
            "Use a candidate cap of at least 1.",
        ));
    }

    let symbols = store.list_symbols().map_err(index::store_error)?;
    let uses = references::all_uses(store)?;
    let bindings = references::bindings_by_use_id(store)?;
    let evidence = references::Evidence::load(store, &uses, &bindings)?;
    let graph = UseGraph::new(&symbols, &uses, &bindings, &evidence, config, options);

    let mut traversal = Traversal {
        found: HashMap::new(),
        examined: 0,
        options,
    };
    let limit_reached = traversal.run(&graph, target).is_err();

    let mut candidates: Vec<Candidate> = traversal.found.into_values().collect();
    candidates.sort_by(|a, b| {
        a.rank_key()
            .cmp(&b.rank_key())
            .then_with(|| a.symbol.id.as_bytes().cmp(b.symbol.id.as_bytes()))
    });
    Ok(Collection {
        candidates,
        candidate_limit_reached: limit_reached,
    })
}

/// A cap stopped traversal with work remaining.
struct CapReached;

/// One link a single examined use yields for a frontier symbol.
struct Link<'a> {
    symbol: &'a SymbolRow,
    reason: Reason,
    resolution: Resolution,
}

/// Snapshot-wide lookups built once per collection, so each frontier symbol
/// only hands the shared matcher the rows that can concern it.
struct UseGraph<'a> {
    by_id: HashMap<&'a str, &'a SymbolRow>,
    uses: &'a [rivet_store::UseRow],
    bindings: &'a HashMap<i64, rivet_store::BindingRow>,
    /// Reference-mode exclusion evidence (LR2).
    evidence: &'a references::Evidence,
    /// Use indices by `containing_symbol`, ascending.
    contained: HashMap<&'a str, Vec<usize>>,
    /// Call-use indices by `lookup_name`, ascending.
    calls_by_name: HashMap<&'a str, Vec<usize>>,
    /// Call-use indices by binding target, ascending.
    calls_by_target: HashMap<&'a str, Vec<usize>>,
    /// Per file, the declarations its file-level imports bind to, with the
    /// first import binding's resolution in use order (as T26).
    import_targets: HashMap<&'a str, HashMap<&'a str, Resolution>>,
    contained_kinds: HashSet<RefKind>,
    include_callers: bool,
    exclude_test_files: bool,
    test_globs: TestGlobs,
}

impl<'a> UseGraph<'a> {
    fn new(
        symbols: &'a [SymbolRow],
        uses: &'a [rivet_store::UseRow],
        bindings: &'a HashMap<i64, rivet_store::BindingRow>,
        evidence: &'a references::Evidence,
        config: &ContextConfig,
        options: &ContextOptions,
    ) -> UseGraph<'a> {
        let by_id = symbols.iter().map(|row| (row.id.as_str(), row)).collect();
        let mut contained: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut calls_by_name: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut calls_by_target: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut import_targets: HashMap<&str, HashMap<&str, Resolution>> = HashMap::new();
        for (index, row) in uses.iter().enumerate() {
            let binding = row.use_id.and_then(|use_id| bindings.get(&use_id));
            if let Some(container) = row.containing_symbol.as_deref() {
                contained.entry(container).or_default().push(index);
            }
            if row.ref_kind == RefKind::Call {
                calls_by_name
                    .entry(row.lookup_name.as_str())
                    .or_default()
                    .push(index);
                if let Some(binding) = binding {
                    calls_by_target
                        .entry(binding.target_id.as_str())
                        .or_default()
                        .push(index);
                }
            }
            if row.ref_kind == RefKind::Import
                && row.containing_symbol.is_none()
                && let Some(binding) = binding
            {
                import_targets
                    .entry(row.file.as_str())
                    .or_default()
                    .entry(binding.target_id.as_str())
                    .or_insert(binding.resolution);
            }
        }

        let mut contained_kinds: HashSet<RefKind> = HashSet::from([
            RefKind::Call,
            RefKind::Type,
            RefKind::Import,
            RefKind::Assignment,
            RefKind::Read,
            RefKind::Write,
            RefKind::Unknown,
        ]);
        if !options.include_callees {
            contained_kinds.remove(&RefKind::Call);
        }

        UseGraph {
            by_id,
            uses,
            bindings,
            evidence,
            contained,
            calls_by_name,
            calls_by_target,
            import_targets,
            contained_kinds,
            include_callers: options.include_callers,
            exclude_test_files: !options.include_tests,
            test_globs: TestGlobs::new(&config.test_globs),
        }
    }

    /// The rows at `indices`, in their original deterministic order.
    fn rows(&self, indices: &[usize]) -> Vec<rivet_store::UseRow> {
        indices
            .iter()
            .map(|&index| self.uses[index].clone())
            .collect()
    }

    /// The examined uses of `symbol`, in `(file bytes, start_byte, end_byte,
    /// ref_kind)` order, each with the links it yields.
    ///
    /// Selection goes through [`references::collect_matches`], the one shared
    /// matcher; the indices only narrow its input to a superset of the rows it
    /// would keep, which leaves its per-row decisions unchanged.
    fn examined_uses(&self, symbol: &SymbolRow) -> Vec<Vec<Link<'a>>> {
        type Key = (String, u32, u32, &'static str);
        let mut by_use: HashMap<Key, (Vec<Link<'a>>, Vec<Link<'a>>)> = HashMap::new();

        let contained_rows = self.rows(
            self.contained
                .get(symbol.id.as_str())
                .map_or(&[][..], Vec::as_slice),
        );
        for reference in references::collect_matches(
            &contained_rows,
            self.bindings,
            self.evidence,
            symbol,
            Selection::Contained,
            Some(&self.contained_kinds),
            Resolution::NameMatch,
        )
        .matches
        {
            let mut links = Vec::new();
            if let Some(bound) = reference.resolved_target.as_deref()
                && let Some(&declaration) = self.by_id.get(bound)
            {
                match reference.ref_kind {
                    RefKind::Call => links.push(Link {
                        symbol: declaration,
                        reason: Reason::Callee,
                        resolution: reference.resolution,
                    }),
                    RefKind::Type => links.push(Link {
                        symbol: declaration,
                        reason: Reason::Type,
                        resolution: reference.resolution,
                    }),
                    _ => {}
                }
                if let Some(&resolution) = self
                    .import_targets
                    .get(symbol.file.as_str())
                    .and_then(|targets| targets.get(bound))
                {
                    links.push(Link {
                        symbol: declaration,
                        reason: Reason::Import,
                        resolution,
                    });
                }
            }
            let key = (
                reference.file,
                reference.start_byte,
                reference.end_byte,
                reference.ref_kind.as_str(),
            );
            by_use.entry(key).or_default().0.extend(links);
        }

        if self.include_callers {
            let mut indices: Vec<usize> = self
                .calls_by_name
                .get(symbol.lookup_name.as_str())
                .into_iter()
                .chain(self.calls_by_target.get(symbol.id.as_str()))
                .flatten()
                .copied()
                .collect();
            indices.sort_unstable();
            indices.dedup();
            let caller_rows = self.rows(&indices);
            let call_kinds: HashSet<RefKind> = HashSet::from([RefKind::Call]);
            for reference in references::collect_matches(
                &caller_rows,
                self.bindings,
                self.evidence,
                symbol,
                Selection::Query(Mode::References),
                Some(&call_kinds),
                Resolution::NameMatch,
            )
            .matches
            {
                let is_test_file = self.test_globs.matches(&reference.file);
                if self.exclude_test_files && is_test_file {
                    continue;
                }
                let mut links = Vec::new();
                if let Some(container_id) = reference.containing_symbol.as_deref()
                    && let Some(&container) = self.by_id.get(container_id)
                {
                    let reason = if self.test_globs.matches(&container.file) {
                        Reason::Test
                    } else {
                        Reason::Caller
                    };
                    links.push(Link {
                        symbol: container,
                        reason,
                        resolution: reference.resolution,
                    });
                }
                let key = (
                    reference.file,
                    reference.start_byte,
                    reference.end_byte,
                    reference.ref_kind.as_str(),
                );
                by_use.entry(key).or_default().1.extend(links);
            }
        }

        let mut keyed: Vec<(Key, Vec<Link<'a>>)> = by_use
            .into_iter()
            .map(|(key, (mut contained, callers))| {
                contained.extend(callers);
                (key, contained)
            })
            .collect();
        keyed.sort_by(|(a, _), (b, _)| {
            a.0.as_bytes()
                .cmp(b.0.as_bytes())
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
                .then_with(|| a.3.as_bytes().cmp(b.3.as_bytes()))
        });
        keyed.into_iter().map(|(_, links)| links).collect()
    }
}

/// The mutable traversal state: best candidate per ID and the work counters.
struct Traversal<'o> {
    found: HashMap<String, Candidate>,
    examined: usize,
    options: &'o ContextOptions,
}

impl Traversal<'_> {
    fn run(&mut self, graph: &UseGraph<'_>, target: &SymbolRow) -> Result<(), CapReached> {
        self.admit(target, Reason::Target, Resolution::Exact)?;

        // Depth 1: the target's examined uses, then its parent.
        for links in graph.examined_uses(target) {
            self.examine()?;
            for link in links {
                self.admit(link.symbol, link.reason, link.resolution)?;
            }
        }
        if let Some(parent_id) = target.parent_id.as_deref()
            && let Some(&parent) = graph.by_id.get(parent_id)
        {
            self.admit(parent, Reason::Parent, Resolution::Exact)?;
        }

        if self.options.depth < 2 {
            return Ok(());
        }

        // Depth 2: expandable depth-1 candidates in tuple order. Neither a
        // name-only candidate nor the parent (containment) seeds a hop.
        let mut frontier: Vec<Candidate> = self
            .found
            .values()
            .filter(|candidate| {
                candidate.reason.depth() == 1
                    && candidate.reason != Reason::Parent
                    && candidate.resolution != Resolution::NameMatch
            })
            .cloned()
            .collect();
        frontier.sort_by(|a, b| {
            a.rank_key()
                .cmp(&b.rank_key())
                .then_with(|| a.symbol.id.as_bytes().cmp(b.symbol.id.as_bytes()))
        });
        for seed in &frontier {
            for links in graph.examined_uses(&seed.symbol) {
                self.examine()?;
                for link in links {
                    if link.resolution == Resolution::NameMatch {
                        continue;
                    }
                    let resolution = weakest(seed.resolution, link.resolution);
                    self.admit(link.symbol, Reason::SecondDegree, resolution)?;
                }
            }
        }
        Ok(())
    }

    /// Counts one examined use, refusing when the cap is already spent.
    fn examine(&mut self) -> Result<(), CapReached> {
        if self.examined >= self.options.max_examined_uses {
            return Err(CapReached);
        }
        self.examined += 1;
        Ok(())
    }

    /// Records a link to `symbol`. An ID already present keeps the better
    /// tuple and never counts against the cap; a new ID beyond the cap stops
    /// traversal.
    fn admit(
        &mut self,
        symbol: &SymbolRow,
        reason: Reason,
        resolution: Resolution,
    ) -> Result<(), CapReached> {
        let candidate = Candidate {
            symbol: symbol.clone(),
            reason,
            resolution,
        };
        if let Some(existing) = self.found.get_mut(symbol.id.as_str()) {
            if candidate.rank_key() < existing.rank_key() {
                *existing = candidate;
            }
            return Ok(());
        }
        if self.found.len() >= self.options.max_candidates {
            return Err(CapReached);
        }
        self.found.insert(symbol.id.clone(), candidate);
        Ok(())
    }
}

/// The weaker of two link resolutions: a path is only as strong as its
/// weakest hop (spec §16.3).
fn weakest(first: Resolution, second: Resolution) -> Resolution {
    if resolution_priority(first) >= resolution_priority(second) {
        first
    } else {
        second
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
