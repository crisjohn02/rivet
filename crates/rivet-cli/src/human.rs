//! Compact human rendering shared by every command (spec §11.3, §13–§16.1;
//! T34).
//!
//! Every renderer reads only the command's JSON success object (or a
//! [`CliError`]), so the human form can never report something the JSON does
//! not, and the JSON bytes are untouched by it. The rules:
//!
//! - Resolution: each reference line carries its tier word (`exact`,
//!   `scoped`, `name_match`), and every `name_match` result also ends in `?`
//!   (spec §11.3), so `scoped` is visibly distinct from `exact`.
//! - Pagination: a truncated list says which slice is shown and the next
//!   `--offset` ([`page_line`]).
//! - Coverage: an incomplete snapshot, any skipped file, any diagnostic, or a
//!   `cached` (`--no-refresh`) answer is called out ([`index_notes`]), so an
//!   empty or short result is never mistaken for a complete one.
//!
//! Output never depends on the terminal: no colour, no width detection, no
//! locale. Columns are padded to the widest cell of the list being printed.

use serde_json::Value;

use crate::transport::CliError;

/// The tier label for a resolution value: the tier word, plus ` ?` for
/// `name_match` (spec §11.3: "Human output marks `name_match` results with
/// `?`").
pub(crate) fn tier(resolution: &Value) -> String {
    match resolution.as_str().unwrap_or("") {
        "name_match" => "name_match ?".to_string(),
        other => other.to_string(),
    }
}

/// `file:start-end` of a symbol object.
pub(crate) fn span(symbol: &Value) -> String {
    format!(
        "{}:{}-{}",
        text(&symbol["file"]),
        symbol["start_line"].as_u64().unwrap_or(0),
        symbol["end_line"].as_u64().unwrap_or(0)
    )
}

/// `file:line:column` of a reference object.
pub(crate) fn site(reference: &Value) -> String {
    format!(
        "{}:{}:{}",
        text(&reference["file"]),
        reference["line"].as_u64().unwrap_or(0),
        reference["column"].as_u64().unwrap_or(0)
    )
}

/// A string field, or the empty string.
pub(crate) fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

/// Renders `rows` as space-separated columns, each padded to its widest cell
/// (by character count), with `indent` before every row. Trailing padding is
/// never emitted.
pub(crate) fn columns(rows: &[Vec<String>], indent: &str) -> String {
    let count = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0_usize; count];
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let mut output = String::new();
    for row in rows {
        let mut line = indent.to_string();
        for (index, cell) in row.iter().enumerate() {
            line.push_str(cell);
            if index + 1 < row.len() {
                let pad = widths[index] - cell.chars().count();
                line.push_str(&" ".repeat(pad + 2));
            }
        }
        output.push_str(line.trim_end());
        output.push('\n');
    }
    output
}

/// The pagination line for a truncated page, or `None` when the page holds
/// every match.
///
/// The page's start is derived from the JSON alone: with a `next_offset` the
/// page ends there; without one the page runs to the end of the list (or is
/// empty because the offset is past the end). `next` is the command-line
/// form that fetches the next page, for example `--offset 50`.
pub(crate) fn page_line(
    total: u64,
    truncated: bool,
    next_offset: Option<u64>,
    shown: u64,
    next: impl Fn(u64) -> String,
) -> Option<String> {
    if !truncated {
        return None;
    }
    if shown == 0 {
        return Some(format!(
            "showing none of {total}: --offset is past the end; start again at --offset 0"
        ));
    }
    let end = next_offset.unwrap_or(total);
    let start = end.saturating_sub(shown) + 1;
    Some(match next_offset {
        Some(offset) => format!("showing {start}-{end} of {total}; next: {}", next(offset)),
        None => format!("showing {start}-{end} of {total}; this is the last page"),
    })
}

/// The pagination line of a JSON list object with `total`, `truncated`, and
/// `next_offset` whose page holds `shown` items.
pub(crate) fn page_line_of(list: &Value, shown: usize) -> Option<String> {
    page_line(
        list["total"].as_u64().unwrap_or(0),
        list["truncated"].as_bool().unwrap_or(false),
        list["next_offset"].as_u64(),
        shown as u64,
        |offset| format!("--offset {offset}"),
    )
}

/// The notes an `index` metadata object requires in human output: a line for
/// a `cached` (`--no-refresh`) snapshot, and a coverage line whenever
/// coverage is incomplete, any file was skipped, or any diagnostic exists.
/// Empty when the snapshot is fresh and complete.
pub(crate) fn index_notes(index: &Value) -> String {
    let mut output = String::new();
    if index["freshness"] == "cached" {
        output.push_str("snapshot: cached (--no-refresh); it may not match the working tree\n");
    }
    let coverage = &index["coverage"];
    let complete = coverage["complete"].as_bool().unwrap_or(false);
    let skipped = &coverage["skipped"];
    let skipped_parts: Vec<String> = [
        "unsupported",
        "binary",
        "size",
        "encoding",
        "parse_error",
        "resource_limit",
    ]
    .iter()
    .filter_map(|key| {
        let count = skipped[*key].as_u64().unwrap_or(0);
        (count > 0).then(|| format!("{count} {key}"))
    })
    .collect();
    let diagnostics = &index["diagnostics"];
    let diagnostics_total = diagnostics["total"].as_u64().unwrap_or(0);
    if complete && skipped_parts.is_empty() && diagnostics_total == 0 {
        return output;
    }
    let mut line = format!(
        "coverage: {}; {} of {} files indexed",
        if complete { "complete" } else { "incomplete" },
        coverage["files_indexed"].as_u64().unwrap_or(0),
        coverage["files_seen"].as_u64().unwrap_or(0),
    );
    if !skipped_parts.is_empty() {
        line.push_str(&format!("; skipped {}", skipped_parts.join(", ")));
    }
    line.push_str(&format!("; {diagnostics_total} diagnostics"));
    if diagnostics_total > 0 {
        line.push_str(" (listed with --json)");
    }
    output.push_str(&line);
    output.push('\n');
    output
}

/// Appends `source` followed by exactly one line break.
pub(crate) fn push_block(output: &mut String, source: &str) {
    output.push_str(source);
    if !source.ends_with('\n') {
        output.push('\n');
    }
}

/// Appends every line of `body` indented by two spaces.
pub(crate) fn push_indented(output: &mut String, body: &str) {
    for line in body.lines() {
        if line.is_empty() {
            output.push('\n');
        } else {
            output.push_str("  ");
            output.push_str(line);
            output.push('\n');
        }
    }
}

/// The human error text for `rivet <command>`: the message, the hint, and
/// whatever the error's extra fields carry (ambiguity candidates with their
/// pagination, suggestions, the budget shortfall, the parser detail, and the
/// snapshot's coverage notes). Written to stderr by the binary.
pub fn error_text(command: &str, error: &CliError) -> String {
    let mut output = format!("rivet {command}: {}\n", error.message);
    if !error.hint.is_empty() {
        output.push_str(&format!("hint: {}\n", error.hint));
    }
    let extra = &error.extra;
    if let Some(Value::Array(candidates)) = extra.get("candidates") {
        output.push_str("candidates:\n");
        let rows: Vec<Vec<String>> = candidates
            .iter()
            .map(|candidate| {
                vec![
                    text(&candidate["id"]).to_string(),
                    text(&candidate["kind"]).to_string(),
                    span(candidate),
                ]
            })
            .collect();
        output.push_str(&columns(&rows, "  "));
        let line = page_line(
            extra.get("total").and_then(Value::as_u64).unwrap_or(0),
            extra
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            extra.get("next_offset").and_then(Value::as_u64),
            candidates.len() as u64,
            |offset| {
                if command == "context" {
                    // Context rejects `--offset`; further candidates are paged
                    // with `rivet symbol` (OUTPUT-CONTRACT "Errors").
                    format!("rivet symbol <query> --offset {offset}")
                } else {
                    format!("--offset {offset}")
                }
            },
        );
        if let Some(line) = line {
            output.push_str(&line);
            output.push('\n');
        }
    }
    if let Some(Value::Array(suggestions)) = extra.get("suggestions")
        && !suggestions.is_empty()
    {
        output.push_str("did you mean:\n");
        for suggestion in suggestions {
            output.push_str(&format!("  {}\n", text(suggestion)));
        }
    }
    if let (Some(budget), Some(required)) = (
        extra.get("budget_tokens").and_then(Value::as_u64),
        extra.get("required_tokens").and_then(Value::as_u64),
    ) {
        output.push_str(&format!(
            "required_tokens: {required} (budget_tokens: {budget})\n"
        ));
    }
    if let Some(Value::String(detail)) = extra.get("detail") {
        output.push_str(&format!("detail: {detail}\n"));
    }
    if let Some(index) = extra.get("index") {
        output.push_str(&index_notes(index));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{columns, index_notes, page_line, tier};
    use serde_json::json;

    #[test]
    fn name_match_is_marked_and_scoped_is_distinct_from_exact() {
        assert_eq!(tier(&json!("exact")), "exact");
        assert_eq!(tier(&json!("scoped")), "scoped");
        assert_eq!(tier(&json!("name_match")), "name_match ?");
    }

    #[test]
    fn pages_name_their_slice_and_the_next_offset() {
        let next = |offset: u64| format!("--offset {offset}");
        assert_eq!(page_line(5, false, None, 5, next), None);
        assert_eq!(
            page_line(120, true, Some(50), 50, next).as_deref(),
            Some("showing 1-50 of 120; next: --offset 50")
        );
        assert_eq!(
            page_line(120, true, Some(100), 50, next).as_deref(),
            Some("showing 51-100 of 120; next: --offset 100")
        );
        assert_eq!(
            page_line(120, true, None, 20, next).as_deref(),
            Some("showing 101-120 of 120; this is the last page")
        );
        assert_eq!(
            page_line(120, true, None, 0, next).as_deref(),
            Some("showing none of 120: --offset is past the end; start again at --offset 0")
        );
    }

    #[test]
    fn columns_pad_from_content_without_trailing_space() {
        let rows = vec![
            vec!["a".to_string(), "bb".to_string(), "c".to_string()],
            vec!["aaa".to_string(), "b".to_string(), "".to_string()],
        ];
        assert_eq!(columns(&rows, "  "), "  a    bb  c\n  aaa  b\n");
    }

    #[test]
    fn complete_fresh_coverage_prints_nothing() {
        let index = json!({
            "freshness": "content",
            "coverage": {"complete": true, "files_seen": 2, "files_indexed": 2,
                "skipped": {"unsupported": 0}},
            "diagnostics": {"total": 0},
        });
        assert_eq!(index_notes(&index), "");
        let cached = json!({
            "freshness": "cached",
            "coverage": {"complete": false, "files_seen": 3, "files_indexed": 1,
                "skipped": {"unsupported": 1, "parse_error": 1}},
            "diagnostics": {"total": 2},
        });
        assert_eq!(
            index_notes(&cached),
            "snapshot: cached (--no-refresh); it may not match the working tree\n\
             coverage: incomplete; 1 of 3 files indexed; skipped 1 unsupported, 1 parse_error; \
             2 diagnostics (listed with --json)\n"
        );
    }
}
