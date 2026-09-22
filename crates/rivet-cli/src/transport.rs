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
}
