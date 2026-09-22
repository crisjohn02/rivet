//! `rivet index` formatting and options (spec §13; OUTPUT-CONTRACT "Common
//! index metadata" and "Administrative commands").
//!
//! T15 moves the actual refresh into [`crate::refresh`]. This module keeps the
//! command-line options, the thin [`run`] wrapper, and the JSON/human
//! formatters for the shared [`Report`]. [`Report`] and its coverage fields are
//! also consumed by every navigation command, so both `index` and `symbol`
//! format one refresh outcome produced by a single code path.

use serde_json::{Map, Value, json};

use rivet_core::{Config, ConfigError, Freshness, RootError, WalkError};
use rivet_languages::is_language_compiled;

use crate::refresh::{RefreshMode, open_context, open_store, open_store_rebuildable, refresh};
use crate::transport::CliError;

/// Maximum number of diagnostic items emitted; counts stay exhaustive.
pub(crate) const DIAGNOSTIC_CAP: usize = 50;

/// Options accepted by `rivet index`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--force`: reparse every eligible file from disk, reusing no stored fact,
    /// and replace every stored fact in one transaction (AF6).
    pub force: bool,
    /// `--timing`: include `elapsed_ms` in JSON output.
    pub timing: bool,
    /// `--languages a,b`: override the configured enabled languages.
    pub languages: Option<String>,
    /// `--freshness content|metadata`: override the configured freshness.
    pub freshness: Option<String>,
    /// `--no-refresh`: rejected for `index` (queries only).
    pub no_refresh: bool,
}

/// Coverage skip counts, one per non-`ok` parse status.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Skipped {
    pub(crate) unsupported: u64,
    pub(crate) binary: u64,
    pub(crate) size: u64,
    pub(crate) encoding: u64,
    pub(crate) parse_error: u64,
    pub(crate) resource_limit: u64,
}

impl Skipped {
    /// The sum of every skipped count.
    pub(crate) fn total(self) -> u64 {
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
pub(crate) struct DiagnosticItem {
    pub(crate) file: String,
    pub(crate) code: &'static str,
    pub(crate) detail: String,
}

/// The success payload of one refresh.
#[derive(Debug)]
pub struct Report {
    pub(crate) snapshot: String,
    pub(crate) freshness: Freshness,
    pub(crate) complete: bool,
    pub(crate) files_seen: u64,
    pub(crate) files_indexed: u64,
    pub(crate) skipped: Skipped,
    pub(crate) diagnostics_total: usize,
    pub(crate) diagnostics_truncated: bool,
    pub(crate) diagnostics: Vec<DiagnosticItem>,
    pub(crate) symbols: u64,
    pub(crate) uses: u64,
    pub(crate) bindings: u64,
    pub(crate) updated: u64,
    pub(crate) unchanged: u64,
    pub(crate) deleted: u64,
    pub(crate) elapsed_ms: u64,
    pub(crate) timing: bool,
}

/// Runs one `rivet index`.
///
/// Argument validation and config-language validation happen before any
/// filesystem work (spec §27). The refresh itself lives in [`crate::refresh`];
/// this wrapper only decides the effective flags and formats the outcome.
pub fn run(options: Options) -> Result<Report, CliError> {
    let Options {
        force,
        timing,
        languages,
        freshness,
        no_refresh,
    } = options;

    // Argument validation happens before any filesystem work (spec §27).
    if no_refresh {
        return Err(CliError::invalid_arguments(
            "`index` cannot be combined with `--no-refresh`",
            "Run `rivet index` to refresh, or use `--no-refresh` with a query command.",
        ));
    }
    let requested_languages = parse_languages(languages.as_deref())?;
    let requested_freshness = parse_freshness(freshness.as_deref())?;

    let mut context = open_context()?;
    match requested_languages {
        Some(languages) => context.config.languages.enabled = languages,
        None => validate_configured_languages(&context.config)?,
    }
    let effective_freshness = requested_freshness.unwrap_or(context.config.index.freshness);
    let mode = refresh_mode(effective_freshness);

    let mut store = if force {
        open_store_rebuildable(&context.root)?
    } else {
        open_store(&context.root)?
    };
    let outcome = refresh(&context.root.root, &context.config, &mut store, mode, force)?;

    let mut report = outcome.report;
    report.timing = timing;
    Ok(report)
}

/// Maps an effective freshness to the refresh behavior it selects.
///
/// `Cached` never reaches this function: it cannot come from config or
/// `--freshness`, and `--no-refresh` is rejected for `index`.
pub(crate) fn refresh_mode(freshness: Freshness) -> RefreshMode {
    match freshness {
        Freshness::Metadata => RefreshMode::Metadata,
        Freshness::Content | Freshness::Cached => RefreshMode::Content,
    }
}

/// Builds the top-level `index --json` object (without `schema_version`, which
/// the transport prepends).
pub fn success_json(report: &Report) -> Value {
    let mut object = Map::new();
    object.insert("index".to_string(), index_metadata(report));
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

/// Builds the shared `index` metadata object used by every navigation command
/// (OUTPUT-CONTRACT "Common index metadata").
pub fn index_metadata(report: &Report) -> Value {
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
    json!({
        "snapshot": report.snapshot,
        "freshness": report.freshness.as_str(),
        "coverage": coverage,
        "diagnostics": diagnostics,
    })
}

/// The human `rivet index` output (spec §13's possible output), plus the
/// deleted count and the coverage notes when coverage is not complete.
///
/// "Indexed" counts files with stored facts; the coverage line reports how
/// many were seen and why the rest were skipped.
pub fn human(report: &Report) -> String {
    let mut output = format!(
        "Indexed {} files\n{} symbols\n{} relationships\n\nUpdated: {}\nUnchanged: {}\nDeleted: {}\n\nElapsed: {} ms\n",
        grouped(report.files_indexed),
        grouped(report.symbols),
        grouped(report.uses),
        grouped(report.updated),
        grouped(report.unchanged),
        grouped(report.deleted),
        grouped(report.elapsed_ms),
    );
    let notes = crate::human::index_notes(&index_metadata(report));
    if !notes.is_empty() {
        output.push('\n');
        output.push_str(&notes);
    }
    output
}

/// `value` with `,` between digit groups (`4821` -> `4,821`), independent of
/// locale.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut output = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(digit);
    }
    output
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
pub(crate) fn parse_freshness(value: Option<&str>) -> Result<Option<Freshness>, CliError> {
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
pub(crate) fn validate_configured_languages(config: &Config) -> Result<(), CliError> {
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

/// Maps root discovery failures to `repository_unavailable` (exit 3).
pub(crate) fn root_error(error: RootError) -> CliError {
    CliError::repository_unavailable(
        error.to_string(),
        "Run `rivet init` to establish a root, or run from inside a repository.",
    )
}

/// Maps config failures: a malformed schema is an argument error (exit 2) whose
/// message names the offending key; I/O and unsafe destinations are exit 3.
pub(crate) fn config_error(error: ConfigError) -> CliError {
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
pub(crate) fn walk_error(error: WalkError) -> CliError {
    CliError::repository_unavailable(
        error.to_string(),
        "Check the repository permissions and retry.",
    )
}

/// Maps store open/publish failures to `repository_unavailable` (exit 3),
/// including an incompatible or locked index.
pub(crate) fn store_error(error: rivet_store::Error) -> CliError {
    let hint = match &error {
        rivet_store::Error::IncompatibleIndexFormat { .. } => {
            "Run `rivet index --force` to rebuild the disposable cache."
        }
        rivet_store::Error::InvalidDestination { .. } => {
            "Check that .rivet/ is a real writable directory, not a symlink."
        }
        rivet_store::Error::WriterLocked { .. } => {
            "Another rivet process is refreshing this index; retry when it finishes."
        }
        _ => "Check .rivet/ permissions and retry.",
    };
    CliError::repository_unavailable(error.to_string(), hint)
}

#[cfg(test)]
mod tests {
    use super::grouped;

    #[test]
    fn counts_group_digits_without_locale() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(4821), "4,821");
        assert_eq!(grouped(32418), "32,418");
        assert_eq!(grouped(1_000_000), "1,000,000");
    }
}
