//! `rivet refs <query>` (spec §11; OUTPUT-CONTRACT "Pagination and resolution",
//! "Ordering and versioning", and the `rivet refs` block).
//!
//! [`run`] follows the same pipeline as [`crate::symbol`]: validate every
//! argument before any filesystem work, discover the root and load the config,
//! validate the configured languages, refresh (or answer from the committed
//! snapshot for `--no-refresh`), resolve the query to exactly one declaration,
//! and only then assemble the reference list.
//!
//! T23 implements the two reference modes:
//!
//! - `references` (the default) keeps uses bound to the queried declaration plus
//!   same-name uses that are unresolved, and drops uses bound elsewhere;
//! - `candidates` adds every extracted use whose normalized unqualified name
//!   matches the target, reporting a use bound elsewhere as query-relative
//!   `name_match` while preserving its real `resolved_target`.
//!
//! The selection, resolution, ordering, dedupe, counting, and slicing live in
//! [`crate::references`] so `rivet symbol`'s call lists (T24) reuse exactly the
//! same machinery. `name_match` is computed at query time from the relationship
//! between a use's stored binding and the queried target; it is never written
//! to the store, and the `bindings` table rejects it by schema.

use std::collections::HashSet;

use rivet_core::{RefKind, Resolution};
use rivet_index::{QueryOutcome, resolve_query, suggestions};
use rivet_store::{Store, SymbolRow};
use serde_json::{Map, Value, json};

use crate::index;
use crate::references::{self, Mode};
use crate::refresh::{acquire_snapshot, open_context};
use crate::symbol::{self, symbol_object};
use crate::transport::CliError;

/// Options accepted by `rivet refs`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--mode references|candidates`.
    pub mode: Option<String>,
    /// `--kind a,b`: comma-separated `ref_kind` values, or every kind.
    pub kind: Option<String>,
    /// `--min-resolution exact|scoped|name_match`.
    pub min_resolution: Option<String>,
    /// `--limit N`: reference page size.
    pub limit: Option<u64>,
    /// `--offset N`: reference page offset.
    pub offset: Option<u64>,
    /// `--freshness content|metadata`: override the configured freshness.
    pub freshness: Option<String>,
    /// `--no-refresh`: answer from the committed snapshot without refreshing.
    pub no_refresh: bool,
}

/// Runs one `rivet refs` and returns the success object (without the
/// transport-prepended `schema_version`), or a documented failure.
pub fn run(query: &str, options: Options) -> Result<Value, CliError> {
    let Options {
        mode,
        kind,
        min_resolution,
        limit,
        offset,
        freshness,
        no_refresh,
    } = options;

    // Argument validation happens before any filesystem work (spec §27;
    // OUTPUT-CONTRACT "Flag applicability").
    if no_refresh && freshness.is_some() {
        return Err(CliError::invalid_arguments(
            "`--no-refresh` cannot be combined with `--freshness`",
            "Drop `--freshness` when answering from the committed snapshot.",
        ));
    }
    let mode = parse_mode(mode.as_deref())?;
    let kinds = parse_kinds(kind.as_deref())?;
    let minimum = references::parse_min_resolution(min_resolution.as_deref())?;
    let limit = symbol::parse_limit(limit)?;
    let offset = offset.unwrap_or(0);
    let requested_freshness = index::parse_freshness(freshness.as_deref())?;

    // Refresh first so references describe the current working tree, then read
    // from the same committed snapshot. With `--no-refresh`, open the committed
    // snapshot directly and never walk.
    let context = open_context()?;
    index::validate_configured_languages(&context.config)?;
    let (store, report) = acquire_snapshot(&context, no_refresh, requested_freshness)?;

    // A snapshot is acquired from here on, so an index-dependent failure
    // carries its `index` (OUTPUT-CONTRACT "Errors").
    answer(
        &store,
        &report,
        query,
        Query {
            mode,
            kinds,
            minimum,
            limit,
            offset,
        },
    )
    .map_err(|error| error.with_snapshot_index(index::index_metadata(&report)))
}

/// The validated query options applied to an acquired snapshot.
struct Query {
    mode: Mode,
    kinds: Option<HashSet<RefKind>>,
    minimum: Resolution,
    limit: u64,
    offset: u64,
}

/// Resolves `query` against the acquired snapshot and builds the success
/// object or the documented failure.
fn answer(
    store: &Store,
    report: &index::Report,
    query: &str,
    options: Query,
) -> Result<Value, CliError> {
    let Query {
        mode,
        kinds,
        minimum,
        limit,
        offset,
    } = options;

    let target = resolve_target(store, query, limit, offset)?;

    // A stored `name_match` does not exist, so the relationship between each
    // use's binding and the target is computed here. `bindings` is hashed by
    // use_id for lookup; its iteration order never reaches output because the
    // reference list is explicitly sorted.
    let bindings = references::bindings_by_use_id(store)?;
    let all = references::all_uses(store)?;
    let matches = references::collect_matches(
        &all,
        &bindings,
        &target,
        references::Selection::Query(mode),
        kinds.as_ref(),
        minimum,
    );

    let mut exact = 0_u64;
    let mut scoped = 0_u64;
    let mut name_match = 0_u64;
    for reference in &matches {
        match reference.resolution {
            Resolution::Exact => exact += 1,
            Resolution::Scoped => scoped += 1,
            Resolution::NameMatch => name_match += 1,
        }
    }

    // Count, then slice (OUTPUT-CONTRACT "Pagination and resolution").
    // `by_resolution` counts every filtered match, before pagination;
    // `truncated` also covers earlier pages.
    let page = references::paginate(&matches, limit, offset);

    let references: Vec<Value> = page
        .items
        .iter()
        .map(|reference| references::reference_object(store, reference))
        .collect::<Result<_, _>>()?;

    // Keys are inserted in contract order.
    let mut object = Map::new();
    object.insert("index".to_string(), index::index_metadata(report));
    object.insert(
        "symbol".to_string(),
        symbol_object(store, &target).map_err(index::store_error)?,
    );
    object.insert("mode".to_string(), json!(mode.as_str()));
    object.insert("total".to_string(), json!(page.total));
    object.insert("truncated".to_string(), json!(page.truncated));
    object.insert("next_offset".to_string(), json!(page.next_offset));
    object.insert(
        "by_resolution".to_string(),
        json!({"exact": exact, "scoped": scoped, "name_match": name_match}),
    );
    object.insert("references".to_string(), Value::Array(references));
    Ok(Value::Object(object))
}

/// Resolves `query` to exactly one declaration of the acquired snapshot,
/// handling every [`QueryOutcome`] arm the same way `rivet symbol` does.
///
/// Shared by `refs` and `context`. No match is `symbol_not_found` with
/// suggestions; several are `ambiguous_symbol` with the candidate page at
/// `limit`/`offset`.
pub(crate) fn resolve_target(
    store: &Store,
    query: &str,
    limit: u64,
    offset: u64,
) -> Result<SymbolRow, CliError> {
    match resolve_query(store, query).map_err(index::store_error)? {
        QueryOutcome::Symbols(matches) => match matches.len() {
            0 => {
                let suggestions = suggestions(store, query).map_err(index::store_error)?;
                Err(CliError::symbol_not_found(
                    format!("query '{query}' matched no symbols"),
                    "Check the spelling, or use a qualified name or canonical ID.",
                    suggestions,
                ))
            }
            1 => Ok(matches.into_iter().next().expect("exactly one match")),
            total => symbol::ambiguous(store, &matches, query, total, limit, offset)
                .map(|_| unreachable!("`ambiguous` always returns the failure")),
        },
        QueryOutcome::PathNotIndexed { path } => Err(CliError::symbol_not_found(
            format!("query '{query}' matched no symbols"),
            format!(
                "{path} is not indexed or is excluded; run `rivet index` and check exclusions."
            ),
            Vec::new(),
        )),
        QueryOutcome::NoEnclosingSymbol { suggestions } => Err(CliError::symbol_not_found(
            format!("no symbol encloses {query}"),
            "Pick the nearest symbol, or query it by name.",
            suggestions,
        )),
        QueryOutcome::InvalidFileLine { path, reason } => Err(CliError::invalid_arguments(
            format!("invalid file:line query for '{path}': {reason}"),
            "Use a repository-relative path with `/` separators and no `..`.",
        )),
    }
}

/// Parses `--mode`, rejecting unknown values before any filesystem work.
fn parse_mode(value: Option<&str>) -> Result<Mode, CliError> {
    match value {
        None | Some("references") => Ok(Mode::References),
        Some("candidates") => Ok(Mode::Candidates),
        Some(other) => Err(CliError::invalid_arguments(
            format!(
                "invalid value for `--mode`: {other:?} (expected \"references\" or \"candidates\")"
            ),
            "Pass `--mode references` or `--mode candidates`.",
        )),
    }
}

/// Parses the comma-separated `--kind` list, rejecting unknown values before
/// any filesystem work. `None` means every kind.
fn parse_kinds(value: Option<&str>) -> Result<Option<HashSet<RefKind>>, CliError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut kinds = HashSet::new();
    for raw in value.split(',') {
        let name = raw.trim();
        let kind = name.parse::<RefKind>().map_err(|_| {
            CliError::invalid_arguments(
                format!(
                    "invalid value for `--kind`: {name:?} (expected a comma-separated list of ref_kind values)"
                ),
                "Pass `--kind call,type` using call, type, import, assignment, read, write, or unknown.",
            )
        })?;
        kinds.insert(kind);
    }
    Ok(Some(kinds))
}

/// The compact human rendering of a successful reference lookup.
pub fn human(value: &Value) -> String {
    let symbol = &value["symbol"];
    let mut output = format!(
        "{} references to {} ({})\n",
        value["total"].as_u64().unwrap_or(0),
        symbol["qualified_name"].as_str().unwrap_or(""),
        value["mode"].as_str().unwrap_or("references"),
    );
    if let Some(items) = value["references"].as_array() {
        for item in items {
            // Human output marks `name_match` results with `?` (spec §11.3).
            let mark = if item["resolution"] == "name_match" {
                "?"
            } else {
                " "
            };
            output.push_str(&format!(
                "{mark}{}:{}:{}  {}\n",
                item["file"].as_str().unwrap_or(""),
                item["line"].as_u64().unwrap_or(0),
                item["column"].as_u64().unwrap_or(0),
                item["ref_kind"].as_str().unwrap_or(""),
            ));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{Mode, parse_kinds, parse_mode};
    use crate::references::parse_min_resolution;
    use rivet_core::{RefKind, Resolution};

    #[test]
    fn arguments_are_validated_before_work() {
        assert_eq!(parse_mode(None).unwrap(), Mode::References);
        assert_eq!(parse_mode(Some("references")).unwrap(), Mode::References);
        assert_eq!(parse_mode(Some("candidates")).unwrap(), Mode::Candidates);
        let error = parse_mode(Some("everything")).unwrap_err();
        assert_eq!((error.code, error.exit), ("invalid_arguments", 2));

        assert_eq!(parse_min_resolution(None).unwrap(), Resolution::NameMatch);
        assert_eq!(
            parse_min_resolution(Some("scoped")).unwrap(),
            Resolution::Scoped
        );
        let error = parse_min_resolution(Some("strong")).unwrap_err();
        assert_eq!((error.code, error.exit), ("invalid_arguments", 2));

        assert!(parse_kinds(None).unwrap().is_none());
        let kinds = parse_kinds(Some("call, type")).unwrap().unwrap();
        assert!(kinds.contains(&RefKind::Call));
        assert!(kinds.contains(&RefKind::Type));
        let error = parse_kinds(Some("call,bogus")).unwrap_err();
        assert_eq!((error.code, error.exit), ("invalid_arguments", 2));
    }
}
