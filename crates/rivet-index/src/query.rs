//! Symbol query-form resolution (spec §10.2, §10.3; OUTPUT-CONTRACT "Errors"
//! and "Ordering and versioning").
//!
//! [`resolve_query`] tries the accepted forms in a fixed order and stops at the
//! first form that yields at least one match:
//!
//! 1. a canonical ID containing `#`;
//! 2. a native qualified name containing `\` or `::` (exact, then
//!    ASCII-case-insensitively for case-insensitive kinds);
//! 3. `file:line`, a repository-relative path followed by a positive line after
//!    the final colon;
//! 4. a dotted path, comparing separator-normalized qualified names at a
//!    component boundary;
//! 5. a short name, matched against the persisted `lookup_name`.
//!
//! Every case-insensitive comparison folds ASCII letters only, as PHP does and
//! as the persisted `lookup_name` is folded (AF2).
//!
//! Canonical IDs and native qualified names are tried before `file:line` so a
//! native `Foo::bar` is never misread as a path. Everything here works from
//! persisted rows, so resolution never reparses a file.
//!
//! A `file:line` query's *syntax* (a positive in-range line, a non-empty
//! repository-relative path with no `..` component) is checked by
//! [`check_query_syntax`], which reads nothing, so a command can reject it
//! before any filesystem work (OUTPUT-CONTRACT "Errors": "Invalid arguments
//! take precedence over filesystem work"). Only what needs the snapshot stays
//! here: whether the path is a stored file, how that file was classified, and
//! which symbol encloses the line (a line past the end of the file encloses
//! nothing).

use std::collections::BTreeSet;

use rivet_core::{ParseStatus, SymbolId, SymbolKind};
use rivet_store::{Error, Store, SymbolRow};

/// The outcome of resolving one query against the persisted symbols.
#[derive(Debug)]
pub enum QueryOutcome {
    /// Zero or more matching symbols, ordered by `(file bytes, start_byte, id)`.
    Symbols(Vec<SymbolRow>),
    /// A `file:line` query whose path is not present in the `files` table.
    PathNotIndexed {
        /// The repository-relative path named before the final colon.
        path: String,
    },
    /// A `file:line` query, or a canonical ID matching no symbol, whose path
    /// is a stored file that was not indexed (T32; OUTPUT-CONTRACT "Errors").
    FileNotIndexed {
        /// The repository-relative path.
        path: String,
        /// Why the file carries no facts: never [`ParseStatus::Ok`].
        status: ParseStatus,
        /// The stored `files.language`, `None` for a file of no enabled
        /// language.
        language: Option<String>,
    },
    /// A `file:line` query on an indexed file whose line lies in no symbol.
    NoEnclosingSymbol {
        /// Up to five distinct qualified names nearest to the line.
        suggestions: Vec<String>,
    },
    /// A `file:line`-shaped query that is syntactically invalid. A command
    /// rejects it before any filesystem work with [`check_query_syntax`];
    /// [`resolve_query`] reports the same outcome for a direct caller.
    InvalidFileLine {
        /// The rejected path.
        path: String,
        /// Why the query was rejected.
        reason: String,
    },
}

/// A syntactically invalid `file:line` query (see [`check_query_syntax`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidFileLine {
    /// The path before the final colon.
    pub path: String,
    /// Why the query was rejected.
    pub reason: String,
}

/// Checks the syntax of `query` without reading anything.
///
/// A query is `file:line`-shaped when the text after its final colon is an
/// optional `+` followed by one or more ASCII digits, exactly the spellings the
/// `file:line` form has always accepted. No other form can end that way: a PHP
/// name cannot start with a digit, and a canonical ID ends in a qualified name
/// or a `#` ordinal. A shaped query is invalid when its line is zero or does
/// not fit a `u32`, or its path is empty, absolute, or has a `..` component.
/// Any other query is left to [`resolve_query`].
///
/// Every check here is decidable from the text alone, so a command runs it
/// before discovering the root or refreshing. What remains for
/// [`resolve_query`] needs the snapshot: whether the path is a stored file,
/// that file's parse status, and whether any symbol encloses the line, which
/// is also how a line past the end of the file is detected.
pub fn check_query_syntax(query: &str) -> Result<(), InvalidFileLine> {
    match classify_file_line(query) {
        Some(Err(invalid)) => Err(invalid),
        _ => Ok(()),
    }
}

/// Resolves `query` against the persisted symbols of `store`.
pub fn resolve_query(store: &Store, query: &str) -> Result<QueryOutcome, Error> {
    // (1) Canonical ID: only a query with `#` can be one.
    if query.contains('#')
        && let Ok(id) = SymbolId::parse(query)
    {
        if let Some(row) = store.get_symbol(&id.as_canonical())? {
            return Ok(QueryOutcome::Symbols(vec![row]));
        }
        // An ID whose file part names a stored file that carries no facts
        // addresses that file directly (spec §27: "Targeting that file by
        // path/ID returns exit 6"). An ID into an indexed file, or into a path
        // the snapshot does not hold, falls through to the other forms, so a
        // missing name there is still `symbol_not_found`.
        if let Some(outcome) = not_indexed(store, id.path())? {
            return Ok(outcome);
        }
    }

    // (2) Native qualified name.
    if query.contains('\\') || query.contains("::") {
        let exact = store.find_symbols_by_qualified_name(query)?;
        if !exact.is_empty() {
            return Ok(QueryOutcome::Symbols(sort_rows(exact)));
        }
        let folded = query.to_ascii_lowercase();
        let case_folded: Vec<SymbolRow> = store
            .list_symbols()?
            .into_iter()
            .filter(|row| {
                case_insensitive_kind(row.kind) && row.qualified_name.to_ascii_lowercase() == folded
            })
            .collect();
        if !case_folded.is_empty() {
            return Ok(QueryOutcome::Symbols(sort_rows(case_folded)));
        }
    }

    // (3) `file:line`, tried before the dotted form. Native `Foo::bar` is
    // already handled above; here the text after the final colon must be a
    // positive integer.
    match classify_file_line(query) {
        Some(Ok((path, line))) => return resolve_file_line(store, path, line),
        Some(Err(InvalidFileLine { path, reason })) => {
            return Ok(QueryOutcome::InvalidFileLine { path, reason });
        }
        None => {}
    }

    // (4) Dotted path: normalize every separator to `.` on both sides and match
    // complete trailing components. A case-insensitive retry only considers
    // kinds PHP treats case-insensitively, so a wrong-case property never
    // matches.
    let normalized_query = normalize_separators(query);
    if normalized_query.contains('.') {
        let rows = store.list_symbols()?;
        let exact: Vec<SymbolRow> = rows
            .iter()
            .filter(|row| {
                ends_with_component(
                    &normalize_separators(&row.qualified_name),
                    &normalized_query,
                )
            })
            .cloned()
            .collect();
        if !exact.is_empty() {
            return Ok(QueryOutcome::Symbols(sort_rows(exact)));
        }
        let folded = normalized_query.to_ascii_lowercase();
        let case_folded: Vec<SymbolRow> = rows
            .into_iter()
            .filter(|row| {
                case_insensitive_kind(row.kind)
                    && ends_with_component(
                        &normalize_separators(&row.qualified_name).to_ascii_lowercase(),
                        &folded,
                    )
            })
            .collect();
        if !case_folded.is_empty() {
            return Ok(QueryOutcome::Symbols(sort_rows(case_folded)));
        }
    }

    // (5) Short name. PHP identifiers are case-insensitive except properties and
    // constants, so a lowercased retry is filtered to case-insensitive kinds.
    // The retry folds ASCII only, exactly as the stored `lookup_name` was
    // folded (`rivet_languages::php::lookup_name`), because PHP folds only
    // ASCII letters (AF2). Every candidate is accepted by
    // [`lookup_name_matches`], the comparison the `refs` matcher also uses
    // (AF4), so the two cannot disagree about case.
    let mut short: Vec<SymbolRow> = Vec::new();
    let lowered = query.to_ascii_lowercase();
    let mut spellings = vec![query];
    if lowered != query {
        spellings.push(lowered.as_str());
    }
    for spelling in spellings {
        for row in store.find_symbols_by_lookup_name(spelling)? {
            if lookup_name_matches(query, row.kind, &row.lookup_name)
                && !short.iter().any(|existing| existing.id == row.id)
            {
                short.push(row);
            }
        }
    }
    Ok(QueryOutcome::Symbols(sort_rows(short)))
}

/// Whether the short name `name` (a query, or a use's persisted lookup name)
/// can name a declaration of `kind` whose persisted lookup name is
/// `lookup_name` (AF4).
///
/// The comparison folds according to the *declaration's* kind, so a name whose
/// own kind is unknown (a bare identifier) is compared correctly against both
/// case-sensitive and case-insensitive declarations:
///
/// - a property compares case-sensitively with any leading `$` removed from
///   both sides, because a declaration keeps its `$` and `$x->name` omits it;
/// - a constant (including an enum case) compares case-sensitively;
/// - a type alias, which only TypeScript declares, compares case-sensitively,
///   as TypeScript identifiers do (T42);
/// - every other kind (class, interface, trait, enum, function, method,
///   namespace) compares by ASCII case folding, as PHP does (AF2).
pub fn lookup_name_matches(name: &str, kind: SymbolKind, lookup_name: &str) -> bool {
    match kind {
        SymbolKind::Property => {
            name.strip_prefix('$').unwrap_or(name)
                == lookup_name.strip_prefix('$').unwrap_or(lookup_name)
        }
        SymbolKind::Const | SymbolKind::TypeAlias => name == lookup_name,
        _ => name.eq_ignore_ascii_case(lookup_name),
    }
}

/// Classifies the `file:line` form: `None` when `query` is not
/// `file:line`-shaped (see [`check_query_syntax`]), else the path and line or
/// why the shaped query is invalid.
///
/// Native names such as `Foo::bar` are not shaped, so they are never mistaken
/// for paths.
fn classify_file_line(query: &str) -> Option<Result<(&str, u32), InvalidFileLine>> {
    let (path, line) = query.rsplit_once(':')?;
    let digits = line.strip_prefix('+').unwrap_or(line);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let invalid = |reason: &str| {
        Some(Err(InvalidFileLine {
            path: path.to_string(),
            reason: reason.to_string(),
        }))
    };
    let Ok(line) = digits.parse::<u32>() else {
        return invalid("the line number is out of range");
    };
    if line == 0 {
        return invalid("lines are numbered from 1");
    }
    if path.is_empty() {
        return invalid("the path is empty");
    }
    if path.starts_with('/') {
        return invalid("absolute paths are not repository-relative");
    }
    if path.split('/').any(|component| component == "..") {
        return invalid("`..` path components are not allowed");
    }
    Some(Ok((path, line)))
}

/// The outcome for a directly addressed `path` the snapshot stores but did not
/// index, or `None` when it is indexed or not stored at all.
fn not_indexed(store: &Store, path: &str) -> Result<Option<QueryOutcome>, Error> {
    Ok(store
        .get_file(path)?
        .filter(|file| file.parse_status != ParseStatus::Ok)
        .map(|file| QueryOutcome::FileNotIndexed {
            path: file.path,
            status: file.parse_status,
            language: file.language,
        }))
}

/// Resolves a syntactically valid `file:line` query to the innermost
/// enclosing symbol(s).
fn resolve_file_line(store: &Store, path: &str, line: u32) -> Result<QueryOutcome, Error> {
    if store.get_file(path)?.is_none() {
        return Ok(QueryOutcome::PathNotIndexed {
            path: path.to_string(),
        });
    }
    if let Some(outcome) = not_indexed(store, path)? {
        return Ok(outcome);
    }

    let symbols: Vec<SymbolRow> = store
        .list_symbols()?
        .into_iter()
        .filter(|row| row.file == path)
        .collect();
    let mut enclosing: Vec<SymbolRow> = symbols
        .iter()
        .filter(|row| row.start_line <= line && line <= row.end_line)
        .cloned()
        .collect();
    if enclosing.is_empty() {
        return Ok(QueryOutcome::NoEnclosingSymbol {
            suggestions: nearest_symbol_names(&symbols, line),
        });
    }

    // "Innermost" means minimal by containment: a symbol is dropped only when
    // another enclosing symbol lies strictly inside its span. Every remaining
    // symbol is returned so the caller reports ambiguity rather than guessing.
    // Comparing span lengths alone would silently pick the shorter of two
    // unrelated declarations that share the line, such as `function a() {}
    // function bb() {}` (T36).
    let snapshot = enclosing.clone();
    enclosing.retain(|row| {
        !snapshot.iter().any(|other| {
            row.start_byte <= other.start_byte
                && other.end_byte <= row.end_byte
                && other.end_byte - other.start_byte < row.end_byte - row.start_byte
        })
    });
    Ok(QueryOutcome::Symbols(sort_rows(enclosing)))
}

/// Returns up to five distinct qualified names nearest to `line` in `symbols`.
///
/// Ordering is by distance in lines, then by `start_line` and ID bytes so the
/// list is deterministic.
fn nearest_symbol_names(symbols: &[SymbolRow], line: u32) -> Vec<String> {
    let mut ranked: Vec<(u32, &SymbolRow)> = symbols
        .iter()
        .map(|row| (line_distance(row, line), row))
        .collect();
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.start_line.cmp(&b.1.start_line))
            .then_with(|| a.1.id.as_bytes().cmp(b.1.id.as_bytes()))
    });

    let mut seen = BTreeSet::new();
    let mut names = Vec::new();
    for (_, row) in ranked {
        if seen.insert(row.qualified_name.clone()) {
            names.push(row.qualified_name.clone());
        }
        if names.len() == 5 {
            break;
        }
    }
    names
}

/// The number of lines between `line` and the symbol's inclusive line range.
fn line_distance(row: &SymbolRow, line: u32) -> u32 {
    row.start_line
        .saturating_sub(line)
        .max(line.saturating_sub(row.end_line))
}

/// Returns up to five distinct qualified names nearest to `query`.
///
/// Suggestions are ordered by the Unicode-scalar Levenshtein distance between
/// the query's last component and the symbol's short name, then by
/// qualified-name bytes (OUTPUT-CONTRACT "Ordering and versioning"). The
/// distance is case-insensitive for kinds PHP treats case-insensitively.
pub fn suggestions(store: &Store, query: &str) -> Result<Vec<String>, Error> {
    let needle = last_component(query);
    let mut seen = BTreeSet::new();
    let mut ranked: Vec<(usize, String)> = Vec::new();
    for row in store.list_symbols()? {
        if !seen.insert(row.qualified_name.clone()) {
            continue;
        }
        let distance = if case_insensitive_kind(row.kind) {
            levenshtein(&needle.to_ascii_lowercase(), &row.name.to_ascii_lowercase())
        } else {
            levenshtein(&needle, &row.name)
        };
        ranked.push((distance, row.qualified_name.clone()));
    }
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.as_bytes().cmp(b.1.as_bytes()))
    });
    Ok(ranked.into_iter().take(5).map(|(_, name)| name).collect())
}

/// The final `.`-separated component of a normalized query.
fn last_component(query: &str) -> String {
    let normalized = normalize_separators(query);
    match normalized.rsplit_once('.') {
        Some((_, last)) => last.to_string(),
        None => normalized,
    }
}

/// The Levenshtein edit distance between two strings, counted in Unicode
/// scalar values rather than bytes.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0_usize; b.len() + 1];
    for (i, a_char) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, b_char) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(a_char != b_char);
            current[j + 1] = (previous[j + 1] + 1).min(current[j] + 1).min(substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// Reports whether `kind` names an identifier PHP treats case-insensitively.
fn case_insensitive_kind(kind: SymbolKind) -> bool {
    !matches!(kind, SymbolKind::Property | SymbolKind::Const)
}

/// Replaces every language-native path separator with `.`, collapses runs of
/// separators, and strips a single leading separator.
fn normalize_separators(value: &str) -> String {
    let replaced = value.replace("::", ".").replace("->", ".");
    let mut normalized = String::with_capacity(replaced.len());
    let mut previous_separator = false;
    for ch in replaced.chars() {
        if matches!(ch, '.' | '\\' | '/') {
            if !previous_separator {
                normalized.push('.');
                previous_separator = true;
            }
        } else {
            previous_separator = false;
            normalized.push(ch);
        }
    }
    match normalized.strip_prefix('.') {
        Some(stripped) => stripped.to_string(),
        None => normalized,
    }
}

/// Reports whether `qualified` ends with `query` at a component boundary.
///
/// `query` is expected to be separator-normalized. A match is either the whole
/// string or a suffix preceded by `.`, so `Survey.launch` matches
/// `App.Services.Survey::launch` but not `App.Services.NotSurvey.launch`.
fn ends_with_component(qualified: &str, query: &str) -> bool {
    if qualified == query {
        return true;
    }
    match qualified.strip_suffix(query) {
        Some(prefix) => prefix.ends_with('.'),
        None => false,
    }
}

/// Sorts candidate rows by the contract's `(file bytes, start_byte, id)` key.
fn sort_rows(mut rows: Vec<SymbolRow>) -> Vec<SymbolRow> {
    rows.sort_by(|a, b| {
        a.file
            .as_bytes()
            .cmp(b.file.as_bytes())
            .then(a.start_byte.cmp(&b.start_byte))
            .then_with(|| a.id.as_bytes().cmp(b.id.as_bytes()))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::{
        InvalidFileLine, QueryOutcome, check_query_syntax, classify_file_line, ends_with_component,
        last_component, levenshtein, normalize_separators, resolve_query,
    };
    use rivet_core::{ParseStatus, SymbolKind};
    use rivet_store::{FileRow, Store, SymbolRow};

    /// A type alias compares case-sensitively, as TypeScript identifiers do;
    /// a class still folds ASCII case, as PHP does (T42).
    #[test]
    fn type_alias_lookup_is_case_sensitive() {
        use super::lookup_name_matches;

        assert!(lookup_name_matches(
            "SurveyId",
            SymbolKind::TypeAlias,
            "SurveyId"
        ));
        assert!(!lookup_name_matches(
            "surveyid",
            SymbolKind::TypeAlias,
            "SurveyId"
        ));
        assert!(lookup_name_matches(
            "SurveyId",
            SymbolKind::Class,
            "surveyid"
        ));
    }

    #[test]
    fn normalizes_and_collapses_every_language_separator() {
        assert_eq!(
            normalize_separators("App\\Services\\SurveyService::launch"),
            "App.Services.SurveyService.launch"
        );
        assert_eq!(normalize_separators("a->b/c.d"), "a.b.c.d");
        // Runs collapse and a single leading separator is stripped.
        assert_eq!(normalize_separators("\\\\App\\\\Services"), "App.Services");
        assert_eq!(normalize_separators("//App//Services"), "App.Services");
        assert_eq!(normalize_separators("\\launch"), "launch");
        assert_eq!(normalize_separators("..App"), "App");
    }

    #[test]
    fn component_boundary_suffix_matching() {
        assert!(ends_with_component(
            "App.Services.Survey.launch",
            "Survey.launch"
        ));
        assert!(ends_with_component("Survey.launch", "Survey.launch"));
        assert!(!ends_with_component(
            "App.NotSurvey.launch",
            "Survey.launch"
        ));
        assert!(!ends_with_component("App.Survey.launchX", "Survey.launch"));
    }

    #[test]
    fn levenshtein_counts_unicode_scalars() {
        assert_eq!(levenshtein("launch", "launch"), 0);
        assert_eq!(levenshtein("launch", "lanch"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        // Umlaut is one scalar, not two bytes.
        assert_eq!(levenshtein("bär", "bar"), 1);
    }

    #[test]
    fn parses_only_positive_line_numbers() {
        assert_eq!(classify_file_line("a/b.php:82"), Some(Ok(("a/b.php", 82))));
        assert_eq!(classify_file_line("a.php:+7"), Some(Ok(("a.php", 7))));
        assert_eq!(classify_file_line("Foo::bar"), None);
        assert_eq!(classify_file_line("a.php:-3"), None);
        assert_eq!(classify_file_line("a.php:"), None);
        assert_eq!(classify_file_line("a.php:+"), None);
        assert_eq!(classify_file_line("launch"), None);
        assert_eq!(classify_file_line("a.php#App\\Foo::bar"), None);
    }

    /// Every syntactic rejection is decided from the text alone.
    #[test]
    fn query_syntax_rejects_each_invalid_file_line_form() {
        let reason = |query: &str| check_query_syntax(query).unwrap_err();
        assert_eq!(
            reason("a.php:0"),
            InvalidFileLine {
                path: "a.php".to_string(),
                reason: "lines are numbered from 1".to_string()
            }
        );
        assert_eq!(reason("a.php:+0").reason, "lines are numbered from 1");
        assert_eq!(reason("a.php:00").reason, "lines are numbered from 1");
        assert_eq!(
            reason("a.php:4294967296").reason,
            "the line number is out of range"
        );
        assert_eq!(reason(":3").reason, "the path is empty");
        assert_eq!(
            reason("/etc/passwd:1").reason,
            "absolute paths are not repository-relative"
        );
        assert_eq!(
            reason("../a.php:1").reason,
            "`..` path components are not allowed"
        );
        assert_eq!(
            reason("src/../a.php:1").reason,
            "`..` path components are not allowed"
        );
        for valid in [
            "a.php:4294967295",
            "a..b.php:1",
            "launch",
            "Foo::bar",
            "App\\Foo",
            "a.php#App\\f",
            "a.php:-1",
        ] {
            assert_eq!(check_query_syntax(valid), Ok(()), "{valid}");
        }
    }

    #[test]
    fn last_component_uses_the_trailing_name() {
        assert_eq!(
            last_component("App\\Services\\SurveyService::launch"),
            "launch"
        );
        assert_eq!(last_component("launch"), "launch");
    }

    fn file(path: &str) -> FileRow {
        FileRow {
            path: path.to_string(),
            language: Some("php".to_string()),
            mtime_ns: 0,
            size: 0,
            content_hash: None,
            source: None,
            parse_status: ParseStatus::Ok,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn symbol(
        file: &str,
        name: &str,
        qualified_name: &str,
        kind: SymbolKind,
        start_byte: u32,
        end_byte: u32,
        start_line: u32,
        end_line: u32,
    ) -> SymbolRow {
        SymbolRow {
            id: format!("{file}#{qualified_name}"),
            file: file.to_string(),
            name: name.to_string(),
            lookup_name: name.to_ascii_lowercase(),
            qualified_name: qualified_name.to_string(),
            kind,
            parent_id: None,
            start_byte,
            end_byte,
            start_line,
            end_line,
            signature: None,
            doc_comment: None,
        }
    }

    fn store_with(symbols: Vec<SymbolRow>) -> Store {
        let mut store = Store::open_in_memory().unwrap();
        for path in symbols
            .iter()
            .map(|row| row.file.clone())
            .collect::<Vec<_>>()
        {
            if store.get_file(&path).unwrap().is_none() {
                store.upsert_file(&file(&path)).unwrap();
            }
        }
        let mut by_file: std::collections::HashMap<String, Vec<SymbolRow>> =
            std::collections::HashMap::new();
        for row in symbols {
            by_file.entry(row.file.clone()).or_default().push(row);
        }
        for (path, rows) in by_file {
            store.replace_file_symbols(&path, &rows).unwrap();
        }
        store
    }

    fn symbols(outcome: QueryOutcome) -> Vec<SymbolRow> {
        match outcome {
            QueryOutcome::Symbols(rows) => rows,
            other => panic!("expected symbols, got {other:?}"),
        }
    }

    #[test]
    fn file_line_selects_the_innermost_symbol() {
        let store = store_with(vec![
            symbol(
                "a.php",
                "SurveyService",
                "App\\SurveyService",
                SymbolKind::Class,
                0,
                400,
                1,
                20,
            ),
            symbol(
                "a.php",
                "launch",
                "App\\SurveyService::launch",
                SymbolKind::Method,
                100,
                150,
                10,
                12,
            ),
        ]);
        let rows = symbols(resolve_query(&store, "a.php:11").unwrap());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].qualified_name, "App\\SurveyService::launch");
    }

    #[test]
    fn file_line_same_minimal_span_is_ambiguous() {
        let store = store_with(vec![
            symbol(
                "a.php",
                "one",
                "App\\one",
                SymbolKind::Function,
                10,
                20,
                3,
                3,
            ),
            symbol(
                "a.php",
                "two",
                "App\\two",
                SymbolKind::Function,
                30,
                40,
                3,
                3,
            ),
        ]);
        let rows = symbols(resolve_query(&store, "a.php:3").unwrap());
        assert_eq!(rows.len(), 2, "same-line declarations must not be guessed");
    }

    #[test]
    fn file_line_siblings_of_different_lengths_on_one_line_are_ambiguous() {
        // `function a() {} function bb() { function c() {} }` on line 3:
        // `a` and `c` are both innermost (neither contains the other), and
        // `bb` contains `c`, so it is not.
        let store = store_with(vec![
            symbol("a.php", "a", "App\\a", SymbolKind::Function, 10, 25, 3, 3),
            symbol("a.php", "bb", "App\\bb", SymbolKind::Function, 26, 70, 3, 3),
            symbol("a.php", "c", "App\\c", SymbolKind::Function, 40, 68, 3, 3),
        ]);
        let rows = symbols(resolve_query(&store, "a.php:3").unwrap());
        let names: Vec<&str> = rows.iter().map(|row| row.qualified_name.as_str()).collect();
        assert_eq!(names, vec!["App\\a", "App\\c"]);
    }

    #[test]
    fn paths_differing_only_in_case_are_distinct() {
        // A case-sensitive filesystem can hold both; the store keeps each path's
        // bytes, and neither the canonical ID nor `file:line` folds case.
        let store = store_with(vec![
            symbol("a.php", "f", "App\\f", SymbolKind::Function, 10, 20, 3, 3),
            symbol("A.php", "f", "App\\f", SymbolKind::Function, 10, 20, 3, 3),
        ]);
        let lower = symbols(resolve_query(&store, "a.php#App\\f").unwrap());
        assert_eq!(lower.len(), 1);
        assert_eq!(lower[0].file, "a.php");
        let upper = symbols(resolve_query(&store, "A.php#App\\f").unwrap());
        assert_eq!(upper.len(), 1);
        assert_eq!(upper[0].file, "A.php");
        assert_ne!(lower[0].id, upper[0].id);
        let line = symbols(resolve_query(&store, "A.php:3").unwrap());
        assert_eq!(line.len(), 1);
        assert_eq!(line[0].file, "A.php");
        // The short name matches both and is ambiguous, never folded to one.
        let both = symbols(resolve_query(&store, "f").unwrap());
        assert_eq!(both.len(), 2);
    }

    #[test]
    fn file_line_reports_missing_path_and_empty_line() {
        let store = store_with(vec![symbol(
            "a.php",
            "launch",
            "App\\launch",
            SymbolKind::Function,
            10,
            20,
            5,
            6,
        )]);
        match resolve_query(&store, "nope.php:3").unwrap() {
            QueryOutcome::PathNotIndexed { path } => assert_eq!(path, "nope.php"),
            other => panic!("expected PathNotIndexed, got {other:?}"),
        }
        match resolve_query(&store, "a.php:1").unwrap() {
            QueryOutcome::NoEnclosingSymbol { suggestions } => {
                assert_eq!(suggestions, vec!["App\\launch".to_string()]);
            }
            other => panic!("expected NoEnclosingSymbol, got {other:?}"),
        }
    }

    /// A stored file that carries no facts is reported with its status, for
    /// both `file:line` and a canonical ID naming it; an indexed file and an
    /// unknown path are not.
    #[test]
    fn file_line_and_id_report_a_stored_unindexed_file() {
        let store = store_with(vec![symbol(
            "a.php",
            "launch",
            "App\\launch",
            SymbolKind::Function,
            10,
            20,
            5,
            6,
        )]);
        let mut broken = file("broken.php");
        broken.parse_status = ParseStatus::ParseError;
        store.upsert_file(&broken).unwrap();
        let mut readme = file("README.md");
        readme.language = None;
        readme.parse_status = ParseStatus::Unsupported;
        store.upsert_file(&readme).unwrap();

        for query in ["broken.php:1", "broken.php#App\\Broken"] {
            match resolve_query(&store, query).unwrap() {
                QueryOutcome::FileNotIndexed {
                    path,
                    status,
                    language,
                } => {
                    assert_eq!(path, "broken.php");
                    assert_eq!(status, ParseStatus::ParseError);
                    assert_eq!(language.as_deref(), Some("php"));
                }
                other => panic!("{query}: expected FileNotIndexed, got {other:?}"),
            }
        }
        match resolve_query(&store, "README.md:1").unwrap() {
            QueryOutcome::FileNotIndexed {
                status, language, ..
            } => {
                assert_eq!(status, ParseStatus::Unsupported);
                assert_eq!(language, None);
            }
            other => panic!("expected FileNotIndexed, got {other:?}"),
        }
        // A missing name in an indexed file, and an ID into an unknown path,
        // fall through to an empty match rather than a file error.
        assert!(symbols(resolve_query(&store, "a.php#App\\missing").unwrap()).is_empty());
        assert!(symbols(resolve_query(&store, "nope.php#App\\launch").unwrap()).is_empty());
    }

    #[test]
    fn file_line_rejects_absolute_and_parent_paths() {
        let store = store_with(Vec::new());
        assert!(matches!(
            resolve_query(&store, "/etc/passwd:1").unwrap(),
            QueryOutcome::InvalidFileLine { .. }
        ));
        assert!(matches!(
            resolve_query(&store, "../a.php:1").unwrap(),
            QueryOutcome::InvalidFileLine { .. }
        ));
    }

    #[test]
    fn dotted_query_requires_a_component_boundary_and_case_follows_kind() {
        let store = store_with(vec![
            symbol(
                "a.php",
                "launch",
                "App\\Services\\SurveyService::launch",
                SymbolKind::Method,
                10,
                20,
                3,
                4,
            ),
            symbol(
                "a.php",
                "$label",
                "App\\Services\\SurveyService::$label",
                SymbolKind::Property,
                30,
                40,
                6,
                6,
            ),
        ]);
        // Case-insensitive for a method.
        let rows = symbols(resolve_query(&store, "services.surveyservice.LAUNCH").unwrap());
        assert_eq!(rows.len(), 1);
        // No match across a component boundary.
        assert!(symbols(resolve_query(&store, "Service.launch").unwrap()).is_empty());
        // A property keeps its exact case and leading `$`.
        assert!(
            symbols(resolve_query(&store, "App\\Services\\SurveyService::$Label").unwrap())
                .is_empty()
        );
    }

    #[test]
    fn suggestions_rank_by_last_component_distance() {
        let store = store_with(vec![
            symbol(
                "a.php",
                "launch",
                "App\\SurveyService::launch",
                SymbolKind::Method,
                10,
                20,
                3,
                4,
            ),
            symbol(
                "a.php",
                "relaunch",
                "App\\SurveyService::relaunch",
                SymbolKind::Method,
                30,
                40,
                6,
                7,
            ),
        ]);
        let ranked = super::suggestions(&store, "App\\SurveyService::lunch").unwrap();
        assert_eq!(ranked[0], "App\\SurveyService::launch");
    }

    #[test]
    fn case_insensitive_forms_fold_ascii_only() {
        // PHP folds ASCII letters only (AF2): `Ärger` and `ärger` are distinct
        // classes, while `Foo` is still found as `FOO`.
        let store = store_with(vec![
            symbol("a.php", "Foo", "App\\Foo", SymbolKind::Class, 0, 10, 1, 1),
            symbol(
                "a.php",
                "Ärger",
                "App\\Ärger",
                SymbolKind::Class,
                20,
                30,
                2,
                2,
            ),
        ]);
        let names = |query: &str| -> Vec<String> {
            symbols(resolve_query(&store, query).unwrap())
                .into_iter()
                .map(|row| row.qualified_name)
                .collect()
        };
        assert_eq!(names("FOO"), vec!["App\\Foo"]);
        assert_eq!(names("app\\foo"), vec!["App\\Foo"]);
        assert_eq!(names("app.FOO"), vec!["App\\Foo"]);
        assert_eq!(names("Ärger"), vec!["App\\Ärger"]);
        assert_eq!(names("ÄRGER"), vec!["App\\Ärger"]);
        assert_eq!(names("APP\\ÄRGER"), vec!["App\\Ärger"]);
        assert!(names("ärger").is_empty());
        assert!(names("app\\ärger").is_empty());
        assert!(names("app.ärger").is_empty());
    }
}
