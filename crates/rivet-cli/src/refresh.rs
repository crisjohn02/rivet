//! Refresh and cached-read paths shared by `rivet index` and every query
//! command (spec §12.3, §12.4 steps 1–2 and 4; ARCHITECTURE "Refresh and
//! invalidation" and "Concurrency and source consistency").
//!
//! [`refresh`] is the single code path that performs a refresh: it walks the
//! eligible files, compares the stored effective-config, extractor, and
//! resolver fingerprints, reads and hashes each enabled-language file (or
//! reuses stored hashes in [`RefreshMode::Metadata`]), reparses only changed
//! content (or *all* enabled content when the extractor fingerprint differs),
//! drops facts for deleted or newly excluded files, and publishes one atomic
//! snapshot. Files whose content hash and stored parse status are unchanged keep
//! their stored symbols; their mtime/size are still rewritten. `--force` clears
//! every stored fact and rebuilds it in the same publish transaction.
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
use rivet_parser::parse_file;
use rivet_store::{
    FileRow, Fingerprint, INDEX_FORMAT_VERSION, InventoryInput, ScopeRow, Store, SymbolRow, UseRow,
    clamp_mtime_ns,
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
    Store::open(&rivet_dir).map_err(cached_store_error)
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
/// T19 writes a real version because bindings are computed on every publish. A
/// future change to the resolution rules bumps this so the snapshot digest and
/// T22's invalidation trigger both see it.
const RESOLVER_FINGERPRINT: &str = "php-rules-v1";

/// The shared result of one refresh.
///
/// The embedded [`Report`] is the same data `rivet index` formats, so query
/// commands can attach the identical coverage/diagnostics/snapshot metadata.
#[derive(Debug)]
pub struct RefreshOutcome {
    /// Coverage, diagnostics, counts, and the committed snapshot digest.
    pub report: Report,
}

/// Refreshes the index inside one publish transaction and returns its report.
///
/// This is the only function that walks, parses, and publishes on the query
/// path. A failed parse does not abort the refresh: the file is recorded with
/// its failure status and no symbols, and unrelated results stay available
/// with partial coverage (ARCHITECTURE "Parse and coverage policy"). An I/O
/// failure aborts, publishing nothing. `force` clears every stored fact and
/// rebuilds it in the same transaction (spec §13).
pub fn refresh(
    root: &Path,
    config: &Config,
    store: &mut Store,
    mode: RefreshMode,
    force: bool,
) -> Result<RefreshOutcome, CliError> {
    refresh_inventory(root, config, store, mode, force)
}

/// Walks, reads, conditionally reparses, and publishes the complete inventory.
fn refresh_inventory(
    root: &Path,
    config: &Config,
    store: &mut Store,
    mode: RefreshMode,
    force: bool,
) -> Result<RefreshOutcome, CliError> {
    let started = Instant::now();

    let walk = walk_eligible(root, config).map_err(walk_error)?;
    let max_bytes = config.index.max_file_size_kb.saturating_mul(1024);

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

    // Resolver invalidation. T19 recomputes bindings for every use on every
    // publish (spec §12.3), so a changed resolver fingerprint needs no separate
    // clear here; T22 adds the membership-change triggers.
    let stored_resolver = store
        .get_meta("resolver_fingerprint")
        .map_err(store_error)?;
    let _resolver_changed = stored_resolver.as_deref() != Some(RESOLVER_FINGERPRINT);

    // Load the current inventory and facts once. Reused files keep their
    // stored source bytes and symbol/use/scope rows.
    let mut stored_by_path: HashMap<String, FileRow> = store
        .list_files()
        .map_err(store_error)?
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect();
    let mut stored_symbols = symbols_by_file(store)?;
    let mut stored_uses = uses_by_file(store)?;
    let mut stored_scopes = scopes_by_file(store)?;
    // Newly reparsed uses get explicit IDs above this high-water mark so the
    // in-memory resolver can name them before the publish transaction runs.
    let mut next_use_id = stored_uses
        .values()
        .flatten()
        .filter_map(|row| row.use_id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    let mut files = Vec::with_capacity(walk.files.len());
    let mut symbols: Vec<SymbolRow> = Vec::new();
    let mut uses: Vec<UseRow> = Vec::new();
    let mut scopes: Vec<ScopeRow> = Vec::new();
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
            let facts = if file.parse_status == ParseStatus::Ok {
                FileFacts {
                    symbols: stored_symbols.remove(&entry.rel_path).unwrap_or_default(),
                    uses: stored_uses.remove(&entry.rel_path).unwrap_or_default(),
                    scopes: stored_scopes.remove(&entry.rel_path).unwrap_or_default(),
                }
            } else {
                stored_symbols.remove(&entry.rel_path);
                stored_uses.remove(&entry.rel_path);
                stored_scopes.remove(&entry.rel_path);
                FileFacts::default()
            };
            count_status(file.parse_status, &mut files_indexed, &mut skipped);
            if let Some(item) = reused_status_diagnostic(&entry.rel_path, file.parse_status) {
                diagnostics.push(item);
            }
            symbols.extend(facts.symbols);
            uses.extend(facts.uses);
            scopes.extend(facts.scopes);
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
                        facts: FileFacts {
                            symbols: stored_symbols.remove(&entry.rel_path).unwrap_or_default(),
                            uses: stored_uses.remove(&entry.rel_path).unwrap_or_default(),
                            scopes: stored_scopes.remove(&entry.rel_path).unwrap_or_default(),
                        },
                        diagnostic: None,
                    }
                } else {
                    reparsed.push(entry.rel_path.clone());
                    let extracted = parse_file(id, &bytes);
                    match extracted.diagnostics.first() {
                        Some(diagnostic) => {
                            let status = match diagnostic.code.as_str() {
                                "resource_limit" => ParseStatus::ResourceLimit,
                                _ => ParseStatus::ParseError,
                            };
                            ParsedFileFacts {
                                parse_status: status,
                                source: None,
                                facts: FileFacts::default(),
                                diagnostic: Some(DiagnosticItem {
                                    file: entry.rel_path.clone(),
                                    code: static_diagnostic_code(&diagnostic.code),
                                    detail: diagnostic.detail.clone(),
                                }),
                            }
                        }
                        None => {
                            let file_facts = build_facts(&entry.rel_path, &bytes, &extracted);
                            ParsedFileFacts {
                                parse_status: ParseStatus::Ok,
                                source: Some(bytes),
                                facts: file_facts,
                                diagnostic: None,
                            }
                        }
                    }
                };

                count_status(facts.parse_status, &mut files_indexed, &mut skipped);
                if let Some(item) = facts.diagnostic {
                    diagnostics.push(item);
                }
                symbols.extend(facts.facts.symbols);
                uses.extend(facts.facts.uses);
                scopes.extend(facts.facts.scopes);
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
    // `complete: false` (OUTPUT-CONTRACT "Common index metadata").
    for path in &walk.skipped {
        diagnostics.push(DiagnosticItem {
            file: path.lossy.clone(),
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

    // Give every newly extracted use an explicit ID so the in-memory resolver
    // can reference it before the publish transaction runs. Reused rows already
    // carry their stored ID, and the high-water mark keeps new IDs unique.
    for row in &mut uses {
        if row.use_id.is_none() {
            row.use_id = Some(next_use_id);
            next_use_id = next_use_id.saturating_add(1);
        }
    }
    // Resolve every use for the snapshot being published. The rules run over
    // the same rows written below, so bindings and facts commit in one
    // transaction (spec §12.3).
    let bindings = rivet_index::Resolver::new(&symbols, &uses, &scopes).resolve();
    let binding_count = bindings.len() as u64;

    let fingerprint = Fingerprint {
        index_format_version: INDEX_FORMAT_VERSION.to_string(),
        effective_config,
        extractor: EXTRACTOR_FINGERPRINT.to_string(),
        resolver: RESOLVER_FINGERPRINT.to_string(),
    };
    let symbol_count = symbols.len() as u64;
    let use_count = uses.len() as u64;
    let published = store
        .publish_inventory(InventoryInput {
            fingerprint,
            files,
            symbols,
            uses,
            scopes,
            bindings,
            force,
        })
        .map_err(store_error)?;

    record_reparsed(&reparsed);

    Ok(RefreshOutcome {
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
    })
}

/// Groups every persisted symbol row by owning file.
fn symbols_by_file(store: &Store) -> Result<HashMap<String, Vec<SymbolRow>>, CliError> {
    let mut by_file: HashMap<String, Vec<SymbolRow>> = HashMap::new();
    for symbol in store.list_symbols().map_err(store_error)? {
        by_file.entry(symbol.file.clone()).or_default().push(symbol);
    }
    Ok(by_file)
}

/// Groups every persisted use row by owning file.
fn uses_by_file(store: &Store) -> Result<HashMap<String, Vec<UseRow>>, CliError> {
    let mut by_file: HashMap<String, Vec<UseRow>> = HashMap::new();
    for file in store.list_files().map_err(store_error)? {
        let rows = store.list_uses_for_file(&file.path).map_err(store_error)?;
        if !rows.is_empty() {
            by_file.insert(file.path, rows);
        }
    }
    Ok(by_file)
}

/// Groups every persisted scope row by owning file.
fn scopes_by_file(store: &Store) -> Result<HashMap<String, Vec<ScopeRow>>, CliError> {
    let mut by_file: HashMap<String, Vec<ScopeRow>> = HashMap::new();
    for file in store.list_files().map_err(store_error)? {
        let rows = store
            .list_scopes_for_file(&file.path)
            .map_err(store_error)?;
        if !rows.is_empty() {
            by_file.insert(file.path, rows);
        }
    }
    Ok(by_file)
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
        count_status(file.parse_status, &mut files_indexed, &mut skipped);
        uses += store
            .list_uses_for_file(&file.path)
            .map_err(store_error)?
            .len() as u64;
        if let Some(item) = reused_status_diagnostic(&file.path, file.parse_status) {
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

/// One parsed file's status, stored source, facts, and optional diagnostic,
/// collected before the file row is pushed.
struct ParsedFileFacts {
    parse_status: ParseStatus,
    source: Option<Vec<u8>>,
    facts: FileFacts,
    diagnostic: Option<DiagnosticItem>,
}

/// Builds a file's persisted symbols, uses, and scopes from its extraction.
///
/// Only PHP has an adapter in this milestone; the TypeScript adapter will fill
/// this in later without changing the refresh path.
#[cfg(feature = "lang-php")]
fn build_facts(path: &str, source: &[u8], extracted: &rivet_core::ExtractedFile) -> FileFacts {
    let ids = symbol_ids(path, &extracted.symbols);
    FileFacts {
        symbols: build_symbol_rows(path, source, &extracted.symbols, &ids),
        uses: use_rows(path, source, extracted, &ids),
        scopes: scope_rows(path, extracted, &ids),
    }
}

/// No compiled language adapter yet: a parsed tree yields no persisted facts.
#[cfg(not(feature = "lang-php"))]
fn build_facts(_path: &str, _source: &[u8], _extracted: &rivet_core::ExtractedFile) -> FileFacts {
    FileFacts::default()
}

/// The canonical IDs of one file's extracted symbols.
///
/// Duplicate qualified names within the file receive one-based ordinals in
/// `(start_byte, end_byte, kind)` order (T03), and the canonical ID escapes `%`
/// and `#`.
#[cfg(feature = "lang-php")]
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

/// Turns extracted symbols into persisted rows for one file.
#[cfg(feature = "lang-php")]
fn build_symbol_rows(
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
            lookup_name: rivet_languages::php::lookup_name(&symbol.name, symbol.kind),
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
#[cfg(feature = "lang-php")]
fn use_rows(
    path: &str,
    source: &[u8],
    extracted: &rivet_core::ExtractedFile,
    ids: &[String],
) -> Vec<UseRow> {
    use rivet_core::{LineCol, LineIndex};

    let lines = LineIndex::new(source);
    extracted
        .uses
        .iter()
        .map(|use_| {
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
                lookup_name: use_lookup_name(&use_.spelling, use_.ref_kind),
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

/// Turns extracted scopes into persisted rows for one file.
///
/// The adapter records declarations by symbol index; persistence rewrites each
/// to its canonical ID, the only form usable after a reparse.
#[cfg(feature = "lang-php")]
fn scope_rows(path: &str, extracted: &rivet_core::ExtractedFile, ids: &[String]) -> Vec<ScopeRow> {
    use rivet_core::extract::{NewBinding, ScopeImport, TypedBinding};

    /// The exact persisted `scopes.facts_json` shape.
    #[derive(serde::Serialize)]
    struct PersistedScopeFacts<'a> {
        imports: &'a [ScopeImport],
        typed_bindings: &'a [TypedBinding],
        new_bindings: &'a [NewBinding],
        declares: Vec<&'a str>,
    }

    extracted
        .scopes
        .iter()
        .map(|scope| {
            let declares: Vec<&str> = scope
                .facts
                .declares
                .iter()
                .filter_map(|index| ids.get(*index).map(String::as_str))
                .collect();
            let facts = PersistedScopeFacts {
                imports: &scope.facts.imports,
                typed_bindings: &scope.facts.typed_bindings,
                new_bindings: &scope.facts.new_bindings,
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

/// The persisted `lookup_name` for one use.
///
/// PHP call, type, and import names are case-insensitive, so those fold to
/// lowercase; property and constant reads/writes keep their exact spelling.
/// An `Unknown` use has no known referenced kind, so it is stored lowercase.
#[cfg(feature = "lang-php")]
fn use_lookup_name(spelling: &str, ref_kind: rivet_core::RefKind) -> String {
    use rivet_core::{RefKind, SymbolKind};

    match ref_kind {
        RefKind::Read | RefKind::Write | RefKind::Assignment => {
            rivet_languages::php::lookup_name(spelling, SymbolKind::Property)
        }
        RefKind::Call | RefKind::Type | RefKind::Import | RefKind::Unknown => {
            spelling.to_lowercase()
        }
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
                force: false,
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
