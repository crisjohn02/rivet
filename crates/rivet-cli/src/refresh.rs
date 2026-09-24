//! Refresh and cached-read paths shared by `rivet index` and every query
//! command (spec §12.3, §12.4; ARCHITECTURE "Refresh and
//! invalidation" and "Concurrency and source consistency").
//!
//! [`refresh`] is the single code path that performs a refresh: it walks the
//! eligible files, compares the stored effective-config, extractor, and
//! resolver fingerprints, reads and hashes each enabled-language file (or
//! reuses stored hashes in [`RefreshMode::Metadata`]), reparses only changed
//! content (or *all* enabled content when the extractor fingerprint differs),
//! drops facts for deleted or newly excluded files, and publishes one atomic
//! snapshot. Files whose content hash and stored parse status are unchanged keep
//! their stored symbols, uses, and scopes untouched; only a changed mtime/size
//! is rewritten. Bindings are cleared and every persisted use re-resolved only
//! when content, membership, or a fingerprint changed, so a refresh that
//! changes nothing writes no row and resolves nothing (PF1). `--force` reuses
//! nothing stored: it rereads and reparses every eligible file, regenerates all
//! facts and bindings, and replaces every stored fact in the same transaction.
//!
//! Concurrency (T31; spec §12.4 steps 3 and 4). Each attempt takes the writer
//! lock (`BEGIN IMMEDIATE`, 5 second busy timeout) before it loads anything,
//! and walks, reads, parses, writes, and re-resolves inside that one
//! transaction, so competing refreshes publish in lock order and never from a
//! stale inventory. A lock not acquired in time is exit 3. Before commit a
//! second walk must observe the same eligible paths with the same size and
//! mtime; a difference rolls back and retries the whole refresh once, and a
//! second difference is `repository_changed` (exit 9). A query then reads
//! everything from one committed read transaction pinned to the digest it
//! reports (see [`acquire_snapshot`]).
//!
//! [`open_cached_store`] plus [`cached_report`] implement explicit
//! `--no-refresh` access: they open a compatible committed snapshot and report
//! `freshness: cached` without walking, reading, or parsing anything.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use rivet_core::{
    Config, Freshness, ParseStatus, SourceRead, discover_root, read_source, walk_eligible,
};
use rivet_languages::{EXTRACTOR_FINGERPRINT, language_for_path};
use rivet_parser::parse_file_with_limits;
use rivet_store::{
    FileRow, Fingerprint, INDEX_FORMAT_VERSION, InventoryInput, ScopeRow, StagePlan, Store,
    SymbolRow, UseRow, clamp_mtime_ns,
};

use crate::index::{
    DIAGNOSTIC_CAP, DiagnosticItem, Report, Skipped, config_error, root_error, store_error,
    walk_error,
};
use crate::transport::CliError;

/// The discovered root and loaded configuration shared by `index` and queries.
pub(crate) struct Context {
    pub(crate) root: rivet_core::RootInfo,
    pub(crate) config: Config,
}

/// Discovers the nearest root and loads its configuration.
///
/// This performs no writes; [`open_store`] creates the cache directory only
/// when the caller is ready to refresh.
pub(crate) fn open_context() -> Result<Context, CliError> {
    let cwd = std::env::current_dir().map_err(|error| {
        CliError::repository_unavailable(
            format!("cannot determine the working directory: {error}"),
            "Run rivet from inside a repository.",
        )
    })?;
    let root = discover_root(&cwd).map_err(root_error)?;
    let config = Config::load(&root.root).map_err(config_error)?;
    Ok(Context { root, config })
}

/// Acquires the snapshot a query command answers from: refreshes the working
/// tree with the requested (or configured) freshness, or, for
/// `--no-refresh`, opens the committed snapshot directly (keeping the AF5
/// compatibility check) and never walks.
///
/// Either way the returned store has one open read transaction, and the
/// returned report's `snapshot` is the digest of exactly the snapshot that
/// transaction reads, so the query's rows, source bytes, and reported digest
/// all come from one committed snapshot even while another process publishes.
///
/// Shared by `symbol`, `refs`, and `context`. The caller validates every
/// argument and the configured languages first.
pub(crate) fn acquire_snapshot(
    context: &Context,
    no_refresh: bool,
    requested_freshness: Option<Freshness>,
) -> Result<(Store, Report), CliError> {
    let (store, report) = if no_refresh {
        let store = open_cached_store(&context.root)?;
        let report = cached_report(&store, false)?;
        (store, report)
    } else {
        let effective_freshness = requested_freshness.unwrap_or(context.config.index.freshness);
        let mode = crate::index::refresh_mode(effective_freshness);
        let store = open_store(&context.root)?;
        let report = refresh_with_retry(
            &context.root.root,
            &context.config,
            &store,
            mode,
            false,
            PinSnapshot::Yes,
        )?
        .report;
        (store, report)
    };
    // The store now holds one committed read transaction whose snapshot digest
    // is `report.snapshot`. Every read the command makes from here, including
    // the stored source bytes it slices, joins that transaction (spec §12.4
    // step 4; ARCHITECTURE "Readers use a committed read transaction").
    debug_point!("query-snapshot-pinned");
    Ok((store, report))
}

/// Opens the cache store, creating `.rivet/` at a Git root when absent.
///
/// A query auto-creates `.rivet/` only at a Git root and never edits
/// `.gitignore` (spec §25). `discover_root` reports `has_rivet_dir = false`
/// only when the boundary was `.git`, so this creates the cache directory
/// exactly there.
pub(crate) fn open_store(root: &rivet_core::RootInfo) -> Result<Store, CliError> {
    let rivet_dir = root.root.join(".rivet");
    if !root.has_rivet_dir {
        create_rivet_dir(&rivet_dir)?;
    }
    Store::open(&rivet_dir).map_err(store_error)
}

/// Opens the index cache for `index --force`, allowing an incompatible
/// database to be dropped and recreated after destination validation.
pub(crate) fn open_store_rebuildable(root: &rivet_core::RootInfo) -> Result<Store, CliError> {
    let rivet_dir = root.root.join(".rivet");
    if !root.has_rivet_dir {
        create_rivet_dir(&rivet_dir)?;
    }
    Store::open_rebuildable(&rivet_dir).map_err(store_error)
}

/// Opens an existing committed cache for `--no-refresh` without creating,
/// walking, or refreshing anything.
///
/// A missing `.rivet/index.db` is `repository_unavailable` (exit 3) with a hint
/// to run `rivet index`; the caller must never fall back to an empty cache. An
/// incompatible database is likewise exit 3 because this path has no `--force`.
pub(crate) fn open_cached_store(root: &rivet_core::RootInfo) -> Result<Store, CliError> {
    let rivet_dir = root.root.join(".rivet");
    let db = rivet_dir.join("index.db");
    match std::fs::symlink_metadata(&db) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CliError::repository_unavailable(
                format!("no committed index at {}", db.display()),
                "Run `rivet index` to build the cache, then retry with `--no-refresh`.",
            ));
        }
        Err(error) => {
            return Err(CliError::repository_unavailable(
                format!("cannot inspect {}: {error}", db.display()),
                "Run `rivet index` to build the cache, then retry with `--no-refresh`.",
            ));
        }
    }
    let store = Store::open_cached(&rivet_dir).map_err(cached_store_error)?;
    // Pin one committed snapshot before the first fact is read, so the
    // compatibility check, the reconstructed report, and every query read agree
    // on one snapshot (spec §12.4 step 4).
    store.begin_snapshot_read().map_err(cached_store_error)?;
    check_cached_fingerprints(&store)?;
    Ok(store)
}

/// Refuses a cache whose facts or bindings were produced by different rules.
///
/// OUTPUT-CONTRACT "Flag applicability": `--no-refresh` "requires compatible
/// cache", and "Errors": `repository_unavailable` (exit 3) "includes
/// lock/I/O/incompatible-cache failures". [`Store::open`] already refuses a
/// different `index_format_version`. This also compares the stored
/// `extractor_fingerprint` (the rules that produced symbols, uses, and scopes)
/// and `resolver_fingerprint` (the rules that produced bindings) with this
/// build's values, because a cache from other rules would serve facts and
/// tiers this build would not produce (AF5). A missing value is refused too.
///
/// `effective_config_fingerprint` is deliberately not compared: a config edit
/// changes which files are eligible, like any other working-tree edit, and a
/// `cached` answer is explicitly allowed to be stale relative to the working
/// tree. The facts it serves were still produced by this build's rules.
fn check_cached_fingerprints(store: &Store) -> Result<(), CliError> {
    for (key, expected) in [
        ("extractor_fingerprint", EXTRACTOR_FINGERPRINT),
        ("resolver_fingerprint", RESOLVER_FINGERPRINT),
    ] {
        let stored = store.get_meta(key).map_err(cached_store_error)?;
        if stored.as_deref() != Some(expected) {
            let found = stored.as_deref().unwrap_or("none");
            return Err(CliError::repository_unavailable(
                format!(
                    "incompatible cache: stored {key} {found:?} differs from this build's {expected:?}"
                ),
                "Run `rivet index` to rebuild the cache with this build, then retry with `--no-refresh`.",
            ));
        }
    }
    Ok(())
}

/// Maps cached-open failures to exit 3 with a rebuild hint.
fn cached_store_error(error: rivet_store::Error) -> CliError {
    let hint = match &error {
        rivet_store::Error::IncompatibleIndexFormat { .. } => {
            "Run `rivet index` (or `rivet index --force`) to rebuild the cache."
        }
        rivet_store::Error::InvalidDestination { .. } => {
            "Check that .rivet/ is a real writable directory, not a symlink."
        }
        _ => "Run `rivet index` to rebuild the cache, then retry with `--no-refresh`.",
    };
    CliError::repository_unavailable(error.to_string(), hint)
}

/// Creates the cache directory, tolerating a concurrent creation.
fn create_rivet_dir(path: &Path) -> Result<(), CliError> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(CliError::repository_unavailable(
            format!("cannot create {}: {error}", path.display()),
            "Check that the repository root is writable.",
        )),
    }
}

/// How a refresh decides which bytes are trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshMode {
    /// Read and hash eligible content; reparse only changed content or when
    /// the extractor fingerprint changed (spec §12.4 step 2).
    Content,
    /// Trust stored `(mtime_ns, size)` for unchanged files and reuse their
    /// stored content hash instead of reading; otherwise read and hash exactly
    /// like [`RefreshMode::Content`]. This can miss a same-size edit whose
    /// modification time was restored (see [`refresh_inventory`]).
    Metadata,
}

impl RefreshMode {
    /// The freshness spelling this mode reports.
    pub fn freshness(self) -> Freshness {
        match self {
            RefreshMode::Content => Freshness::Content,
            RefreshMode::Metadata => Freshness::Metadata,
        }
    }
}

/// The resolver fingerprint recorded in `meta.resolver_fingerprint`.
///
/// It covers the *binding rules*: how stored uses are resolved to declarations
/// and which resolution tier each binding carries (imports, functions,
/// constants, member lookup, `new` and typed receivers, fallbacks). It is
/// separate from [`EXTRACTOR_FINGERPRINT`], which covers the persisted facts
/// those rules read. The snapshot digest folds it in, a mismatch on refresh
/// re-resolves every use without reparsing (T22), and `--no-refresh` refuses a
/// cache whose stored value differs (AF5).
///
/// Any change to resolution behavior, meaning a use that would bind
/// differently, bind at a different tier, or stop or start binding, must bump
/// this value, even when the extractor fingerprint changes in the same task.
/// Relying on an extractor bump to invalidate stale bindings only works by
/// coincidence and misses a rules-only change.
///
/// History, newest first:
///
/// - **php-rules-v4;ts-rules-v1** — T44: the first TypeScript rules. Only
///   TypeScript's rules changed: direct relative named, aliased, default, and
///   `import type` imports bind to the declaration the module exports
///   (following the module's own `export { x as a }` renames); a namespace
///   import's `ns.member` binds to the export `member`; and a use lexically
///   bound, with shadowing and value/type spaces accounted for, to one
///   module-local declaration in its own file binds to it; all `exact`.
///   Re-exports, `export *`, path aliases, packages, `require`, ambiguous or
///   unindexed modules, globals, and receivers stay unresolved, and no
///   TypeScript use gets a receiver class. The PHP rules are exactly
///   php-rules-v4's, so PHP bindings are unchanged.
/// - **php-rules-v4** — T43: resolution dispatches on each file's stored
///   language (`rivet_index::resolve`'s rule table). The PHP rules bind only
///   uses in PHP files and see only PHP declarations, so a TypeScript use is
///   never bound (and gets no receiver class) until TypeScript rules exist,
///   and a PHP use never binds, or is made ambiguous by, a TypeScript
///   declaration. PHP-only snapshots bind exactly as under v3.
/// - **php-rules-v3** — LR2: the resolver also records, for each member or
///   scoped use no rule bound, the receiver class a receiver rule determined
///   for it (`receiver_classes`), including a class that is not indexed.
///   No binding or tier changes; reference mode reads the new rows to exclude
///   same-name uses whose receiver class is unrelated to the target's.
/// - **php-rules-v2** — records the resolution changes of AF1 through AF4,
///   which had shipped under v1: per-namespace-block top-level scopes (AF1);
///   kind-aware lookup, ASCII-only case folding, and the global function
///   fallback suppressed under partial coverage (AF2); typed-parameter
///   receivers suppressed by any rebinding, reference, or unproven by-value
///   argument, and no binding for union, intersection, or DNF types (AF3); and
///   anonymous-class receivers binding nothing, explicit static-call and
///   class-constant classes binding as `type` uses, and `static::`/`parent::`
///   binding nothing (AF4).
/// - **php-rules-v1** — T19: the first real binding rules (imports and
///   functions), later extended by T20/T21 receivers under the same value.
const RESOLVER_FINGERPRINT: &str = "php-rules-v4;ts-rules-v1";

/// The `meta` key recording whether the committed bindings were resolved with
/// the global function fallback suppressed because an enabled PHP file was not
/// indexed (AF2). Part of that input comes from skipped non-UTF-8 paths, which
/// have no `files` row, so a refresh compares the stored value to decide
/// whether the stored bindings are still current (PF1). A cache that predates
/// the key re-resolves once and records it.
const UNINDEXED_PHP_META_KEY: &str = "resolver_unindexed_php";

/// The shared result of one refresh.
///
/// The embedded [`Report`] is the same data `rivet index` formats, so query
/// commands can attach the identical coverage/diagnostics/snapshot metadata.
#[derive(Debug)]
pub struct RefreshOutcome {
    /// Coverage, diagnostics, counts, and the committed snapshot digest.
    pub report: Report,
}

/// Refreshes the index inside one writer transaction and returns its report.
///
/// This is the only function that walks, parses, and publishes on the query
/// path. A failed parse does not abort the refresh: the file is recorded with
/// its failure status and no symbols, and unrelated results stay available
/// with partial coverage (ARCHITECTURE "Parse and coverage policy"). An I/O
/// failure aborts, publishing nothing. `force` reuses no stored fact: every
/// eligible file is reread and reparsed, and every stored fact is replaced in
/// the same transaction (spec §13; AF6).
///
/// The writer lock is taken first and the whole refresh runs inside it (see
/// [`refresh_inventory`]). A writer lock not acquired within the busy timeout
/// is `repository_unavailable` (exit 3) naming the lock; a race detected on
/// the refresh and again on its one retry is `repository_changed` (exit 9).
/// Neither ever falls back to the existing snapshot.
pub fn refresh(
    root: &Path,
    config: &Config,
    store: &mut Store,
    mode: RefreshMode,
    force: bool,
) -> Result<RefreshOutcome, CliError> {
    refresh_with_retry(root, config, store, mode, force, PinSnapshot::No)
}

/// How many times a refresh runs before a detected race becomes
/// `repository_changed`: the first attempt and one retry (spec §12.4 step 3:
/// "Retry the whole refresh once on a detected race").
const REFRESH_ATTEMPTS: u32 = 2;

/// Whether a refresh must leave the store in a read transaction pinned to the
/// snapshot it committed, for a query to answer from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinSnapshot {
    /// `rivet index`: report the committed snapshot and read nothing more.
    No,
    /// A query: begin a read transaction right after the commit and require
    /// that it observes the committed digest.
    Yes,
}

/// Runs [`refresh_inventory`], retrying the whole refresh once on a detected
/// race (spec §12.4 step 3).
///
/// With [`PinSnapshot::Yes`], a read transaction is begun after the commit.
/// Between the commit and that transaction's first read another process may
/// publish; its snapshot then differs from the report being returned, which
/// would pair this refresh's coverage and digest with another snapshot's rows.
/// The digest is deterministic over the indexed content and configuration, so a
/// different digest means another refresh saw different content: that is a
/// detected race too, and it spends the same single retry.
fn refresh_with_retry(
    root: &Path,
    config: &Config,
    store: &Store,
    mode: RefreshMode,
    force: bool,
    pin: PinSnapshot,
) -> Result<RefreshOutcome, CliError> {
    let started = Instant::now();
    for attempt in 1..=REFRESH_ATTEMPTS {
        let Some(mut outcome) = refresh_inventory(root, config, store, mode, force, attempt)?
        else {
            // A race was detected and this attempt rolled back.
            continue;
        };
        if pin == PinSnapshot::Yes {
            let pinned = store.begin_snapshot_read().map_err(store_error)?;
            if pinned.as_deref() != Some(outcome.report.snapshot.as_str()) {
                store.end_snapshot_read().map_err(store_error)?;
                debug_point!("query-snapshot-moved-{attempt}");
                continue;
            }
        }
        outcome.report.elapsed_ms = started.elapsed().as_millis() as u64;
        return Ok(outcome);
    }
    Err(CliError::repository_changed(
        "the repository changed while it was being indexed, and again during the one retry",
        "Retry once files under the repository stop changing.",
    ))
}

/// One refresh attempt, in the order of the ARCHITECTURE "Refresh and
/// invalidation" pseudocode: take the writer lock (`BEGIN IMMEDIATE`, busy
/// timeout 5 seconds); load the fingerprints and inventory; walk; read, hash,
/// and conditionally reparse each eligible file in path order; replace file
/// facts; drop deleted files; re-resolve every use when content, membership,
/// or a fingerprint changed (and otherwise keep the stored bindings); recheck
/// the observed file metadata and eligible path set; compute the digest and
/// commit.
///
/// Returns `Ok(None)` when the recheck detects a race: the transaction was
/// rolled back and nothing was published. Any error also drops the writer
/// transaction, rolling it back, so a failed refresh leaves the previous
/// complete snapshot intact (spec §12.3).
fn refresh_inventory(
    root: &Path,
    config: &Config,
    store: &Store,
    mode: RefreshMode,
    force: bool,
    // Names the debug pause points only; unused in a release build.
    #[cfg_attr(not(debug_assertions), allow(unused_variables))] attempt: u32,
) -> Result<Option<RefreshOutcome>, CliError> {
    let started = Instant::now();

    // Writer lock first: every read below, including the previous inventory
    // and the use-ID high-water mark, sees the state this transaction will
    // replace, and no competing refresh can publish in between (ARCHITECTURE
    // "Concurrency and source consistency": parsing inside the writer
    // transaction "prevents competing refreshes from publishing out of
    // order").
    debug_point!("refresh-before-lock-{attempt}");
    let mut txn = store
        .begin_write(crate::debug_hook::busy_timeout())
        .map_err(store_error)?;
    debug_point!("refresh-locked-{attempt}");

    let max_bytes = config.index.max_file_size_kb.saturating_mul(1024);
    let resource_limits = crate::debug_hook::resource_limits();

    // Fingerprint invalidation at the start of every refresh (spec §12.3;
    // ARCHITECTURE "Refresh and invalidation"). The effective-config comparison
    // matters most in metadata mode: a changed ignore/language/size rule must
    // re-read and re-evaluate eligibility rather than trust a stored skip.
    let effective_config = config.fingerprint();
    let stored_config = store
        .get_meta("effective_config_fingerprint")
        .map_err(store_error)?;
    let config_matches = stored_config.as_deref() == Some(effective_config.as_str());

    // A stored extractor fingerprint that differs from this build invalidates
    // every enabled-language file regardless of content hash (spec §12.3,
    // ARCHITECTURE "Refresh and invalidation").
    let stored_extractor = store
        .get_meta("extractor_fingerprint")
        .map_err(store_error)?;
    let fingerprint_matches = stored_extractor.as_deref() == Some(EXTRACTOR_FINGERPRINT);

    // Resolver invalidation (ARCHITECTURE "Refresh and invalidation": "if
    // content, membership, or resolver fingerprint changed: clear bindings;
    // re-resolve all persisted uses/scopes"). A stored resolver fingerprint
    // that differs from this build's is one of the triggers computed after the
    // walk (`resolve_needed`); when any trigger fires, every persisted use is
    // re-resolved over exactly the symbol/use/scope rows about to be published
    // and the whole `bindings` table is replaced (T22). When none fires, the
    // stored bindings were produced by these rules from these exact facts, so
    // they are kept and nothing is re-resolved (PF1). Explicit `--no-refresh`
    // does not refresh, so it refuses a cache whose stored resolver (or
    // extractor) fingerprint differs from this build's
    // (`check_cached_fingerprints`, AF5). This is proven by
    // `tests/refresh.rs::stale_bindings_are_re_resolved_without_reparsing`.
    let stored_resolver = store
        .get_meta("resolver_fingerprint")
        .map_err(store_error)?;
    let resolver_matches = stored_resolver.as_deref() == Some(RESOLVER_FINGERPRINT);
    let stored_unindexed_php = store
        .get_meta(UNINDEXED_PHP_META_KEY)
        .map_err(store_error)?;

    // Load the current inventory once, and its symbol/use/scope rows only when
    // they are needed (PF1): a refresh in which nothing changed neither
    // re-resolves nor rewrites any fact, so it never reads them. Reused files
    // keep their stored source bytes and fact rows.
    //
    // `--force` (AF6) loads nothing: it is the recovery path for a cache whose
    // stored facts are suspect although no fingerprint changed, so it must not
    // trust a single stored row. With no stored inventory, neither the
    // metadata-mode nor the content-hash reuse test below can match, so every
    // eligible file is read from disk afresh and reparsed, every symbol, use,
    // and scope is regenerated, and use IDs restart at 1 exactly as for a
    // first index. The store's `force` path then deletes every stored fact
    // before writing these rows, in this same writer transaction.
    let mut stored_by_path: HashMap<String, FileRow> = if force {
        HashMap::new()
    } else {
        store
            .list_files()
            .map_err(store_error)?
            .into_iter()
            .map(|file| (file.path.clone(), file))
            .collect()
    };
    // The content-deciding columns of every stored row, kept for the
    // change test after the walk (`stored_by_path` is consumed below).
    let stored_state: HashMap<String, StoredState> = stored_by_path
        .iter()
        .map(|(path, file)| (path.clone(), StoredState::of(file)))
        .collect();

    // Walk regular eligible files with local ignore rules. The walked entries
    // carry the metadata observed for each file; the recheck compares it.
    let walk = walk_eligible(root, config).map_err(walk_error)?;

    let mut files = Vec::with_capacity(walk.files.len());
    // Each file's facts in walk order: fresh from a parse, or the stored rows,
    // which are loaded after the walk only if bindings must be re-resolved.
    let mut pending: Vec<PendingFacts> = Vec::new();
    let mut skipped = Skipped::default();
    let mut diagnostics = Vec::new();
    let mut files_indexed = 0_u64;
    let mut reparsed: Vec<String> = Vec::new();

    for entry in &walk.files {
        let language = language_for_path(&entry.rel_path).filter(|id| {
            config
                .languages
                .enabled
                .iter()
                .any(|name| name.as_str() == id.name())
        });
        let Some(id) = language else {
            skipped.unsupported += 1;
            files.push(FileRow {
                path: entry.rel_path.clone(),
                language: None,
                mtime_ns: clamp_mtime_ns(entry.mtime_ns),
                size: entry.size,
                content_hash: None,
                source: None,
                parse_status: ParseStatus::Unsupported,
            });
            continue;
        };

        let language_name = id.name().to_string();

        // An enabled language with no extraction adapter (none since T43
        // gave TypeScript one; `LanguageId::has_extractor` is the one switch
        // that keeps a future language unindexed) yields no facts even when it
        // parses, so the file must not count as indexed. OUTPUT-CONTRACT "Common index metadata": "`complete` is
        // true only when all skip counts are zero", and ARCHITECTURE "Parse and
        // coverage policy": "Unsupported language, binary, oversize, encoding,
        // and deterministic parser-resource skips are counted separately."
        // The language is unsupported by this build's extraction, so the file
        // counts as `unsupported`; it is not a `parse_error`, because a valid
        // file would be misreported as broken. The file is not read, so a size
        // or binary skip is not distinguished, exactly as for any other
        // unsupported file. Unlike an ordinary unsupported file (a README), the
        // user enabled this language, so a diagnostic names the reason.
        if !id.has_extractor() {
            stored_by_path.remove(&entry.rel_path);
            skipped.unsupported += 1;
            diagnostics.push(no_extractor_diagnostic(&entry.rel_path, &language_name));
            files.push(FileRow {
                path: entry.rel_path.clone(),
                language: Some(language_name),
                mtime_ns: clamp_mtime_ns(entry.mtime_ns),
                size: entry.size,
                content_hash: None,
                source: None,
                parse_status: ParseStatus::Unsupported,
            });
            continue;
        }

        let stored = stored_by_path.remove(&entry.rel_path);

        // Metadata mode trusts a stored row whose size and mtime match the
        // walked entry and reuses its content hash without reading. It is only
        // safe when the effective config and extractor fingerprints are
        // current: a changed rule (exclude/languages/max size) must re-evaluate
        // eligibility, and a changed extractor must reparse.
        //
        // NOTE: trusting size+mtime can miss a same-size edit whose mtime was
        // restored; content mode reads and hashes and does catch it. See
        // `tests/freshness_modes.rs` metadata-mode test.
        let metadata_reuse = mode == RefreshMode::Metadata
            && config_matches
            && fingerprint_matches
            && stored.as_ref().is_some_and(|file| {
                file.mtime_ns == clamp_mtime_ns(entry.mtime_ns) && file.size == entry.size
            });
        if metadata_reuse {
            let file = stored.expect("metadata reuse requires a stored row");
            if file.parse_status == ParseStatus::Ok {
                pending.push(PendingFacts::Stored(entry.rel_path.clone()));
            }
            count_status(file.parse_status, &mut files_indexed, &mut skipped);
            if let Some(item) = reused_status_diagnostic(&entry.rel_path, file.parse_status) {
                diagnostics.push(item);
            }
            files.push(FileRow {
                path: entry.rel_path.clone(),
                language: Some(language_name),
                mtime_ns: clamp_mtime_ns(entry.mtime_ns),
                size: entry.size,
                content_hash: file.content_hash,
                source: file.source,
                parse_status: file.parse_status,
            });
            continue;
        }

        match read_source(root, entry, max_bytes) {
            SourceRead::Ok { bytes, hash } => {
                // Do not reparse when the fingerprint is current, the hash is
                // unchanged, and the file previously parsed cleanly.
                let reuse = fingerprint_matches
                    && stored.as_ref().is_some_and(|file| {
                        file.content_hash.as_deref() == Some(hash.as_str())
                            && file.parse_status == ParseStatus::Ok
                    });

                let facts = if reuse {
                    let source = stored
                        .as_ref()
                        .and_then(|file| file.source.clone())
                        .or(Some(bytes));
                    ParsedFileFacts {
                        parse_status: ParseStatus::Ok,
                        source,
                        facts: PendingFacts::Stored(entry.rel_path.clone()),
                        diagnostic: None,
                    }
                } else {
                    reparsed.push(entry.rel_path.clone());
                    // Deterministic count bounds (spec §27), never time.
                    let extracted = parse_file_with_limits(id, &bytes, resource_limits);
                    match extracted.diagnostics.first() {
                        Some(diagnostic) => {
                            let status = match diagnostic.code.as_str() {
                                "resource_limit" => ParseStatus::ResourceLimit,
                                _ => ParseStatus::ParseError,
                            };
                            ParsedFileFacts {
                                parse_status: status,
                                source: None,
                                facts: PendingFacts::Fresh(FileFacts::default()),
                                diagnostic: Some(DiagnosticItem {
                                    file: entry.rel_path.clone(),
                                    code: static_diagnostic_code(&diagnostic.code),
                                    detail: diagnostic.detail.clone(),
                                }),
                            }
                        }
                        None => {
                            let file_facts = build_facts(id, &entry.rel_path, &bytes, &extracted);
                            ParsedFileFacts {
                                parse_status: ParseStatus::Ok,
                                source: Some(bytes),
                                facts: PendingFacts::Fresh(file_facts),
                                diagnostic: None,
                            }
                        }
                    }
                };

                count_status(facts.parse_status, &mut files_indexed, &mut skipped);
                if let Some(item) = facts.diagnostic {
                    diagnostics.push(item);
                }
                pending.push(facts.facts);
                files.push(FileRow {
                    path: entry.rel_path.clone(),
                    language: Some(language_name),
                    mtime_ns: clamp_mtime_ns(entry.mtime_ns),
                    size: entry.size,
                    content_hash: Some(hash),
                    source: facts.source,
                    parse_status: facts.parse_status,
                });
            }
            SourceRead::Skipped { reason } => {
                let status = classify_skip(reason, &mut skipped);
                if let Some((code, detail)) = skip_diagnostic(reason) {
                    diagnostics.push(DiagnosticItem {
                        file: entry.rel_path.clone(),
                        code,
                        detail: detail.to_string(),
                    });
                }
                files.push(FileRow {
                    path: entry.rel_path.clone(),
                    language: Some(language_name),
                    mtime_ns: clamp_mtime_ns(entry.mtime_ns),
                    size: entry.size,
                    content_hash: None,
                    source: None,
                    parse_status: status,
                });
            }
            // A file that vanished between the walk and the read is a change
            // to the eligible path set: a detected race, not an I/O failure
            // (spec §12.4 step 3).
            SourceRead::Failed(error) if error.kind() == std::io::ErrorKind::NotFound => {
                drop(txn);
                debug_point!("refresh-raced-{attempt}");
                return Ok(None);
            }
            // An I/O failure aborts the refresh; it never silently preserves old
            // facts (spec §27).
            SourceRead::Failed(error) => {
                return Err(CliError::repository_unavailable(
                    format!("cannot read {}: {error}", entry.rel_path),
                    "Fix the file permissions or remove the unreadable path, then retry.",
                ));
            }
        }
    }

    // Non-UTF-8 paths are outside the scan domain but must be reported and force
    // `complete: false` (OUTPUT-CONTRACT "Common index metadata": "Non-UTF-8
    // paths are excluded with a diagnostic using escaped bytes and force
    // `complete: false`"). The diagnostic `file` is the repository-relative
    // path with the invalid bytes escaped, like every other diagnostic path
    // (AF5, audit finding 17).
    for path in &walk.skipped {
        diagnostics.push(DiagnosticItem {
            file: path.rel_escaped.clone(),
            code: "non_utf8_path",
            detail: "path is not valid UTF-8".to_string(),
        });
    }

    let skipped_total = skipped.total();
    let files_seen = files_indexed + skipped_total;
    let complete = skipped_total == 0 && walk.skipped.is_empty();

    diagnostics.sort_by(|a, b| {
        a.file
            .as_bytes()
            .cmp(b.file.as_bytes())
            .then_with(|| a.code.as_bytes().cmp(b.code.as_bytes()))
            .then_with(|| a.detail.as_bytes().cmp(b.detail.as_bytes()))
    });
    let diagnostics_total = diagnostics.len();
    let diagnostics_truncated = diagnostics_total > DIAGNOSTIC_CAP;
    diagnostics.truncate(DIAGNOSTIC_CAP);

    // When anything changed (below), resolve every use for the snapshot being
    // published. The rules run over the same rows written below, so bindings
    // and facts commit in one transaction (spec §12.3).
    //
    // A PHP file that should have been indexed but was not may declare a
    // namespaced function, so the global function fallback is suppressed
    // (AF2). A skipped non-UTF-8 path has no files row; it counts when its
    // lossy spelling names an enabled PHP file.
    let php_unindexed = rivet_index::unindexed_php_files(&files)
        || walk.skipped.iter().any(|path| {
            language_for_path(&path.rel_escaped).is_some_and(|id| {
                id.name() == "php"
                    && config
                        .languages
                        .enabled
                        .iter()
                        .any(|name| name.as_str() == id.name())
            })
        });
    let unindexed_php_value = if php_unindexed { "true" } else { "false" };

    // Did content, membership, or anything bindings depend on change? The
    // resolver reads exactly the published symbols, uses, and scopes plus the
    // unindexed-PHP flag. Facts change only when a file is new, deleted,
    // changes content hash, parse status, language, or source presence, or is
    // reparsed into `ok` facts (the extractor fingerprint changed). A config
    // change can change eligibility and the flag's inputs, so it re-resolves
    // too. `--force` always re-resolves (AF6).
    let resolve_needed = force
        || !config_matches
        || !fingerprint_matches
        || !resolver_matches
        || stored_unindexed_php.as_deref() != Some(unindexed_php_value)
        || files.len() != stored_state.len()
        || files.iter().any(|file| {
            stored_state
                .get(&file.path)
                .is_none_or(|state| *state != StoredState::of(file))
        })
        || files
            .iter()
            .any(|file| file.parse_status == ParseStatus::Ok && reparsed.contains(&file.path));
    // Materialize the facts being published, in walk order. When bindings
    // must be re-resolved the resolver needs every persisted use, so the
    // stored rows of reused files are loaded (in this writer transaction) and
    // spliced in. Otherwise only freshly parsed facts exist: a reparse into
    // `ok` facts always sets `resolve_needed`, so these are the empty facts of
    // files that failed again, and the store keeps every other file's rows.
    let mut symbols: Vec<SymbolRow> = Vec::new();
    let mut uses: Vec<UseRow> = Vec::new();
    let mut scopes: Vec<ScopeRow> = Vec::new();
    let mut next_use_id = 1_i64;
    if resolve_needed && !force {
        let mut stored_symbols = symbols_by_file(store)?;
        let mut stored_uses = uses_by_file(store)?;
        let mut stored_scopes = scopes_by_file(store)?;
        // Newly reparsed uses get explicit IDs above the stored high-water
        // mark (including the uses of files about to be replaced) so the
        // in-memory resolver can name them before the publish runs.
        next_use_id = stored_uses
            .values()
            .flatten()
            .filter_map(|row| row.use_id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        for facts in pending {
            let fresh = match facts {
                PendingFacts::Stored(path) => FileFacts {
                    symbols: stored_symbols.remove(&path).unwrap_or_default(),
                    uses: stored_uses.remove(&path).unwrap_or_default(),
                    scopes: stored_scopes.remove(&path).unwrap_or_default(),
                },
                PendingFacts::Fresh(fresh) => fresh,
            };
            symbols.extend(fresh.symbols);
            uses.extend(fresh.uses);
            scopes.extend(fresh.scopes);
        }
    } else {
        // `--force` reuses nothing, so every entry is fresh. Without
        // re-resolution no stored use is read, and no fresh use can exist.
        debug_assert!(
            force
                || pending.iter().all(|facts| match facts {
                    PendingFacts::Stored(_) => true,
                    PendingFacts::Fresh(fresh) => fresh.uses.is_empty(),
                }),
            "fresh uses without re-resolution"
        );
        for facts in pending {
            if let PendingFacts::Fresh(fresh) = facts {
                symbols.extend(fresh.symbols);
                uses.extend(fresh.uses);
                scopes.extend(fresh.scopes);
            }
        }
    }
    // Give every newly extracted use an explicit ID so the in-memory resolver
    // can reference it before the publish transaction runs. Reused rows already
    // carry their stored ID, and the high-water mark keeps new IDs unique.
    for row in &mut uses {
        if row.use_id.is_none() {
            row.use_id = Some(next_use_id);
            next_use_id = next_use_id.saturating_add(1);
        }
    }
    let (links, binding_count) = if resolve_needed {
        // Each file's language picks the rule set that may bind its uses
        // (T43): PHP rules for PHP uses over PHP declarations, TypeScript
        // rules for TypeScript uses over TypeScript declarations (T44).
        let links = rivet_index::Resolver::new(&files, &symbols, &uses, &scopes)
            .with_unindexed_php_files(php_unindexed)
            .resolve_links();
        let count = links.bindings.len() as u64;
        (links, count)
    } else {
        // Nothing the bindings depend on changed: keep the stored rows, and
        // the receiver classes resolved with them (LR2).
        (
            rivet_index::ResolvedLinks::default(),
            txn.store().count_bindings().map_err(store_error)?,
        )
    };

    let fingerprint = Fingerprint {
        index_format_version: INDEX_FORMAT_VERSION.to_string(),
        effective_config,
        extractor: EXTRACTOR_FINGERPRINT.to_string(),
        resolver: RESOLVER_FINGERPRINT.to_string(),
    };
    let (symbol_count, use_count) = if resolve_needed {
        (symbols.len() as u64, uses.len() as u64)
    } else {
        // Every published fact is a kept stored row (fresh facts are empty).
        (
            txn.store().count_symbols().map_err(store_error)?,
            txn.store().count_uses().map_err(store_error)?,
        )
    };
    // OUTPUT-CONTRACT "Administrative commands": "`updated` counts current file
    // rows with changed source/status/language or regenerated facts". A file
    // reparsed because the stored extractor fingerprint differs has its facts
    // regenerated by different rules even when its source, status, and language
    // are unchanged, so it is `updated` (AF5, audit finding 16). With a current
    // fingerprint, an unchanged `ok` file is reused rather than reparsed, and an
    // unchanged failed file reparses deterministically to the same failure and
    // no facts, so nothing is regenerated and it stays `unchanged`; a changed
    // file is already `updated` by its hash or status.
    let regenerated: Vec<String> = if fingerprint_matches {
        Vec::new()
    } else {
        reparsed.clone()
    };
    let plan = StagePlan {
        reparsed: Some(reparsed.iter().cloned().collect()),
        replace_bindings: resolve_needed,
    };
    let staged = txn
        .stage_refresh(
            InventoryInput {
                fingerprint,
                files,
                symbols,
                uses,
                scopes,
                bindings: links.bindings,
                receiver_classes: links.receiver_classes,
                force,
                regenerated,
            },
            plan,
        )
        .map_err(store_error)?;
    txn.set_meta(UNINDEXED_PHP_META_KEY, unindexed_php_value)
        .map_err(store_error)?;
    debug_point!("refresh-staged-{attempt}");

    // Recheck observed metadata and the eligible path set (spec §12.4 step 3).
    // A second walk must list exactly the same eligible paths with the same
    // size and mtime the first walk observed (and so the metadata stored for
    // every file read), and the same skipped non-UTF-8 paths. A walk that
    // fails now, after the first succeeded, is a change as well. Any
    // difference rolls this attempt back.
    let stable = walk_eligible(root, config).is_ok_and(|recheck| recheck == walk);
    if !stable {
        txn.rollback().map_err(store_error)?;
        debug_point!("refresh-raced-{attempt}");
        return Ok(None);
    }

    // Compute the deterministic digest and commit.
    let published = txn.commit(staged).map_err(store_error)?;
    debug_point!("refresh-committed-{attempt}");

    record_reparsed(&reparsed);

    Ok(Some(RefreshOutcome {
        report: Report {
            snapshot: published.digest,
            freshness: mode.freshness(),
            complete,
            files_seen,
            files_indexed,
            skipped,
            diagnostics_total,
            diagnostics_truncated,
            diagnostics,
            symbols: symbol_count,
            uses: use_count,
            bindings: binding_count,
            updated: published.updated,
            unchanged: published.unchanged,
            deleted: published.deleted,
            elapsed_ms: started.elapsed().as_millis() as u64,
            timing: false,
        },
    }))
}

/// Groups every persisted symbol row by owning file.
fn symbols_by_file(store: &Store) -> Result<HashMap<String, Vec<SymbolRow>>, CliError> {
    let mut by_file: HashMap<String, Vec<SymbolRow>> = HashMap::new();
    for symbol in store.list_symbols().map_err(store_error)? {
        by_file.entry(symbol.file.clone()).or_default().push(symbol);
    }
    Ok(by_file)
}

/// Groups every persisted use row by owning file, each file's rows in
/// [`Store::list_uses_for_file`] order.
fn uses_by_file(store: &Store) -> Result<HashMap<String, Vec<UseRow>>, CliError> {
    let mut by_file: HashMap<String, Vec<UseRow>> = HashMap::new();
    for row in store.list_uses().map_err(store_error)? {
        by_file.entry(row.file.clone()).or_default().push(row);
    }
    Ok(by_file)
}

/// Groups every persisted scope row by owning file, each file's rows in
/// [`Store::list_scopes_for_file`] order.
fn scopes_by_file(store: &Store) -> Result<HashMap<String, Vec<ScopeRow>>, CliError> {
    let mut by_file: HashMap<String, Vec<ScopeRow>> = HashMap::new();
    for row in store.list_scopes().map_err(store_error)? {
        by_file.entry(row.file.clone()).or_default().push(row);
    }
    Ok(by_file)
}

/// The columns of a `files` row that decide whether its facts or bindings may
/// have changed: content hash, parse status, language, and whether source
/// bytes are stored. mtime and size are deliberately absent; a metadata-only
/// change updates those columns and nothing else.
#[derive(Debug, PartialEq, Eq)]
struct StoredState {
    content_hash: Option<String>,
    parse_status: ParseStatus,
    language: Option<String>,
    has_source: bool,
}

impl StoredState {
    fn of(file: &FileRow) -> StoredState {
        StoredState {
            content_hash: file.content_hash.clone(),
            parse_status: file.parse_status,
            language: file.language.clone(),
            has_source: file.source.is_some(),
        }
    }
}

/// Builds the report for an explicit `--no-refresh` query from the committed
/// snapshot, without walking, reading, or parsing anything (spec §12.4).
///
/// Coverage is reconstructed from the stored `files.parse_status` values and
/// `freshness` is always [`Freshness::Cached`]. Diagnostics are reconstructed
/// from the same statuses with stable codes and generic details; the exact
/// parser detail strings are not persisted. Non-UTF-8 path diagnostics are also
/// not persisted, so a cached `complete` can be optimistic relative to the
/// refresh that produced the snapshot. A database without a committed
/// `snapshot_digest` is refused rather than reported as an empty success.
pub(crate) fn cached_report(store: &Store, timing: bool) -> Result<Report, CliError> {
    let snapshot = store
        .get_meta("snapshot_digest")
        .map_err(store_error)?
        .ok_or_else(|| {
            CliError::repository_unavailable(
                "the cache has no committed snapshot",
                "Run `rivet index` to build the cache, then retry with `--no-refresh`.",
            )
        })?;

    let mut skipped = Skipped::default();
    let mut files_indexed = 0_u64;
    let mut uses = 0_u64;
    let mut diagnostics = Vec::new();
    for file in store.list_files().map_err(store_error)? {
        // A row in an enabled language without an extractor is `unsupported`
        // exactly as a refresh would report it, even when an older snapshot
        // stored it as `ok` (AF5, audit finding 14).
        let no_extractor = file
            .language
            .as_deref()
            .and_then(|name| lacks_extractor(&file.path, name));
        let status = if no_extractor.is_some() {
            ParseStatus::Unsupported
        } else {
            file.parse_status
        };
        count_status(status, &mut files_indexed, &mut skipped);
        uses += store
            .list_uses_for_file(&file.path)
            .map_err(store_error)?
            .len() as u64;
        if let Some(name) = no_extractor {
            diagnostics.push(no_extractor_diagnostic(&file.path, name));
        } else if let Some(item) = reused_status_diagnostic(&file.path, status) {
            diagnostics.push(item);
        }
    }

    diagnostics.sort_by(|a, b| {
        a.file
            .as_bytes()
            .cmp(b.file.as_bytes())
            .then_with(|| a.code.as_bytes().cmp(b.code.as_bytes()))
            .then_with(|| a.detail.as_bytes().cmp(b.detail.as_bytes()))
    });
    let diagnostics_total = diagnostics.len();
    let diagnostics_truncated = diagnostics_total > DIAGNOSTIC_CAP;
    diagnostics.truncate(DIAGNOSTIC_CAP);

    let skipped_total = skipped.total();
    let symbols = store.list_symbols().map_err(store_error)?.len() as u64;
    let bindings = store.list_bindings().map_err(store_error)?.len() as u64;
    Ok(Report {
        snapshot,
        freshness: Freshness::Cached,
        complete: skipped_total == 0,
        files_seen: files_indexed + skipped_total,
        files_indexed,
        skipped,
        diagnostics_total,
        diagnostics_truncated,
        diagnostics,
        symbols,
        uses,
        bindings,
        updated: 0,
        unchanged: 0,
        deleted: 0,
        elapsed_ms: 0,
        timing,
    })
}

/// The language name of a stored row whose path maps to a compiled language
/// with no extraction adapter, or `None` when an adapter exists.
fn lacks_extractor<'a>(path: &str, language: &'a str) -> Option<&'a str> {
    language_for_path(path)
        .filter(|id| id.name() == language && !id.has_extractor())
        .map(|_| language)
}

/// The diagnostic for an enabled-language file that no adapter extracts.
///
/// OUTPUT-CONTRACT "Common index metadata": "Diagnostics exclude ordinary
/// `unsupported` files (counted above) but include other skipped files and
/// unsupported paths." A file in a language the configuration enables is not
/// an ordinary unsupported file, so it is reported; `code` is an open string.
fn no_extractor_diagnostic(file: &str, language: &str) -> DiagnosticItem {
    DiagnosticItem {
        file: file.to_string(),
        code: "unsupported_language",
        detail: format!("{language} extraction is not implemented in this build"),
    }
}

/// One reconstructed diagnostic for a cached non-`ok` file.
fn cached_diagnostic(file: &str, code: &'static str) -> DiagnosticItem {
    let detail = match code {
        "file_too_large" => "stored as an oversized file",
        "binary_file" => "stored as binary content",
        "invalid_utf8" => "stored as non-UTF-8 content",
        "resource_limit" => "stored as a parser resource limit",
        _ => "stored parse error",
    };
    DiagnosticItem {
        file: file.to_string(),
        code,
        detail: detail.to_string(),
    }
}

/// The persisted facts for one file, built together so symbols, uses, and
/// scopes are replaced atomically by one publication.
#[derive(Default)]
struct FileFacts {
    symbols: Vec<SymbolRow>,
    uses: Vec<UseRow>,
    scopes: Vec<ScopeRow>,
}

/// One file's facts as the walk decided them.
enum PendingFacts {
    /// Reuse the stored symbol, use, and scope rows of this path.
    Stored(String),
    /// Facts produced by this refresh's parse (empty for a failed parse).
    Fresh(FileFacts),
}

/// One parsed file's status, stored source, facts, and optional diagnostic,
/// collected before the file row is pushed.
struct ParsedFileFacts {
    parse_status: ParseStatus,
    source: Option<Vec<u8>>,
    facts: PendingFacts,
    diagnostic: Option<DiagnosticItem>,
}

/// Builds a file's persisted symbols, uses, and scopes from its extraction.
///
/// The row shapes are generic; the language decides the persisted lookup
/// names (PHP folds case for some kinds, TypeScript never does) and the shape
/// of `scopes.facts_json`, which holds the facts that language's adapter
/// records.
fn build_facts(
    language: rivet_languages::LanguageId,
    path: &str,
    source: &[u8],
    extracted: &rivet_core::ExtractedFile,
) -> FileFacts {
    let ids = symbol_ids(path, &extracted.symbols);
    FileFacts {
        symbols: build_symbol_rows(language, path, source, &extracted.symbols, &ids),
        uses: use_rows(language, path, source, extracted, &ids),
        scopes: scope_rows(language, path, extracted, &ids),
    }
}

/// The canonical IDs of one file's extracted symbols.
///
/// Duplicate qualified names within the file receive one-based ordinals in
/// `(start_byte, end_byte, kind)` order (T03), and the canonical ID escapes `%`
/// and `#`.
fn symbol_ids(path: &str, extracted: &[rivet_core::ExtractedSymbol]) -> Vec<String> {
    use rivet_core::{Span, SymbolId, SymbolKind, assign_ordinals};

    let items: Vec<(&str, Span, SymbolKind)> = extracted
        .iter()
        .map(|symbol| (symbol.qualified_name.as_str(), symbol.span, symbol.kind))
        .collect();
    let ordinals = assign_ordinals(&items);
    extracted
        .iter()
        .zip(&ordinals)
        .map(|(symbol, ordinal)| {
            SymbolId::new(path, &symbol.qualified_name, *ordinal)
                .expect("a non-empty path and qualified name")
                .as_canonical()
        })
        .collect()
}

/// The persisted `lookup_name` of a declaration, by its language's rule.
fn symbol_lookup_name(
    language: rivet_languages::LanguageId,
    name: &str,
    kind: rivet_core::SymbolKind,
) -> String {
    #[cfg(not(any(feature = "lang-php", feature = "lang-typescript")))]
    let _ = (name, kind);
    match language {
        #[cfg(feature = "lang-php")]
        rivet_languages::LanguageId::Php => rivet_languages::php::lookup_name(name, kind),
        #[cfg(feature = "lang-typescript")]
        rivet_languages::LanguageId::Typescript | rivet_languages::LanguageId::Tsx => {
            rivet_languages::typescript::lookup_name(name, kind)
        }
    }
}

/// Turns extracted symbols into persisted rows for one file.
fn build_symbol_rows(
    language: rivet_languages::LanguageId,
    path: &str,
    source: &[u8],
    extracted: &[rivet_core::ExtractedSymbol],
    ids: &[String],
) -> Vec<SymbolRow> {
    use rivet_core::LineIndex;

    let lines = LineIndex::new(source);
    extracted
        .iter()
        .enumerate()
        .map(|(index, symbol)| SymbolRow {
            id: ids[index].clone(),
            file: path.to_string(),
            name: symbol.name.clone(),
            lookup_name: symbol_lookup_name(language, &symbol.name, symbol.kind),
            qualified_name: symbol.qualified_name.clone(),
            kind: symbol.kind,
            parent_id: symbol.parent_index.map(|parent| ids[parent].clone()),
            start_byte: symbol.span.start_byte(),
            end_byte: symbol.span.end_byte(),
            start_line: lines.start_line(symbol.span),
            end_line: lines.end_line(symbol.span),
            signature: symbol.signature.clone(),
            doc_comment: symbol.doc_comment.clone(),
        })
        .collect()
}

/// Turns extracted uses into persisted rows for one file.
fn use_rows(
    language: rivet_languages::LanguageId,
    path: &str,
    source: &[u8],
    extracted: &rivet_core::ExtractedFile,
    ids: &[String],
) -> Vec<UseRow> {
    use rivet_core::{LineCol, LineIndex};

    let lines = LineIndex::new(source);
    let lookup = use_lookup_names(language, extracted);
    extracted
        .uses
        .iter()
        .zip(lookup)
        .map(|(use_, lookup_name)| {
            let position = lines
                .line_col(use_.span.start_byte())
                .unwrap_or(LineCol { line: 1, column: 1 });
            UseRow {
                use_id: None,
                file: path.to_string(),
                containing_symbol: use_
                    .containing_symbol_index
                    .and_then(|index| ids.get(index).cloned()),
                scope_key: use_.scope_key.clone(),
                spelling: use_.spelling.clone(),
                lookup_name,
                ref_kind: use_.ref_kind,
                start_byte: use_.span.start_byte(),
                end_byte: use_.span.end_byte(),
                line: position.line,
                col: position.column,
                receiver: use_.receiver.clone(),
                hint_json: serde_json::to_string(&use_.hint).expect("a use hint serializes"),
            }
        })
        .collect()
}

/// The persisted `lookup_name` of each extracted use, in use order, by the
/// file's language: [`php_use_lookup_name`] for PHP, and the spelling exactly
/// as written for TypeScript (`rivet_languages::typescript::use_lookup_name`),
/// whose identifiers are case-sensitive.
fn use_lookup_names(
    language: rivet_languages::LanguageId,
    extracted: &rivet_core::ExtractedFile,
) -> Vec<String> {
    #[cfg(not(any(feature = "lang-php", feature = "lang-typescript")))]
    let _ = extracted;
    match language {
        #[cfg(feature = "lang-php")]
        rivet_languages::LanguageId::Php => {
            // The alias token of a `use const` import names a case-sensitive
            // constant (AF4); the use itself records only `import`.
            let const_imports: std::collections::HashSet<(u32, u32)> = extracted
                .imports
                .iter()
                .filter(|import| import.kind == rivet_core::extract::ImportKind::Const)
                .map(|import| (import.span.start_byte(), import.span.end_byte()))
                .collect();
            extracted
                .uses
                .iter()
                .map(|use_| {
                    let const_import = use_.ref_kind == rivet_core::RefKind::Import
                        && const_imports.contains(&(use_.span.start_byte(), use_.span.end_byte()));
                    php_use_lookup_name(&use_.spelling, use_.ref_kind, const_import)
                })
                .collect()
        }
        #[cfg(feature = "lang-typescript")]
        rivet_languages::LanguageId::Typescript | rivet_languages::LanguageId::Tsx => extracted
            .uses
            .iter()
            .map(|use_| rivet_languages::typescript::use_lookup_name(&use_.spelling))
            .collect(),
    }
}

/// Turns extracted scopes into persisted rows for one file, in the
/// `scopes.facts_json` shape of the file's language.
///
/// The adapter records declarations by symbol index; persistence rewrites each
/// to its canonical ID, the only form usable after a reparse.
fn scope_rows(
    language: rivet_languages::LanguageId,
    path: &str,
    extracted: &rivet_core::ExtractedFile,
    ids: &[String],
) -> Vec<ScopeRow> {
    #[cfg(not(any(feature = "lang-php", feature = "lang-typescript")))]
    let _ = (path, extracted, ids);
    match language {
        #[cfg(feature = "lang-php")]
        rivet_languages::LanguageId::Php => php_scope_rows(path, extracted, ids),
        #[cfg(feature = "lang-typescript")]
        rivet_languages::LanguageId::Typescript | rivet_languages::LanguageId::Tsx => {
            typescript_scope_rows(path, extracted, ids)
        }
    }
}

/// The canonical IDs among `ids` of the symbol indices a scope declares.
#[cfg(any(feature = "lang-php", feature = "lang-typescript"))]
fn declared_ids<'a>(declares: &[usize], ids: &'a [String]) -> Vec<&'a str> {
    declares
        .iter()
        .filter_map(|index| ids.get(*index).map(String::as_str))
        .collect()
}

/// TypeScript scope rows (T43): each scope's bound names, its import
/// bindings and re-exports, its module's own exports, the spans of its
/// value-position `type` uses, and whether it is an ambient module body
/// (T44), and the canonical IDs of the symbols among its names. No PHP-only
/// field is written.
#[cfg(feature = "lang-typescript")]
fn typescript_scope_rows(
    path: &str,
    extracted: &rivet_core::ExtractedFile,
    ids: &[String],
) -> Vec<ScopeRow> {
    use rivet_core::Span;
    use rivet_core::extract::{LocalBinding, ModuleExport, ModuleImport};

    /// The exact persisted `scopes.facts_json` shape of a TypeScript scope.
    #[derive(serde::Serialize)]
    struct PersistedScopeFacts<'a> {
        locals: &'a [LocalBinding],
        module_imports: &'a [ModuleImport],
        module_exports: &'a [ModuleExport],
        value_type_uses: &'a [Span],
        ambient_module: bool,
        declares: Vec<&'a str>,
    }

    extracted
        .scopes
        .iter()
        .map(|scope| {
            let facts = PersistedScopeFacts {
                locals: &scope.facts.locals,
                module_imports: &scope.facts.module_imports,
                module_exports: &scope.facts.module_exports,
                value_type_uses: &scope.facts.value_type_uses,
                ambient_module: scope.facts.ambient_module,
                declares: declared_ids(&scope.facts.declares, ids),
            };
            ScopeRow {
                file: path.to_string(),
                scope_key: scope.scope_key.clone(),
                parent_scope_key: scope.parent_scope_key.clone(),
                facts_json: serde_json::to_string(&facts).expect("scope facts serialize"),
            }
        })
        .collect()
}

/// PHP scope rows, in the shape the PHP resolver rules read.
#[cfg(feature = "lang-php")]
fn php_scope_rows(
    path: &str,
    extracted: &rivet_core::ExtractedFile,
    ids: &[String],
) -> Vec<ScopeRow> {
    use rivet_core::Span;
    use rivet_core::extract::{CallArg, NewBinding, ScopeImport, SupertypeRelation, TypedBinding};

    /// One declaration's by-reference parameter flags, keyed by canonical ID
    /// (AF3).
    #[derive(serde::Serialize)]
    struct PersistedParameterList<'a> {
        symbol: &'a str,
        by_ref: &'a [bool],
    }

    /// One declared supertype of a class-like, keyed by canonical ID (T36d).
    #[derive(serde::Serialize)]
    struct PersistedSupertype<'a> {
        symbol: &'a str,
        relation: SupertypeRelation,
        spelling: &'a str,
        span: Span,
    }

    /// The exact persisted `scopes.facts_json` shape.
    #[derive(serde::Serialize)]
    struct PersistedScopeFacts<'a> {
        imports: &'a [ScopeImport],
        typed_bindings: &'a [TypedBinding],
        new_bindings: &'a [NewBinding],
        call_args: &'a [CallArg],
        unanalysable: bool,
        namespace_unattributed: bool,
        class_constant_accesses: &'a [Span],
        global_scope: bool,
        call_sites: &'a [Span],
        goto_present: bool,
        global_names: &'a [String],
        dynamic_global_write: bool,
        parameter_lists: Vec<PersistedParameterList<'a>>,
        supertypes: Vec<PersistedSupertype<'a>>,
        anonymous_supertypes: &'a [rivet_core::extract::AnonymousSupertype],
        declares: Vec<&'a str>,
    }

    extracted
        .scopes
        .iter()
        .map(|scope| {
            let declares = declared_ids(&scope.facts.declares, ids);
            let parameter_lists: Vec<PersistedParameterList> = scope
                .facts
                .parameter_lists
                .iter()
                .filter_map(|list| {
                    ids.get(list.symbol).map(|id| PersistedParameterList {
                        symbol: id.as_str(),
                        by_ref: &list.by_ref,
                    })
                })
                .collect();
            let supertypes: Vec<PersistedSupertype> = scope
                .facts
                .supertypes
                .iter()
                .filter_map(|supertype| {
                    ids.get(supertype.symbol).map(|id| PersistedSupertype {
                        symbol: id.as_str(),
                        relation: supertype.relation,
                        spelling: &supertype.spelling,
                        span: supertype.span,
                    })
                })
                .collect();
            let facts = PersistedScopeFacts {
                imports: &scope.facts.imports,
                typed_bindings: &scope.facts.typed_bindings,
                new_bindings: &scope.facts.new_bindings,
                call_args: &scope.facts.call_args,
                unanalysable: scope.facts.unanalysable,
                namespace_unattributed: scope.facts.namespace_unattributed,
                class_constant_accesses: &scope.facts.class_constant_accesses,
                global_scope: scope.facts.global_scope,
                call_sites: &scope.facts.call_sites,
                goto_present: scope.facts.goto_present,
                global_names: &scope.facts.global_names,
                dynamic_global_write: scope.facts.dynamic_global_write,
                parameter_lists,
                supertypes,
                anonymous_supertypes: &scope.facts.anonymous_supertypes,
                declares,
            };
            ScopeRow {
                file: path.to_string(),
                scope_key: scope.scope_key.clone(),
                parent_scope_key: scope.parent_scope_key.clone(),
                facts_json: serde_json::to_string(&facts).expect("scope facts serialize"),
            }
        })
        .collect()
}

/// The persisted `lookup_name` for one PHP use.
///
/// The name is the use's normalized short name (AF4), so it is comparable with
/// the `lookup_name` of the declaration it could name: the last segment of a
/// qualified spelling (`\App\Foo`, `Sub\Missing\Foo`, and `namespace\Foo`
/// all give `Foo`) with any leading `$` removed (`Foo::$count` gives `count`,
/// like `$x->count`).
///
/// Case folding builds on AF2. PHP call, type, and class or function import
/// names are case-insensitive, so those fold ASCII letters to lowercase, as
/// [`rivet_languages::php::lookup_name`] folds the declaration side. Property
/// and constant reads and writes, and the alias of a `use const` import
/// (`const_import`), keep their exact spelling. An `unknown` use (a bare
/// identifier such as the constant `LIMIT`) has no known referenced kind, so it
/// keeps its exact spelling too, and the comparison folds it according to the
/// candidate declaration's kind instead ([`rivet_index::lookup_name_matches`]).
#[cfg(feature = "lang-php")]
fn php_use_lookup_name(
    spelling: &str,
    ref_kind: rivet_core::RefKind,
    const_import: bool,
) -> String {
    use rivet_core::{RefKind, SymbolKind};

    let short = rivet_languages::php::use_short_name(spelling);
    match ref_kind {
        RefKind::Read | RefKind::Write | RefKind::Assignment | RefKind::Unknown => {
            rivet_languages::php::lookup_name(short, SymbolKind::Property)
        }
        RefKind::Import if const_import => {
            rivet_languages::php::lookup_name(short, SymbolKind::Const)
        }
        RefKind::Call | RefKind::Type | RefKind::Import => short.to_ascii_lowercase(),
    }
}

/// Narrows an extraction diagnostic code to a `'static` contract spelling.
fn static_diagnostic_code(code: &str) -> &'static str {
    match code {
        "resource_limit" => "resource_limit",
        _ => "parse_error",
    }
}

/// Adds one file's persisted status to the coverage counters.
fn count_status(status: ParseStatus, files_indexed: &mut u64, skipped: &mut Skipped) {
    match status {
        ParseStatus::Ok => *files_indexed += 1,
        ParseStatus::Unsupported => skipped.unsupported += 1,
        ParseStatus::Binary => skipped.binary += 1,
        ParseStatus::Size => skipped.size += 1,
        ParseStatus::Encoding => skipped.encoding += 1,
        ParseStatus::ParseError => skipped.parse_error += 1,
        ParseStatus::ResourceLimit => skipped.resource_limit += 1,
    }
}

/// The diagnostic for a non-`ok` status reused from stored metadata.
///
/// Metadata mode does not reread the file, so parser details are unavailable;
/// the stable code and a stored-status detail are reported instead. Ordinary
/// `unsupported` files stay counted only.
fn reused_status_diagnostic(file: &str, status: ParseStatus) -> Option<DiagnosticItem> {
    let code = match status {
        ParseStatus::ParseError => "parse_error",
        ParseStatus::ResourceLimit => "resource_limit",
        ParseStatus::Size => "file_too_large",
        ParseStatus::Binary => "binary_file",
        ParseStatus::Encoding => "invalid_utf8",
        ParseStatus::Ok | ParseStatus::Unsupported => return None,
    };
    Some(cached_diagnostic(file, code))
}

/// Increments the skip count for `reason` and returns the persisted status.
///
/// Symlinks and non-regular files cannot reach here because traversal excludes
/// them; if one does, it is classified `unsupported` rather than treated as
/// normal source.
fn classify_skip(reason: rivet_core::SkipReason, skipped: &mut Skipped) -> ParseStatus {
    use rivet_core::SkipReason;

    match reason {
        SkipReason::Size => {
            skipped.size += 1;
            ParseStatus::Size
        }
        SkipReason::Binary => {
            skipped.binary += 1;
            ParseStatus::Binary
        }
        SkipReason::Encoding => {
            skipped.encoding += 1;
            ParseStatus::Encoding
        }
        SkipReason::Symlink | SkipReason::NotRegular => {
            skipped.unsupported += 1;
            ParseStatus::Unsupported
        }
    }
}

/// The diagnostic code and detail for a T07 skip, or `None` when the skip is
/// already represented by a coverage count only.
fn skip_diagnostic(reason: rivet_core::SkipReason) -> Option<(&'static str, &'static str)> {
    use rivet_core::SkipReason;

    match reason {
        SkipReason::Size => Some(("file_too_large", "exceeds max_file_size_kb")),
        SkipReason::Binary => Some(("binary_file", "NUL byte within the inspected prefix")),
        SkipReason::Encoding => Some(("invalid_utf8", "source is not valid UTF-8")),
        SkipReason::Symlink | SkipReason::NotRegular => None,
    }
}

/// Records the paths reparsed by this refresh when the test hook is enabled.
///
/// The hook appends one repository-relative path per line to the file named by
/// `RIVET_DEBUG_REPARSED`. It is compiled only into debug builds, so release
/// binaries never read the variable.
#[cfg(debug_assertions)]
fn record_reparsed(paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let Ok(target) = std::env::var("RIVET_DEBUG_REPARSED") else {
        return;
    };
    if target.is_empty() {
        return;
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&target)
    else {
        return;
    };
    use std::io::Write as _;
    for path in paths {
        let _ = writeln!(file, "{path}");
    }
}

/// Release builds have no reparse-recording hook.
#[cfg(not(debug_assertions))]
fn record_reparsed(_paths: &[String]) {}

#[cfg(test)]
mod tests {
    use super::{RefreshMode, refresh};
    use rivet_core::{Config, ParseStatus, content_hash};
    use rivet_store::{FileRow, Fingerprint, INDEX_FORMAT_VERSION, InventoryInput, Store};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A stale extractor fingerprint must reparse content even when the stored
    /// hash and parse status are unchanged.
    #[test]
    fn extractor_fingerprint_change_reparses_equal_content() {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "rivet-refresh-unit-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create temp root");

        let bytes = b"<?php\nnamespace App;\nfunction f(): void {}\n".to_vec();
        fs::write(root.join("a.php"), &bytes).expect("write fixture");

        let config = Config::default();
        let mut store = Store::open_in_memory().expect("open in-memory store");
        // A current hash and an `ok` status that would be reused, but a stale
        // extractor fingerprint and no persisted symbols.
        store
            .publish_inventory(InventoryInput {
                fingerprint: Fingerprint {
                    index_format_version: INDEX_FORMAT_VERSION.to_string(),
                    effective_config: config.fingerprint(),
                    extractor: "stale-extractor".to_string(),
                    resolver: "none".to_string(),
                },
                files: vec![FileRow {
                    path: "a.php".to_string(),
                    language: Some("php".to_string()),
                    mtime_ns: 0,
                    size: bytes.len() as u64,
                    content_hash: Some(content_hash(&bytes)),
                    source: Some(bytes.clone()),
                    parse_status: ParseStatus::Ok,
                }],
                symbols: Vec::new(),
                uses: Vec::new(),
                scopes: Vec::new(),
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .expect("publish stale inventory");

        let outcome = refresh(&root, &config, &mut store, RefreshMode::Content, false)
            .expect("refresh succeeds");
        assert!(
            outcome.report.symbols > 0,
            "fingerprint change must reparse and republish facts"
        );
        assert!(
            store.get_symbol("a.php#App\\f").expect("read").is_some(),
            "reparsed facts must be published"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
