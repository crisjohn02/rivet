//! Declaration resolution, query matching, context ranking and fitting.
//!
//! T12 implements the symbol query forms of spec §10.2 against persisted rows:
//! canonical ID, native qualified name, dotted path, and short name. `file:line`
//! and query normalization for references are later tasks.

use rivet_core::{SymbolId, SymbolKind};
use rivet_store::{Error, Store, SymbolRow};

/// Resolves `query` against the persisted symbols of `store`.
///
/// Forms are tried in the order spec §10.2 lists and resolution stops at the
/// first form that yields at least one match:
///
/// 1. a canonical ID containing `#`;
/// 2. a native qualified name containing `\` or `::` (exact, then
///    case-insensitively for case-insensitive kinds);
/// 3. a dotted path, comparing separator-normalized qualified names at a
///    component boundary;
/// 4. a short name, matched against the persisted `lookup_name`.
///
/// The returned rows follow the store's `(file bytes, start_byte, id)` order.
pub fn resolve_query(store: &Store, query: &str) -> Result<Vec<SymbolRow>, Error> {
    // (1) Canonical ID: only a query with `#` can be one.
    if query.contains('#')
        && let Ok(id) = SymbolId::parse(query)
        && let Some(row) = store.get_symbol(&id.as_canonical())?
    {
        return Ok(vec![row]);
    }

    // (2) Native qualified name.
    if query.contains('\\') || query.contains("::") {
        let exact = store.find_symbols_by_qualified_name(query)?;
        if !exact.is_empty() {
            return Ok(exact);
        }
        let folded = query.to_lowercase();
        let case_folded: Vec<SymbolRow> = store
            .list_symbols()?
            .into_iter()
            .filter(|row| {
                case_insensitive_kind(row.kind) && row.qualified_name.to_lowercase() == folded
            })
            .collect();
        if !case_folded.is_empty() {
            return Ok(case_folded);
        }
    }

    // (3) Dotted path: normalize every separator to `.` on both sides and match
    // complete trailing components.
    let normalized_query = normalize_separators(query);
    if normalized_query.contains('.') {
        let matches: Vec<SymbolRow> = store
            .list_symbols()?
            .into_iter()
            .filter(|row| {
                ends_with_component(
                    &normalize_separators(&row.qualified_name),
                    &normalized_query,
                )
            })
            .collect();
        if !matches.is_empty() {
            return Ok(matches);
        }
    }

    // (4) Short name. PHP identifiers are case-insensitive except properties and
    // constants, so also try the lowercased spelling when it differs.
    let mut short = store.find_symbols_by_lookup_name(query)?;
    let lowered = query.to_lowercase();
    if lowered != query {
        for row in store.find_symbols_by_lookup_name(&lowered)? {
            if !short.iter().any(|existing| existing.id == row.id) {
                short.push(row);
            }
        }
        short.sort_by(|a, b| {
            a.file
                .as_bytes()
                .cmp(b.file.as_bytes())
                .then(a.start_byte.cmp(&b.start_byte))
                .then_with(|| a.id.as_bytes().cmp(b.id.as_bytes()))
        });
    }
    Ok(short)
}

/// Returns up to five distinct qualified names nearest to `query`.
///
/// Suggestions are ordered by Unicode-scalar Levenshtein distance to `query`,
/// then by name bytes (OUTPUT-CONTRACT "Ordering and versioning").
pub fn suggestions(store: &Store, query: &str) -> Result<Vec<String>, Error> {
    let mut seen = std::collections::BTreeSet::new();
    let mut ranked: Vec<(usize, String)> = Vec::new();
    for row in store.list_symbols()? {
        if seen.insert(row.qualified_name.clone()) {
            ranked.push((
                levenshtein(query, &row.qualified_name),
                row.qualified_name.clone(),
            ));
        }
    }
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.as_bytes().cmp(b.1.as_bytes()))
    });
    Ok(ranked.into_iter().take(5).map(|(_, name)| name).collect())
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

/// Replaces every language-native path separator with `.`.
fn normalize_separators(value: &str) -> String {
    value
        .replace("::", ".")
        .replace("->", ".")
        .replace(['\\', '/'], ".")
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

#[cfg(test)]
mod tests {
    use super::{ends_with_component, levenshtein, normalize_separators};

    #[test]
    fn normalizes_every_language_separator() {
        assert_eq!(
            normalize_separators("App\\Services\\SurveyService::launch"),
            "App.Services.SurveyService.launch"
        );
        assert_eq!(normalize_separators("a->b/c.d"), "a.b.c.d");
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
}
