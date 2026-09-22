//! Shared JSON success/error transport (OUTPUT-CONTRACT "Transport and common
//! rules").
//!
//! Success writes exactly one compact UTF-8 JSON object followed by a newline to
//! stdout and nothing to stderr. Failure writes exactly one compact JSON error
//! object followed by a newline to stderr, leaves stdout empty, and terminates
//! the process with the documented exit code. Every object's first key is
//! `schema_version`, so the map is rebuilt with that key inserted first; the
//! `preserve_order` feature keeps every later key in insertion order.

use std::io::{self, Write};

use serde_json::{Map, Value, json};

/// The fixed `schema_version` for every success and error object.
pub const SCHEMA_VERSION: i64 = 1;

/// Exit codes from OUTPUT-CONTRACT "Errors".
pub const EXIT_GENERAL: i32 = 1;
/// Invalid arguments or configuration.
pub const EXIT_INVALID_ARGUMENTS: i32 = 2;
/// Missing root, lock, I/O, or incompatible-cache failure.
pub const EXIT_REPOSITORY_UNAVAILABLE: i32 = 3;
/// The query matched no declaration.
pub const EXIT_SYMBOL_NOT_FOUND: i32 = 4;
/// The query matched more than one declaration.
pub const EXIT_AMBIGUOUS_SYMBOL: i32 = 5;
/// No allowed form of the context target fits the token budget.
pub const EXIT_BUDGET_TOO_SMALL: i32 = 8;

/// A CLI failure rendered as an error object in `--json` mode and as human text
/// otherwise.
#[derive(Debug)]
pub struct CliError {
    /// The contract error code (for example `repository_unavailable`).
    pub code: &'static str,
    /// The documented process exit code.
    pub exit: i32,
    /// A human-readable message; also the JSON `message` field.
    pub message: String,
    /// A corrective hint; also the JSON `hint` field.
    pub hint: String,
    /// Additional contract fields in the order they must appear. Boxed so
    /// `CliError` stays small enough to be a `Result` error type.
    pub extra: Box<Map<String, Value>>,
}

impl CliError {
    /// A `general` failure (exit 1).
    pub fn general(message: impl Into<String>, hint: impl Into<String>) -> CliError {
        CliError {
            code: "general",
            exit: EXIT_GENERAL,
            message: message.into(),
            hint: hint.into(),
            extra: Box::new(Map::new()),
        }
    }

    /// An `invalid_arguments` failure (exit 2).
    pub fn invalid_arguments(message: impl Into<String>, hint: impl Into<String>) -> CliError {
        CliError {
            code: "invalid_arguments",
            exit: EXIT_INVALID_ARGUMENTS,
            message: message.into(),
            hint: hint.into(),
            extra: Box::new(Map::new()),
        }
    }

    /// A `repository_unavailable` failure (exit 3).
    pub fn repository_unavailable(message: impl Into<String>, hint: impl Into<String>) -> CliError {
        CliError {
            code: "repository_unavailable",
            exit: EXIT_REPOSITORY_UNAVAILABLE,
            message: message.into(),
            hint: hint.into(),
            extra: Box::new(Map::new()),
        }
    }

    /// A `symbol_not_found` failure (exit 4) with up to five suggestions.
    pub fn symbol_not_found(
        message: impl Into<String>,
        hint: impl Into<String>,
        suggestions: Vec<String>,
    ) -> CliError {
        let mut extra = Map::new();
        extra.insert("suggestions".to_string(), json!(suggestions));
        CliError {
            code: "symbol_not_found",
            exit: EXIT_SYMBOL_NOT_FOUND,
            message: message.into(),
            hint: hint.into(),
            extra: Box::new(extra),
        }
    }

    /// An `ambiguous_symbol` failure (exit 5) with a paginated candidate page.
    pub fn ambiguous_symbol(
        message: impl Into<String>,
        hint: impl Into<String>,
        total: u64,
        truncated: bool,
        next_offset: Option<u64>,
        candidates: Vec<Value>,
    ) -> CliError {
        let mut extra = Map::new();
        extra.insert("total".to_string(), json!(total));
        extra.insert("truncated".to_string(), json!(truncated));
        extra.insert("next_offset".to_string(), json!(next_offset));
        extra.insert("candidates".to_string(), json!(candidates));
        CliError {
            code: "ambiguous_symbol",
            exit: EXIT_AMBIGUOUS_SYMBOL,
            message: message.into(),
            hint: hint.into(),
            extra: Box::new(extra),
        }
    }

    /// A `budget_too_small` failure (exit 8): no allowed form of the context
    /// target fits `budget_tokens`. `required_tokens` is the minimum estimate
    /// among the target's allowed forms (OUTPUT-CONTRACT `rivet context`).
    pub fn budget_too_small(
        message: impl Into<String>,
        hint: impl Into<String>,
        budget_tokens: u64,
        required_tokens: u64,
    ) -> CliError {
        let mut extra = Map::new();
        extra.insert("budget_tokens".to_string(), json!(budget_tokens));
        extra.insert("required_tokens".to_string(), json!(required_tokens));
        CliError {
            code: "budget_too_small",
            exit: EXIT_BUDGET_TOO_SMALL,
            message: message.into(),
            hint: hint.into(),
            extra: Box::new(extra),
        }
    }

    /// Whether this error's meaning depends on the snapshot it was computed
    /// against.
    ///
    /// `symbol_not_found` and `ambiguous_symbol` are answers about the indexed
    /// declarations: "no match" or "these matches" is true only of that
    /// snapshot and its coverage. Argument errors, lock/I/O/cache failures,
    /// and `general` failures are not, even when raised after a refresh.
    pub fn is_index_dependent(&self) -> bool {
        matches!(self.code, "symbol_not_found" | "ambiguous_symbol")
    }

    /// Attaches the acquired snapshot's `index` metadata to an index-dependent
    /// error, and returns every other error unchanged.
    ///
    /// OUTPUT-CONTRACT "Errors": "Index-dependent errors also include `index`
    /// if a compatible snapshot was successfully acquired." A caller invokes
    /// this only after acquiring a snapshot, so an error raised before (an
    /// argument error, a failed refresh) never carries `index`. The key is
    /// appended after the error's required fields, leaving their documented
    /// order unchanged (AF5, audit finding 15).
    pub fn with_snapshot_index(mut self, index: Value) -> CliError {
        if self.is_index_dependent() {
            self.extra.insert("index".to_string(), index);
        }
        self
    }
}

/// Writes `value` as the single stdout object, prepending `schema_version` as
/// the first key, followed by exactly one newline.
///
/// `value` is expected to be an object; a non-object is wrapped under a `value`
/// key so the output is always one JSON object.
pub fn emit_success(value: Value) {
    let object = with_schema_version(value);
    let serialized = serde_json::to_string(&object).expect("serializing a JSON value cannot fail");
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(serialized.as_bytes())
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.flush())
        .expect("writing the success object to stdout failed");
}

/// Writes exactly one error object to stderr, leaves stdout untouched, and
/// terminates the process with `exit`.
///
/// Key order is `schema_version`, `error`, `message`, `hint`, then `extra`.
pub fn emit_error(
    code: &str,
    exit: i32,
    message: &str,
    hint: &str,
    extra: Map<String, Value>,
) -> ! {
    let mut object = Map::new();
    object.insert("schema_version".to_string(), json!(SCHEMA_VERSION));
    object.insert("error".to_string(), json!(code));
    object.insert("message".to_string(), json!(message));
    object.insert("hint".to_string(), json!(hint));
    object.extend(extra);

    let serialized = serde_json::to_string(&Value::Object(object))
        .expect("serializing a JSON value cannot fail");
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(serialized.as_bytes());
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
    std::process::exit(exit);
}

/// Returns `value` as an object whose first key is `schema_version`.
fn with_schema_version(value: Value) -> Value {
    let mut object = Map::new();
    object.insert("schema_version".to_string(), json!(SCHEMA_VERSION));
    match value {
        Value::Object(map) => {
            object.extend(map);
        }
        other => {
            object.insert("value".to_string(), other);
        }
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::{CliError, SCHEMA_VERSION, with_schema_version};
    use serde_json::{Value, json};

    #[test]
    fn schema_version_is_prepended_first() {
        let value = json!({"error": "general", "message": "x", "hint": "y"});
        let Value::Object(map) = with_schema_version(value) else {
            panic!("expected an object");
        };
        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["schema_version", "error", "message", "hint"]);
        assert_eq!(map.get("schema_version"), Some(&json!(SCHEMA_VERSION)));
    }

    #[test]
    fn error_constructors_use_documented_codes_and_exits() {
        let general = CliError::general("m", "h");
        assert_eq!((general.code, general.exit), ("general", 1));
        let invalid = CliError::invalid_arguments("m", "h");
        assert_eq!((invalid.code, invalid.exit), ("invalid_arguments", 2));
        let unavailable = CliError::repository_unavailable("m", "h");
        assert_eq!(
            (unavailable.code, unavailable.exit),
            ("repository_unavailable", 3)
        );
    }

    #[test]
    fn symbol_errors_carry_required_extra_fields() {
        let not_found = CliError::symbol_not_found("m", "h", vec!["a".to_string()]);
        assert_eq!((not_found.code, not_found.exit), ("symbol_not_found", 4));
        assert_eq!(not_found.extra.get("suggestions"), Some(&json!(["a"])));

        let ambiguous =
            CliError::ambiguous_symbol("m", "h", 3, true, Some(2), vec![json!({"id": "x"})]);
        assert_eq!((ambiguous.code, ambiguous.exit), ("ambiguous_symbol", 5));
        let keys: Vec<&str> = ambiguous.extra.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["total", "truncated", "next_offset", "candidates"]
        );
        assert_eq!(ambiguous.extra.get("next_offset"), Some(&json!(2)));
    }

    #[test]
    fn budget_too_small_carries_budget_and_required_tokens_in_order() {
        let error = CliError::budget_too_small("m", "h", 10, 17);
        assert_eq!((error.code, error.exit), ("budget_too_small", 8));
        let keys: Vec<&str> = error.extra.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["budget_tokens", "required_tokens"]);
        assert_eq!(error.extra.get("budget_tokens"), Some(&json!(10)));
        assert_eq!(error.extra.get("required_tokens"), Some(&json!(17)));
    }

    #[test]
    fn only_index_dependent_errors_take_the_snapshot_index() {
        let index = json!({"snapshot": "blake3:x"});
        let not_found =
            CliError::symbol_not_found("m", "h", Vec::new()).with_snapshot_index(index.clone());
        let keys: Vec<&str> = not_found.extra.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["suggestions", "index"]);
        assert_eq!(not_found.extra.get("index"), Some(&index));

        let ambiguous = CliError::ambiguous_symbol("m", "h", 2, false, None, Vec::new())
            .with_snapshot_index(index.clone());
        let keys: Vec<&str> = ambiguous.extra.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["total", "truncated", "next_offset", "candidates", "index"]
        );

        for error in [
            CliError::invalid_arguments("m", "h"),
            CliError::repository_unavailable("m", "h"),
            CliError::general("m", "h"),
        ] {
            let error = error.with_snapshot_index(index.clone());
            assert!(error.extra.get("index").is_none(), "{error:?}");
        }
    }
}
