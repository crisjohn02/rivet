//! `rivet index` inventory refresh (spec §12.4 steps 1–2, §13; OUTPUT-CONTRACT
//! "Common index metadata" and "Administrative commands").
//!
//! T10 is an inventory milestone: it discovers the root, loads config, walks
//! eligible files, reads compiled-language source through the bounded T07
//! reader, publishes an atomic file inventory, and reports coverage. It does not
//! parse source, so a readable `.php`/`.ts`/`.tsx` file is stored as `ok` while
//! every other regular file is `unsupported` and no symbols, uses, or bindings
//! are produced.

use std::io;
use std::path::Path;
use std::time::Instant;

use serde_json::{Map, Value, json};

use rivet_core::{
    Config, ConfigError, Freshness, ParseStatus, RootError, SkipReason, SourceRead, WalkError,
    discover_root, read_source, walk_eligible,
};
use rivet_languages::{EXTRACTOR_FINGERPRINT, is_language_compiled, language_for_path};
use rivet_store::{
    FileRow, Fingerprint, INDEX_FORMAT_VERSION, InventoryInput, Store, clamp_mtime_ns,
};

use crate::transport::CliError;

/// Maximum number of diagnostic items emitted; counts stay exhaustive.
const DIAGNOSTIC_CAP: usize = 50;

/// Options accepted by `rivet index`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--force`: accepted for contract compatibility. T10 always re-reads and
    /// re-hashes eligible files, so fact regeneration is a no-op until T15/T16.
    pub force: bool,
    /// `--timing`: include `elapsed_ms` in JSON output.
    pub timing: bool,
    /// `--languages a,b`: override the configured enabled languages.
    pub languages: Option<String>,
    /// `--freshness content|metadata`: override the configured freshness.
    pub freshness: Option<String>,
}

/// Coverage skip counts, one per non-`ok` parse status.
#[derive(Debug, Default, Clone, Copy)]
struct Skipped {
    unsupported: u64,
    binary: u64,
    size: u64,
    encoding: u64,
    parse_error: u64,
    resource_limit: u64,
}

impl Skipped {
    /// The sum of every skipped count.
    fn total(self) -> u64 {
        self.unsupported
            + self.binary
            + self.size
            + self.encoding
            + self.parse_error
            + self.resource_limit
    }
}

/// One `diagnostics.items[]` entry.
#[derive(Debug, Clone)]
struct DiagnosticItem {
    file: String,
    code: &'static str,
    detail: String,
}

/// The success payload of one refresh.
#[derive(Debug)]
pub struct Report {
    snapshot: String,
    freshness: Freshness,
    complete: bool,
    files_seen: u64,
    files_indexed: u64,
    skipped: Skipped,
    diagnostics_total: usize,
    diagnostics_truncated: bool,
    diagnostics: Vec<DiagnosticItem>,
    symbols: u64,
    uses: u64,
    bindings: u64,
    updated: u64,
    unchanged: u64,
    deleted: u64,
    elapsed_ms: u64,
    timing: bool,
}

/// Runs one `rivet index`.
pub fn run(options: Options) -> Result<Report, CliError> {
    let started = Instant::now();
    let Options {
        force: _force,
        timing,
        languages,
        freshness,
    } = options;

    // Argument validation happens before any filesystem discovery (spec §27:
    // invalid arguments take precedence over filesystem work).
    let requested_languages = parse_languages(languages.as_deref())?;
    let requested_freshness = parse_freshness(freshness.as_deref())?;

    let cwd = std::env::current_dir().map_err(|error| {
        CliError::repository_unavailable(
            format!("cannot determine the working directory: {error}"),
            "Run rivet from inside a repository.",
        )
    })?;
    let root = discover_root(&cwd).map_err(root_error)?;
    let config = Config::load(&root.root).map_err(config_error)?;

    // `--freshness metadata` is accepted and recorded in `index.freshness`, but
    // T10 hashes source content in both modes; trusting size/mtime and skipping
    // unchanged reads is T16.
    let effective_freshness = requested_freshness.unwrap_or(config.index.freshness);

    let enabled = match requested_languages {
        Some(languages) => languages,
        None => {
            validate_configured_languages(&config)?;
            config.languages.enabled.clone()
        }
    };

    // A query auto-creates `.rivet/` only at a Git root and never edits
    // `.gitignore` (spec §25). `discover_root` reports `has_rivet_dir = false`
    // only when the boundary was `.git`, so this creates the cache directory
    // exactly there.
    let rivet_dir = root.root.join(".rivet");
    if !root.has_rivet_dir {
        create_rivet_dir(&rivet_dir)?;
    }

    let mut store = Store::open(&rivet_dir).map_err(store_error)?;
    let walk = walk_eligible(&root.root, &config).map_err(walk_error)?;

    let max_bytes = config.index.max_file_size_kb.saturating_mul(1024);
    let mut files = Vec::with_capacity(walk.files.len());
    let mut skipped = Skipped::default();
    let mut diagnostics = Vec::new();
    let mut files_indexed = 0_u64;

    for entry in &walk.files {
        let language = language_for_path(&entry.rel_path)
            .filter(|id| enabled.iter().any(|name| name.as_str() == id.name()));
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
        match read_source(&root.root, entry, max_bytes) {
            SourceRead::Ok { bytes, hash } => {
                files_indexed += 1;
                files.push(FileRow {
                    path: entry.rel_path.clone(),
                    language: Some(language_name),
                    mtime_ns: clamp_mtime_ns(entry.mtime_ns),
                    size: entry.size,
                    content_hash: Some(hash),
                    source: Some(bytes),
                    parse_status: ParseStatus::Ok,
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

    let fingerprint = Fingerprint {
        index_format_version: INDEX_FORMAT_VERSION.to_string(),
        effective_config: config.fingerprint(),
        extractor: EXTRACTOR_FINGERPRINT.to_string(),
        resolver: "none".to_string(),
    };
    let published = store
        .publish_inventory(InventoryInput { fingerprint, files })
        .map_err(store_error)?;

    Ok(Report {
        snapshot: published.digest,
        freshness: effective_freshness,
        complete,
        files_seen,
        files_indexed,
        skipped,
        diagnostics_total,
        diagnostics_truncated,
        diagnostics,
        // No parsing yet: symbols, uses, and bindings are always zero in T10.
        symbols: 0,
        uses: 0,
        bindings: 0,
        updated: published.updated,
        unchanged: published.unchanged,
        deleted: published.deleted,
        elapsed_ms: started.elapsed().as_millis() as u64,
        timing,
    })
}

/// Builds the top-level `index --json` object (without `schema_version`, which
/// the transport prepends).
pub fn success_json(report: &Report) -> Value {
    let skipped = json!({
        "unsupported": report.skipped.unsupported,
        "binary": report.skipped.binary,
        "size": report.skipped.size,
        "encoding": report.skipped.encoding,
        "parse_error": report.skipped.parse_error,
        "resource_limit": report.skipped.resource_limit,
    });
    let coverage = json!({
        "complete": report.complete,
        "files_seen": report.files_seen,
        "files_indexed": report.files_indexed,
        "skipped": skipped,
    });
    let items: Vec<Value> = report
        .diagnostics
        .iter()
        .map(|item| {
            json!({
                "file": item.file,
                "code": item.code,
                "detail": item.detail,
            })
        })
        .collect();
    let diagnostics = json!({
        "total": report.diagnostics_total,
        "truncated": report.diagnostics_truncated,
        "items": items,
    });

    let mut object = Map::new();
    object.insert(
        "index".to_string(),
        json!({
            "snapshot": report.snapshot,
            "freshness": report.freshness.as_str(),
            "coverage": coverage,
            "diagnostics": diagnostics,
        }),
    );
    object.insert("symbols".to_string(), json!(report.symbols));
    object.insert("uses".to_string(), json!(report.uses));
    object.insert("bindings".to_string(), json!(report.bindings));
    object.insert("updated".to_string(), json!(report.updated));
    object.insert("unchanged".to_string(), json!(report.unchanged));
    object.insert("deleted".to_string(), json!(report.deleted));
    if report.timing {
        object.insert("elapsed_ms".to_string(), json!(report.elapsed_ms));
    }
    Value::Object(object)
}

/// The human `rivet index` output (spec §13).
pub fn human(report: &Report) -> String {
    format!(
        "Indexed {} files\n{} symbols\n{} relationships\n\nUpdated: {}\nUnchanged: {}\n\nElapsed: {} ms\n",
        report.files_seen,
        report.symbols,
        report.uses,
        report.updated,
        report.unchanged,
        report.elapsed_ms
    )
}

/// Increments the skip count for `reason` and returns the persisted status.
///
/// Symlinks and non-regular files cannot reach here because traversal excludes
/// them; if one does, it is classified `unsupported` rather than treated as
/// normal source.
fn classify_skip(reason: SkipReason, skipped: &mut Skipped) -> ParseStatus {
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
fn skip_diagnostic(reason: SkipReason) -> Option<(&'static str, &'static str)> {
    match reason {
        SkipReason::Size => Some(("file_too_large", "exceeds max_file_size_kb")),
        SkipReason::Binary => Some(("binary_file", "NUL byte within the inspected prefix")),
        SkipReason::Encoding => Some(("invalid_utf8", "source is not valid UTF-8")),
        SkipReason::Symlink | SkipReason::NotRegular => None,
    }
}

/// Parses `--languages`, rejecting empty and uncompiled names before any
/// filesystem work.
fn parse_languages(value: Option<&str>) -> Result<Option<Vec<String>>, CliError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut languages = Vec::new();
    for raw in value.split(',') {
        let name = raw.trim();
        if name.is_empty() || !is_language_compiled(name) {
            return Err(CliError::invalid_arguments(
                format!(
                    "invalid value for `--languages`: {value:?} (expected a comma-separated list of compiled languages)"
                ),
                "Pass `--languages php` or `--languages php,typescript`.",
            ));
        }
        if !languages.iter().any(|existing: &String| existing == name) {
            languages.push(name.to_string());
        }
    }
    Ok(Some(languages))
}

/// Parses `--freshness`, rejecting unknown values before any filesystem work.
fn parse_freshness(value: Option<&str>) -> Result<Option<Freshness>, CliError> {
    let Some(value) = value else {
        return Ok(None);
    };
    value.parse::<Freshness>().map(Some).map_err(|()| {
        CliError::invalid_arguments(
            format!(
                "invalid value for `--freshness`: {value:?} (expected \"content\" or \"metadata\")"
            ),
            "Pass `--freshness content` or `--freshness metadata`.",
        )
    })
}

/// Rejects a configured language that this binary was not built with.
fn validate_configured_languages(config: &Config) -> Result<(), CliError> {
    for name in &config.languages.enabled {
        if !is_language_compiled(name) {
            return Err(CliError::invalid_arguments(
                format!(
                    "invalid value for `languages.enabled`: {name:?} is not compiled into this binary"
                ),
                "Remove the language from .rivet/config.toml or rebuild with its feature enabled.",
            ));
        }
    }
    Ok(())
}

/// Creates the cache directory at a Git root, tolerating a concurrent creation.
fn create_rivet_dir(path: &Path) -> Result<(), CliError> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(CliError::repository_unavailable(
            format!("cannot create {}: {error}", path.display()),
            "Check that the repository root is writable.",
        )),
    }
}

/// Maps root discovery failures to `repository_unavailable` (exit 3).
fn root_error(error: RootError) -> CliError {
    CliError::repository_unavailable(
        error.to_string(),
        "Run `rivet init` to establish a root, or run from inside a repository.",
    )
}

/// Maps config failures: a malformed schema is an argument error (exit 2) whose
/// message names the offending key; I/O and unsafe destinations are exit 3.
fn config_error(error: ConfigError) -> CliError {
    match &error {
        ConfigError::Invalid { .. } => CliError::invalid_arguments(
            error.to_string(),
            "Fix .rivet/config.toml; the message names the offending key.",
        ),
        _ => CliError::repository_unavailable(
            error.to_string(),
            "Fix the .rivet configuration access and retry.",
        ),
    }
}

/// Maps traversal failures to `repository_unavailable` (exit 3).
fn walk_error(error: WalkError) -> CliError {
    CliError::repository_unavailable(
        error.to_string(),
        "Check the repository permissions and retry.",
    )
}

/// Maps store open/publish failures to `repository_unavailable` (exit 3),
/// including an incompatible or locked index.
fn store_error(error: rivet_store::Error) -> CliError {
    let hint = match &error {
        rivet_store::Error::IncompatibleIndexFormat { .. } => {
            "Delete .rivet/index.db and re-run the index."
        }
        rivet_store::Error::InvalidDestination { .. } => {
            "Check that .rivet/ is a real writable directory, not a symlink."
        }
        _ => "Check .rivet/ permissions and retry.",
    };
    CliError::repository_unavailable(error.to_string(), hint)
}
