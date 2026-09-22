//! `rivet context <query>` (spec §16 and §16.5; OUTPUT-CONTRACT "Flag
//! applicability" and the `rivet context` block; T30).
//!
//! The module is named `context_cmd` because [`crate::context`] already holds
//! candidate collection (T26/T27); this module is only the command wiring.
//!
//! [`run`] follows the same pipeline as [`crate::symbol`] and [`crate::refs`]:
//! validate every argument before any filesystem work, discover the root and
//! load the config, validate the configured languages, refresh (or answer from
//! the committed snapshot for `--no-refresh`), and resolve the query to
//! exactly one declaration. It then collects ranked candidates with
//! [`crate::context::collect_ranked`], fits them with
//! [`crate::budget::fit_collection`], and renders the contract's JSON. Neither
//! collection nor fitting is re-implemented here.

use rivet_core::{Collapse, Config};
use rivet_store::Store;
use serde_json::{Map, Value, json};

use crate::budget::{self, ContextFit, TOKENIZER};
use crate::context::{self, ContextOptions};
use crate::index;
use crate::refresh::{acquire_snapshot, open_context};
use crate::refs::resolve_target;
use crate::symbol::{self, symbol_object};
use crate::transport::CliError;

/// The largest accepted `--tokens` (OUTPUT-CONTRACT `rivet context`).
pub const MAX_TOKENS: u64 = 1_000_000;

/// The contract's `budget_scope` value: the budget bounds source only.
pub const BUDGET_SCOPE: &str = "source";

/// Options accepted by `rivet context`, as parsed from the command line.
///
/// Each `include`/`exclude` pair is two independent flags so that passing
/// both is detected and rejected rather than one silently winning.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--tokens N`: the source-token budget.
    pub tokens: Option<u64>,
    /// `--depth 1|2`.
    pub depth: Option<u64>,
    /// `--collapse auto|always|never`.
    pub collapse: Option<String>,
    /// `--include-tests`.
    pub include_tests: bool,
    /// `--exclude-tests`.
    pub exclude_tests: bool,
    /// `--include-callers`.
    pub include_callers: bool,
    /// `--exclude-callers`.
    pub exclude_callers: bool,
    /// `--include-callees`.
    pub include_callees: bool,
    /// `--exclude-callees`.
    pub exclude_callees: bool,
    /// `--limit N`: the segment count, the target included.
    pub limit: Option<u64>,
    /// `--offset N`: accepted by the parser only so it can be rejected with
    /// `invalid_arguments`; context does not paginate.
    pub offset: Option<u64>,
    /// `--freshness content|metadata`: override the configured freshness.
    pub freshness: Option<String>,
    /// `--no-refresh`: answer from the committed snapshot without refreshing.
    pub no_refresh: bool,
}

/// The validated arguments; `None` fields fall back to configuration.
struct Validated {
    tokens: Option<u64>,
    depth: Option<u8>,
    collapse: Option<Collapse>,
    include_tests: Option<bool>,
    include_callers: bool,
    include_callees: bool,
    limit: u64,
}

/// Runs one `rivet context` and returns the success object (without the
/// transport-prepended `schema_version`), or a documented failure.
pub fn run(query: &str, options: Options) -> Result<Value, CliError> {
    // Argument validation happens before any filesystem work (spec §27;
    // OUTPUT-CONTRACT "Flag applicability").
    let validated = validate(&options)?;
    let requested_freshness = index::parse_freshness(options.freshness.as_deref())?;

    let context = open_context()?;
    index::validate_configured_languages(&context.config)?;
    // The configured default budget is only known once the config is loaded;
    // it is checked before the store is opened.
    let budget_tokens = match validated.tokens {
        Some(tokens) => tokens,
        None => configured_budget(&context.config)?,
    };
    let (store, report) = acquire_snapshot(&context, options.no_refresh, requested_freshness)?;

    // A snapshot is acquired from here on, so an index-dependent failure
    // (`symbol_not_found`, `ambiguous_symbol`, `budget_too_small`) carries its
    // `index` (OUTPUT-CONTRACT "Errors").
    answer(
        &store,
        &report,
        &context.config,
        query,
        &validated,
        budget_tokens,
    )
    .map_err(|error| error.with_snapshot_index(index::index_metadata(&report)))
}

/// Validates every command-line argument without touching the filesystem.
fn validate(options: &Options) -> Result<Validated, CliError> {
    if options.offset.is_some() {
        return Err(CliError::invalid_arguments(
            "`--offset` is not supported by `rivet context`",
            "Drop `--offset`; context returns segments in rank order and uses `--limit` for \
             the segment count.",
        ));
    }
    if options.no_refresh && options.freshness.is_some() {
        return Err(CliError::invalid_arguments(
            "`--no-refresh` cannot be combined with `--freshness`",
            "Drop `--freshness` when answering from the committed snapshot.",
        ));
    }
    let include_tests = exclusive_pair(options.include_tests, options.exclude_tests, "tests")?;
    let include_callers =
        exclusive_pair(options.include_callers, options.exclude_callers, "callers")?;
    let include_callees =
        exclusive_pair(options.include_callees, options.exclude_callees, "callees")?;
    let tokens = options.tokens.map(parse_tokens).transpose()?;
    let depth = options.depth.map(parse_depth).transpose()?;
    let collapse = options
        .collapse
        .as_deref()
        .map(parse_collapse)
        .transpose()?;
    // The segment limit has the common `--limit` range and default.
    let limit = symbol::parse_limit(options.limit)?;
    Ok(Validated {
        tokens,
        depth,
        collapse,
        include_tests,
        include_callers: include_callers.unwrap_or(true),
        include_callees: include_callees.unwrap_or(true),
        limit,
    })
}

/// Resolves the query against the acquired snapshot, collects and fits the
/// context, and builds the success object or the documented failure.
fn answer(
    store: &Store,
    report: &index::Report,
    config: &Config,
    query: &str,
    validated: &Validated,
    budget_tokens: u64,
) -> Result<Value, CliError> {
    // Context rejects `--offset`, so an ambiguity page always starts at zero
    // and uses the supplied `--limit` as its page size.
    let target = resolve_target(store, query, validated.limit, 0).map_err(point_to_symbol)?;

    let defaults = ContextOptions::from_config(&config.context);
    let traversal = ContextOptions {
        depth: validated.depth.unwrap_or(defaults.depth),
        include_tests: validated.include_tests.unwrap_or(defaults.include_tests),
        include_callers: validated.include_callers,
        include_callees: validated.include_callees,
        ..defaults
    };
    let collapse = validated.collapse.unwrap_or(config.context.collapse);
    let collection = context::collect_ranked(store, &target, &config.context, &traversal)?;
    // `limit` is at most 1,000, so the conversion cannot fail.
    let limit = usize::try_from(validated.limit).unwrap_or(usize::MAX);
    let fitted = budget::fit_collection(store, &collection, collapse, budget_tokens, limit)?;

    success_object(store, report, &fitted)
}

/// Renders the fitted context with keys in the contract's order.
fn success_object(
    store: &Store,
    report: &index::Report,
    fitted: &ContextFit,
) -> Result<Value, CliError> {
    let target = fitted
        .segments
        .first()
        .expect("a successful fit always emits the target first");

    let segments: Vec<Value> = fitted
        .segments
        .iter()
        .map(|segment| {
            let mut object = Map::new();
            object.insert(
                "symbol".to_string(),
                symbol_object(store, &segment.candidate.symbol).map_err(index::store_error)?,
            );
            object.insert("form".to_string(), json!(segment.form.as_str()));
            object.insert(
                "reason".to_string(),
                json!(segment.candidate.reason.as_str()),
            );
            object.insert(
                "resolution".to_string(),
                json!(segment.candidate.resolution.as_str()),
            );
            object.insert(
                "estimated_tokens".to_string(),
                json!(segment.estimated_tokens),
            );
            object.insert("source".to_string(), json!(segment.source));
            Ok(Value::Object(object))
        })
        .collect::<Result<_, CliError>>()?;

    // `omitted` keys follow the contract's shape (budget, overlap, limit),
    // not the assignment precedence (overlap, limit, budget).
    let mut omitted = Map::new();
    omitted.insert("budget".to_string(), json!(fitted.omitted.budget));
    omitted.insert("overlap".to_string(), json!(fitted.omitted.overlap));
    omitted.insert("limit".to_string(), json!(fitted.omitted.limit));

    let mut object = Map::new();
    object.insert("index".to_string(), index::index_metadata(report));
    object.insert(
        "symbol".to_string(),
        symbol_object(store, &target.candidate.symbol).map_err(index::store_error)?,
    );
    object.insert("budget_tokens".to_string(), json!(fitted.budget_tokens));
    object.insert(
        "estimated_tokens".to_string(),
        json!(fitted.estimated_tokens),
    );
    object.insert("tokenizer".to_string(), json!(TOKENIZER));
    object.insert("budget_scope".to_string(), json!(BUDGET_SCOPE));
    object.insert("segments".to_string(), Value::Array(segments));
    object.insert("omitted".to_string(), Value::Object(omitted));
    object.insert(
        "candidate_limit_reached".to_string(),
        json!(fitted.candidate_limit_reached),
    );
    Ok(Value::Object(object))
}

/// Points an ambiguity failure at `rivet symbol` for further pages, since
/// context rejects `--offset` (OUTPUT-CONTRACT "Errors").
fn point_to_symbol(mut error: CliError) -> CliError {
    if error.code == "ambiguous_symbol" {
        error.hint = "Re-run with one of the returned canonical IDs; use `rivet symbol` with \
                      `--offset` to page through further candidates."
            .to_string();
    }
    error
}

/// Resolves an `--include-X`/`--exclude-X` pair: `None` when neither is
/// given, and `invalid_arguments` when both are.
fn exclusive_pair(include: bool, exclude: bool, name: &str) -> Result<Option<bool>, CliError> {
    match (include, exclude) {
        (true, true) => Err(CliError::invalid_arguments(
            format!("`--include-{name}` cannot be combined with `--exclude-{name}`"),
            format!("Pass only one of `--include-{name}` or `--exclude-{name}`."),
        )),
        (true, false) => Ok(Some(true)),
        (false, true) => Ok(Some(false)),
        (false, false) => Ok(None),
    }
}

/// Validates `--tokens`: a positive integer up to [`MAX_TOKENS`].
fn parse_tokens(value: u64) -> Result<u64, CliError> {
    if value == 0 || value > MAX_TOKENS {
        return Err(CliError::invalid_arguments(
            format!("invalid value for `--tokens`: {value} (expected 1-{MAX_TOKENS})"),
            "Pass `--tokens` between 1 and 1000000.",
        ));
    }
    Ok(value)
}

/// Validates `--depth`: 1 or 2.
fn parse_depth(value: u64) -> Result<u8, CliError> {
    match value {
        1 => Ok(1),
        2 => Ok(2),
        other => Err(CliError::invalid_arguments(
            format!("invalid value for `--depth`: {other} (expected 1 or 2)"),
            "Pass `--depth 1` or `--depth 2`.",
        )),
    }
}

/// Validates `--collapse`: `auto`, `always`, or `never`.
fn parse_collapse(value: &str) -> Result<Collapse, CliError> {
    value.parse::<Collapse>().map_err(|()| {
        CliError::invalid_arguments(
            format!(
                "invalid value for `--collapse`: {value:?} (expected \"auto\", \"always\", or \
                 \"never\")"
            ),
            "Pass `--collapse auto`, `--collapse always`, or `--collapse never`.",
        )
    })
}

/// The configured `context.default_token_budget`, held to the same range as
/// `--tokens`; an out-of-range value is a configuration error (exit 2).
fn configured_budget(config: &Config) -> Result<u64, CliError> {
    let value = u64::from(config.context.default_token_budget);
    if value == 0 || value > MAX_TOKENS {
        return Err(CliError::invalid_arguments(
            format!(
                "invalid value for `context.default_token_budget`: {value} (expected \
                 1-{MAX_TOKENS})"
            ),
            "Set `context.default_token_budget` in .rivet/config.toml between 1 and 1000000, \
             or pass `--tokens`.",
        ));
    }
    Ok(value)
}

/// The compact human rendering of a successful context lookup: one line per
/// segment and the estimate summary. Full human formatting is T34.
pub fn human(value: &Value) -> String {
    let mut output = String::new();
    if let Some(segments) = value["segments"].as_array() {
        for segment in segments {
            let symbol = &segment["symbol"];
            output.push_str(&format!(
                "{}:{}-{}  {}  [{}, {}]\n",
                symbol["file"].as_str().unwrap_or(""),
                symbol["start_line"].as_u64().unwrap_or(0),
                symbol["end_line"].as_u64().unwrap_or(0),
                symbol["qualified_name"].as_str().unwrap_or(""),
                segment["reason"].as_str().unwrap_or(""),
                segment["form"].as_str().unwrap_or(""),
            ));
        }
    }
    output.push_str(&format!(
        "estimated_tokens: {} / {}  (tokenizer: {}; budget_scope: {})\n",
        value["estimated_tokens"].as_u64().unwrap_or(0),
        value["budget_tokens"].as_u64().unwrap_or(0),
        value["tokenizer"].as_str().unwrap_or(""),
        value["budget_scope"].as_str().unwrap_or(""),
    ));
    output
}

#[cfg(test)]
mod tests {
    use super::{Options, exclusive_pair, parse_collapse, parse_depth, parse_tokens, validate};
    use rivet_core::Collapse;

    #[test]
    fn values_are_validated_before_work() {
        assert_eq!(parse_tokens(1).unwrap(), 1);
        assert_eq!(parse_tokens(1_000_000).unwrap(), 1_000_000);
        assert!(parse_tokens(0).is_err());
        assert!(parse_tokens(1_000_001).is_err());
        assert_eq!(parse_depth(1).unwrap(), 1);
        assert_eq!(parse_depth(2).unwrap(), 2);
        assert!(parse_depth(0).is_err());
        assert!(parse_depth(3).is_err());
        assert_eq!(parse_collapse("never").unwrap(), Collapse::Never);
        assert!(parse_collapse("Auto").is_err());
        assert_eq!(exclusive_pair(false, false, "tests").unwrap(), None);
        assert_eq!(exclusive_pair(true, false, "tests").unwrap(), Some(true));
        assert_eq!(exclusive_pair(false, true, "tests").unwrap(), Some(false));
        let error = exclusive_pair(true, true, "tests").unwrap_err();
        assert_eq!((error.code, error.exit), ("invalid_arguments", 2));
    }

    #[test]
    fn offset_is_rejected() {
        let options = Options {
            offset: Some(0),
            ..Options::default()
        };
        let error = validate(&options).err().expect("offset is rejected");
        assert_eq!((error.code, error.exit), ("invalid_arguments", 2));
    }
}
