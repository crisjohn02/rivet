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

/// `source` with every run of whitespace (`char::is_whitespace`, so line
/// breaks and tabs too) replaced by a single space, for a source excerpt shown
/// inside one row (SY1). Nothing else changes: a run at either end also
/// becomes one space rather than being trimmed.
pub(crate) fn collapse_whitespace(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut in_run = false;
    for character in source.chars() {
        if character.is_whitespace() {
            if !in_run {
                output.push(' ');
            }
            in_run = true;
        } else {
            output.push(character);
            in_run = false;
        }
    }
    output
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

/// The most diagnostics the coverage line names by file; with more, it
/// counts them by code instead (CV1).
const NAMED_DIAGNOSTICS: u64 = 2;

/// The notes an `index` metadata object requires in human output: a line for
/// a `cached` (`--no-refresh`) snapshot, and a coverage line whenever
/// coverage is incomplete, any file was skipped, or any diagnostic exists.
/// Empty when the snapshot is fresh and complete.
///
/// The coverage line's grammar is OUTPUT-CONTRACT "Common index metadata"
/// (CV1): `coverage <complete|incomplete>: <indexed>/<seen> files indexed`,
/// then `; skipped <count> <key>, ...` for the non-zero skip counts, then
/// `; ` and [`diagnostics_clause`]. It states the diagnostics itself rather
/// than pointing to `--json`.
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
        "coverage {}: {}/{} files indexed",
        if complete { "complete" } else { "incomplete" },
        coverage["files_indexed"].as_u64().unwrap_or(0),
        coverage["files_seen"].as_u64().unwrap_or(0),
    );
    if !skipped_parts.is_empty() {
        line.push_str(&format!("; skipped {}", skipped_parts.join(", ")));
    }
    line.push_str("; ");
    line.push_str(&diagnostics_clause(diagnostics));
    output.push_str(&line);
    output.push('\n');
    output
}

/// The coverage line's diagnostics clause (OUTPUT-CONTRACT "Common index
/// metadata"):
///
/// - `0 diagnostics` when `total` is 0;
/// - `1 diagnostic: <file> (<code>)`, or `2 diagnostics: ` and both, when
///   `total` is at most [`NAMED_DIAGNOSTICS`];
/// - otherwise `<total> diagnostics (<count> <code>, ...)`, the listed items
///   counted by code, by descending count and then code bytes, with no path.
///   When the items are capped below `total`, the counts say so:
///   `60 diagnostics (first 50: 50 parse_error)`.
///
/// Named items are taken in the JSON's order, which is already the contract's
/// diagnostic sort order, and `file` is printed exactly as the JSON holds it:
/// repository-relative, with a non-UTF-8 path's escaped bytes kept escaped.
/// Ordinary `unsupported` files have no diagnostic, so they are only counted.
fn diagnostics_clause(diagnostics: &Value) -> String {
    let total = diagnostics["total"].as_u64().unwrap_or(0);
    let noun = if total == 1 {
        "diagnostic"
    } else {
        "diagnostics"
    };
    let items: &[Value] = diagnostics["items"].as_array().map_or(&[], Vec::as_slice);
    if total == 0 || items.is_empty() {
        // `items` holds min(total, 50) entries, so an empty list with a
        // non-zero total is not produced; the count is still stated.
        return format!("{total} {noun}");
    }
    if total <= NAMED_DIAGNOSTICS && items.len() as u64 >= total {
        let named: Vec<String> = items
            .iter()
            .take(total as usize)
            .map(|item| format!("{} ({})", text(&item["file"]), text(&item["code"])))
            .collect();
        return format!("{total} {noun}: {}", named.join(", "));
    }
    let mut by_code: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    for item in items {
        *by_code.entry(text(&item["code"])).or_insert(0) += 1;
    }
    let mut counts: Vec<(&str, u64)> = by_code.into_iter().collect();
    // Descending count, then code bytes (`str` ordering is bytewise).
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let counts: Vec<String> = counts
        .iter()
        .map(|(code, count)| format!("{count} {code}"))
        .collect();
    let listed = items.len() as u64;
    let capped = diagnostics["truncated"].as_bool().unwrap_or(false) || listed < total;
    if capped {
        format!("{total} {noun} (first {listed}: {})", counts.join(", "))
    } else {
        format!("{total} {noun} ({})", counts.join(", "))
    }
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
    use super::{collapse_whitespace, columns, index_notes, page_line, tier};
    use serde_json::json;

    #[test]
    fn whitespace_runs_collapse_to_one_space() {
        assert_eq!(collapse_whitespace("$x"), "$x");
        assert_eq!(collapse_whitespace(""), "");
        assert_eq!(
            collapse_whitespace("$this->a\n        ->b()"),
            "$this->a ->b()"
        );
        assert_eq!(
            collapse_whitespace("$a\r\n\t \u{a0}->b( 1,  2 )"),
            "$a ->b( 1, 2 )"
        );
        // A run at an end becomes one space; it is not trimmed.
        assert_eq!(collapse_whitespace("\n$a\n"), " $a ");
        // Non-ASCII text is kept as is.
        assert_eq!(collapse_whitespace("$ü\n\n->ß"), "$ü ->ß");
    }

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
            "diagnostics": {"total": 2, "truncated": false, "items": [
                diagnostic("a.ts", "unsupported_language"),
                diagnostic("b.php", "parse_error"),
            ]},
        });
        assert_eq!(
            index_notes(&cached),
            "snapshot: cached (--no-refresh); it may not match the working tree\n\
             coverage incomplete: 1/3 files indexed; skipped 1 unsupported, 1 parse_error; \
             2 diagnostics: a.ts (unsupported_language), b.php (parse_error)\n"
        );
    }

    /// One diagnostic item as the JSON holds it.
    fn diagnostic(file: &str, code: &str) -> serde_json::Value {
        json!({"file": file, "code": code, "detail": "detail text"})
    }

    /// The notes of a fresh (`content`) snapshot with this coverage.
    fn fresh_notes(
        complete: bool,
        seen: u64,
        indexed: u64,
        skipped: serde_json::Value,
        diagnostics: serde_json::Value,
    ) -> String {
        index_notes(&json!({
            "freshness": "content",
            "coverage": {"complete": complete, "files_seen": seen,
                "files_indexed": indexed, "skipped": skipped},
            "diagnostics": diagnostics,
        }))
    }

    /// Every skip count at zero except those given.
    fn skipped(counts: &[(&str, u64)]) -> serde_json::Value {
        let mut object = json!({"unsupported": 0, "binary": 0, "size": 0,
            "encoding": 0, "parse_error": 0, "resource_limit": 0});
        for (key, count) in counts {
            object[*key] = json!(count);
        }
        object
    }

    #[test]
    fn coverage_line_without_diagnostics_states_the_zero_count() {
        let line = fresh_notes(
            false,
            10,
            9,
            skipped(&[("unsupported", 1)]),
            json!({"total": 0, "truncated": false, "items": []}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 9/10 files indexed; skipped 1 unsupported; 0 diagnostics\n"
        );
    }

    #[test]
    fn coverage_line_names_one_diagnostic_inline() {
        let line = fresh_notes(
            false,
            2815,
            1219,
            skipped(&[("unsupported", 1595), ("parse_error", 1)]),
            json!({"total": 1, "truncated": false,
                "items": [diagnostic("app/X.php", "parse_error")]}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 1219/2815 files indexed; skipped 1595 unsupported, \
             1 parse_error; 1 diagnostic: app/X.php (parse_error)\n"
        );
        assert!(!line.contains("--json"), "{line}");
    }

    #[test]
    fn coverage_line_names_two_diagnostics() {
        let line = fresh_notes(
            false,
            4,
            2,
            skipped(&[("binary", 1), ("parse_error", 1)]),
            json!({"total": 2, "truncated": false,
                "items": [diagnostic("B.php", "binary_file"), diagnostic("a.php", "parse_error")]}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 2/4 files indexed; skipped 1 binary, 1 parse_error; \
             2 diagnostics: B.php (binary_file), a.php (parse_error)\n"
        );
    }

    #[test]
    fn more_than_two_diagnostics_are_counted_by_code_without_paths() {
        // Three of mixed codes: descending count, then code bytes.
        let items = json!([
            diagnostic("B.php", "binary_file"),
            diagnostic("a.php", "parse_error"),
            diagnostic("z/Broken.php", "parse_error"),
        ]);
        let line = fresh_notes(
            false,
            13,
            9,
            skipped(&[("unsupported", 1), ("binary", 1), ("parse_error", 2)]),
            json!({"total": 3, "truncated": false, "items": items}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 9/13 files indexed; skipped 1 unsupported, 1 binary, \
             2 parse_error; 3 diagnostics (2 parse_error, 1 binary_file)\n"
        );
        assert!(!line.contains(".php"), "{line}");

        // Equal counts are ordered by code bytes, not by item order.
        let items = json!([
            diagnostic("a.php", "parse_error"),
            diagnostic("b.php", "file_too_large"),
            diagnostic("c.php", "binary_file"),
            diagnostic("d.php", "parse_error"),
            diagnostic("e.php", "file_too_large"),
        ]);
        let line = fresh_notes(
            false,
            5,
            0,
            skipped(&[("binary", 1), ("size", 2), ("parse_error", 2)]),
            json!({"total": 5, "truncated": false, "items": items}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 0/5 files indexed; skipped 1 binary, 2 size, 2 parse_error; \
             5 diagnostics (2 file_too_large, 2 parse_error, 1 binary_file)\n"
        );
    }

    #[test]
    fn fifty_diagnostics_of_one_code_are_one_count() {
        // The pilot project's shape: every diagnostic is a `.ts` file.
        let items: Vec<serde_json::Value> = (0..50)
            .map(|index| {
                diagnostic(
                    &format!("resources/js/components/f{index:02}.ts"),
                    "unsupported_language",
                )
            })
            .collect();
        let line = fresh_notes(
            false,
            691,
            250,
            skipped(&[("unsupported", 441)]),
            json!({"total": 50, "truncated": false, "items": items}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 250/691 files indexed; skipped 441 unsupported; \
             50 diagnostics (50 unsupported_language)\n"
        );
    }

    #[test]
    fn a_truncated_list_says_its_counts_cover_the_first_items() {
        // 60 diagnostics, capped at 50 items: the counts are of those 50.
        let items: Vec<serde_json::Value> = (0..50)
            .map(|index| diagnostic(&format!("f{index:02}.php"), "parse_error"))
            .collect();
        let line = fresh_notes(
            false,
            70,
            10,
            skipped(&[("parse_error", 60)]),
            json!({"total": 60, "truncated": true, "items": items}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 10/70 files indexed; skipped 60 parse_error; \
             60 diagnostics (first 50: 50 parse_error)\n"
        );
    }

    #[test]
    fn coverage_line_with_only_unsupported_skips_still_names_diagnostics() {
        // An enabled language without an extractor is counted as unsupported
        // and also has a diagnostic; the ordinary unsupported file is only
        // counted.
        let line = fresh_notes(
            false,
            11,
            9,
            skipped(&[("unsupported", 2)]),
            json!({"total": 1, "truncated": false,
                "items": [diagnostic("src/ok.ts", "unsupported_language")]}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 9/11 files indexed; skipped 2 unsupported; \
             1 diagnostic: src/ok.ts (unsupported_language)\n"
        );
    }

    #[test]
    fn complete_coverage_with_a_diagnostic_prints_the_line() {
        let line = fresh_notes(
            true,
            2,
            2,
            skipped(&[]),
            json!({"total": 1, "truncated": false,
                "items": [diagnostic("a.php", "resource_limit")]}),
        );
        assert_eq!(
            line,
            "coverage complete: 2/2 files indexed; 1 diagnostic: a.php (resource_limit)\n"
        );
    }

    #[test]
    fn a_non_utf8_path_diagnostic_stays_escaped() {
        // The JSON `file` holds `src/bad-\xff.php` with a literal backslash;
        // the line prints those characters, never a decoded byte. The path is
        // outside the scan domain, so it is in no count, yet it forces
        // `complete: false`.
        let line = fresh_notes(
            false,
            2,
            2,
            skipped(&[]),
            json!({"total": 1, "truncated": false,
                "items": [diagnostic("src/bad-\\xff.php", "non_utf8_path")]}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 2/2 files indexed; \
             1 diagnostic: src/bad-\\xff.php (non_utf8_path)\n"
        );
        assert!(line.contains(r"src/bad-\xff.php"), "{line}");
    }

    #[test]
    fn a_count_without_items_prints_only_the_count() {
        // Not produced by the binary (items hold min(total, 50) entries); the
        // line still states the count rather than naming nothing.
        let line = fresh_notes(
            false,
            3,
            1,
            skipped(&[("parse_error", 2)]),
            json!({"total": 2}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 1/3 files indexed; skipped 2 parse_error; 2 diagnostics\n"
        );
        // Fewer items than a small total: nothing is named as if it were all
        // of them; the count says which items it covers.
        let line = fresh_notes(
            false,
            3,
            1,
            skipped(&[("parse_error", 2)]),
            json!({"total": 2, "items": [diagnostic("a.php", "parse_error")]}),
        );
        assert_eq!(
            line,
            "coverage incomplete: 1/3 files indexed; skipped 2 parse_error; \
             2 diagnostics (first 1: 1 parse_error)\n"
        );
    }
}
