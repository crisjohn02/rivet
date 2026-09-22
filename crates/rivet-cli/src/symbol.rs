//! `rivet symbol <query>` (spec §10; OUTPUT-CONTRACT "Coordinates and symbol
//! objects", "Pagination and resolution", and the `rivet symbol` block).
//!
//! T12 resolves canonical IDs, native qualified names, dotted paths, and short
//! names against stored symbol rows after a refresh; T13 adds `file:line` and
//! deterministic ambiguity pagination; T14 adds `--source` from the stored
//! `files.source` bytes plus persisted `signature`/`doc_comment`. Real call
//! lists arrive in T24.

use serde_json::{Map, Value, json};

use rivet_index::{QueryOutcome, resolve_query, suggestions};
use rivet_store::{Store, SymbolRow};

use crate::index;
use crate::refresh::{cached_report, open_cached_store, open_context, open_store, refresh};
use crate::transport::CliError;

/// The default `--limit` and its documented maximum (OUTPUT-CONTRACT
/// "Pagination and resolution").
const DEFAULT_LIMIT: u64 = 50;
/// The largest accepted `--limit`.
const MAX_LIMIT: u64 = 1000;

/// Options accepted by `rivet symbol`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--limit N`: candidate page size.
    pub limit: Option<u64>,
    /// `--offset N`: candidate page offset.
    pub offset: Option<u64>,
    /// `--signature-only`: omit the (currently empty) call lists.
    pub signature_only: bool,
    /// `--source`: include the symbol's source slice from stored bytes.
    pub source: bool,
    /// `--freshness content|metadata`: override the configured freshness.
    pub freshness: Option<String>,
    /// `--no-refresh`: answer from the committed snapshot without refreshing.
    pub no_refresh: bool,
}

/// Runs one `rivet symbol` and returns the success object (without the
/// transport-prepended `schema_version`), or a documented failure.
pub fn run(query: &str, options: Options) -> Result<Value, CliError> {
    let Options {
        limit,
        offset,
        signature_only,
        source,
        freshness,
        no_refresh,
    } = options;

    // Argument validation happens before any filesystem work (spec §27).
    if source && signature_only {
        return Err(CliError::invalid_arguments(
            "`--source` cannot be combined with `--signature-only`",
            "Pass only one of `--source` or `--signature-only`.",
        ));
    }
    if no_refresh && freshness.is_some() {
        return Err(CliError::invalid_arguments(
            "`--no-refresh` cannot be combined with `--freshness`",
            "Drop `--freshness` when answering from the committed snapshot.",
        ));
    }
    let limit = parse_limit(limit)?;
    let offset = offset.unwrap_or(0);
    // Parse `--freshness` before any filesystem work (spec §27).
    let requested_freshness = index::parse_freshness(freshness.as_deref())?;

    // Refresh first so query results describe the current working tree, then
    // read every row from the committed snapshot in the same store. With
    // `--no-refresh`, open the committed snapshot directly and never walk.
    let context = open_context()?;
    index::validate_configured_languages(&context.config)?;
    let (store, report) = if no_refresh {
        let store = open_cached_store(&context.root)?;
        let report = cached_report(&store, false)?;
        (store, report)
    } else {
        let effective_freshness = requested_freshness.unwrap_or(context.config.index.freshness);
        let mode = index::refresh_mode(effective_freshness);
        let mut store = open_store(&context.root)?;
        let report = refresh(&context.root.root, &context.config, &mut store, mode, false)?.report;
        (store, report)
    };
    let matches = match resolve_query(&store, query).map_err(index::store_error)? {
        QueryOutcome::Symbols(matches) => matches,
        QueryOutcome::PathNotIndexed { path } => {
            return Err(CliError::symbol_not_found(
                format!("query '{query}' matched no symbols"),
                format!(
                    "{path} is not indexed or is excluded; run `rivet index` and check exclusions."
                ),
                Vec::new(),
            ));
        }
        QueryOutcome::NoEnclosingSymbol { suggestions } => {
            return Err(CliError::symbol_not_found(
                format!("no symbol encloses {query}"),
                "Pick the nearest symbol, or query it by name.",
                suggestions,
            ));
        }
        QueryOutcome::InvalidFileLine { path, reason } => {
            return Err(CliError::invalid_arguments(
                format!("invalid file:line query for '{path}': {reason}"),
                "Use a repository-relative path with `/` separators and no `..`.",
            ));
        }
    };

    match matches.len() {
        0 => {
            let suggestions = suggestions(&store, query).map_err(index::store_error)?;
            Err(CliError::symbol_not_found(
                format!("query '{query}' matched no symbols"),
                "Check the spelling, or use a qualified name or canonical ID.",
                suggestions,
            ))
        }
        1 => single(
            &store,
            &report,
            &matches[0],
            SuccessOptions {
                source,
                signature_only,
            },
        ),
        total => ambiguous(&store, &matches, query, total, limit, offset),
    }
}

/// The two output-shaping options for the single-symbol success object.
struct SuccessOptions {
    source: bool,
    signature_only: bool,
}

/// Builds the success object for a unique match.
fn single(
    store: &Store,
    report: &index::Report,
    row: &SymbolRow,
    options: SuccessOptions,
) -> Result<Value, CliError> {
    let mut object = Map::new();
    object.insert("index".to_string(), index::index_metadata(report));
    object.insert(
        "symbol".to_string(),
        symbol_object(store, row).map_err(index::store_error)?,
    );
    if options.source {
        object.insert(
            "source".to_string(),
            Value::String(source_slice(store, row)?),
        );
    }
    object.insert(
        "signature".to_string(),
        row.signature
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    object.insert(
        "doc_comment".to_string(),
        row.doc_comment
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    object.insert(
        "parent".to_string(),
        row.parent_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    if !options.signature_only {
        object.insert("calls".to_string(), empty_call_list());
        object.insert("called_by".to_string(), empty_call_list());
    }
    // T24 replaces this note once call/caller lists are real.
    object.insert(
        "development_note".to_string(),
        json!("calls and called_by are not implemented until T24"),
    );
    Ok(Value::Object(object))
}

/// Builds the `ambiguous_symbol` failure with a paginated candidate page.
///
/// Shared with `refs`, which reports ambiguity exactly as `symbol` does.
pub(crate) fn ambiguous(
    store: &Store,
    matches: &[SymbolRow],
    query: &str,
    total: usize,
    limit: u64,
    offset: u64,
) -> Result<Value, CliError> {
    let total = total as u64;
    let offset_index = usize::try_from(offset).unwrap_or(usize::MAX);
    let page: Vec<Value> = matches
        .iter()
        .skip(offset_index)
        .take(limit as usize)
        .map(|row| symbol_object(store, row).map_err(index::store_error))
        .collect::<Result<_, _>>()?;
    let page_len = page.len() as u64;
    // "truncated" covers matches outside this page, including earlier pages.
    let truncated = total > page_len;
    let next_offset = if offset + page_len < total {
        Some(offset + page_len)
    } else {
        None
    };
    Err(CliError::ambiguous_symbol(
        format!("query '{query}' matched {total} symbols"),
        "Re-run with one of the returned canonical IDs.",
        total,
        truncated,
        next_offset,
        page,
    ))
}

/// Builds one full symbol object (OUTPUT-CONTRACT "Coordinates and symbol
/// objects").
///
/// Shared with `refs` for both the queried `symbol` and each reference's
/// `containing_symbol`.
pub(crate) fn symbol_object(store: &Store, row: &SymbolRow) -> Result<Value, rivet_store::Error> {
    let file = store.get_file(&row.file)?;
    let content_hash = file.as_ref().and_then(|file| file.content_hash.clone());
    let language = file.and_then(|file| file.language);
    Ok(json!({
        "id": row.id,
        "name": row.name,
        "qualified_name": row.qualified_name,
        "kind": row.kind.as_str(),
        "language": language,
        "file": row.file,
        "start_byte": row.start_byte,
        "end_byte": row.end_byte,
        "start_line": row.start_line,
        "end_line": row.end_line,
        "content_hash": content_hash,
    }))
}

/// The symbol's span slice from the file's stored bytes (never the live file).
///
/// `--source` promises a string, so a missing stored file, an empty `source`
/// column, or a span outside the stored bytes is a failure rather than a null
/// or empty result that would look complete.
fn source_slice(store: &Store, row: &SymbolRow) -> Result<String, CliError> {
    let bytes = store
        .get_file(&row.file)
        .map_err(index::store_error)?
        .and_then(|file| file.source)
        .ok_or_else(|| {
            index::store_error(rivet_store::Error::Configuration {
                detail: format!("no stored source bytes for {}", row.file),
            })
        })?;
    let slice = bytes
        .get(row.start_byte as usize..row.end_byte as usize)
        .ok_or_else(|| {
            index::store_error(rivet_store::Error::Configuration {
                detail: format!(
                    "span [{}, {}) is outside the {} stored bytes of {}",
                    row.start_byte,
                    row.end_byte,
                    bytes.len(),
                    row.file
                ),
            })
        })?;
    Ok(String::from_utf8_lossy(slice).into_owned())
}

/// An empty call/caller list while T24 is pending.
fn empty_call_list() -> Value {
    json!({"total": 0, "truncated": false, "next_offset": null, "items": []})
}

/// Validates `--limit` before any filesystem work.
///
/// Shared with `refs`, whose page size has the same range and default.
pub(crate) fn parse_limit(limit: Option<u64>) -> Result<u64, CliError> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(value) if value == 0 || value > MAX_LIMIT => Err(CliError::invalid_arguments(
            format!("invalid value for `--limit`: {value} (expected 1-{MAX_LIMIT})"),
            "Pass `--limit` between 1 and 1000.",
        )),
        Some(value) => Ok(value),
    }
}

/// The compact human rendering of a successful symbol lookup.
pub fn human(value: &Value) -> String {
    let symbol = &value["symbol"];
    format!(
        "{}  {}:{}-{}  {}\n",
        symbol["qualified_name"].as_str().unwrap_or(""),
        symbol["file"].as_str().unwrap_or(""),
        symbol["start_line"].as_u64().unwrap_or(0),
        symbol["end_line"].as_u64().unwrap_or(0),
        symbol["kind"].as_str().unwrap_or(""),
    )
}

#[cfg(test)]
mod tests {
    use super::parse_limit;

    #[test]
    fn limit_is_validated_before_work() {
        assert_eq!(parse_limit(None).unwrap(), 50);
        assert_eq!(parse_limit(Some(1)).unwrap(), 1);
        assert_eq!(parse_limit(Some(1000)).unwrap(), 1000);
        assert!(parse_limit(Some(0)).is_err());
        assert!(parse_limit(Some(1001)).is_err());
        let error = parse_limit(Some(0)).unwrap_err();
        assert_eq!((error.code, error.exit), ("invalid_arguments", 2));
    }
}
