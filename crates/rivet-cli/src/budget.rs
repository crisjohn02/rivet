//! Context source estimation and greedy budget fitting (spec §16.2, §16.4
//! items 1 and 2; T28).
//!
//! Every ranked context candidate has two representations ([`Forms`]): `full`,
//! the stored source slice of its declaration span, and `signature`, the
//! collapsed form of spec §16.2 (doc comment, declaration line, and for a
//! container its member signatures with bodies replaced by `{ … }`). Each
//! final source string is estimated as `ceil(UTF8_bytes / 3)`
//! ([`estimate_tokens`], OUTPUT-CONTRACT `rivet context`), in integer
//! arithmetic.
//!
//! [`fit`] walks the ranked candidates once and greedily keeps the first
//! allowed form of each that still fits the remaining budget:
//!
//! - The target tries `full` first in every mode, then `signature` under
//!   `auto` and `always`. If no allowed target form fits, the result is error
//!   8 `budget_too_small` with `required_tokens`, the smallest estimate among
//!   the target's allowed forms.
//! - Every other candidate, in rank order: `auto` tries `full` then
//!   `signature`, `always` only `signature`, `never` only `full`. A candidate
//!   with no allowed form that fits is skipped and fitting continues, so a
//!   smaller later candidate can still fit.
//!
//! A source string is never truncated. The fitted [`Fitted`] result therefore
//! always satisfies `estimated_tokens <= budget_tokens`.
//!
//! Overlap suppression, `--limit`, and the omission counts are T29; command
//! wiring and JSON are T30. [`Fitted`] is the intermediate structure they
//! consume.

use std::collections::HashMap;

use rivet_core::{Collapse, ExtractedSymbol, Span};
use rivet_store::{Store, SymbolRow};

use crate::context::{Candidate, Reason};
use crate::symbol::{span_text, stored_source};
use crate::transport::CliError;

/// The contract's `tokenizer` value for [`estimate_tokens`].
pub const TOKENIZER: &str = "utf8-bytes-v1";

/// The estimate of one final source string: `ceil(UTF8_bytes / 3)`.
///
/// Integer arithmetic only. The empty string is 0; one to three bytes are 1;
/// four bytes are 2. The `…` in a collapsed body is three UTF-8 bytes and
/// counts as such.
pub fn estimate_tokens(source: &str) -> u64 {
    (source.len() as u64).div_ceil(3)
}

/// Which representation of a candidate a segment carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Form {
    /// The stored source slice of the declaration span.
    Full,
    /// The collapsed form of spec §16.2.
    Signature,
}

impl Form {
    /// The contract spelling emitted as a segment's `form`.
    pub fn as_str(self) -> &'static str {
        match self {
            Form::Full => "full",
            Form::Signature => "signature",
        }
    }
}

/// The two representations of one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forms {
    /// The declaration span's stored source slice.
    pub full: String,
    /// The collapsed form, or `None` when the candidate has none (no stored
    /// signature, or a language without a collapsed-form renderer). An
    /// unavailable signature is never an allowed form.
    pub signature: Option<String>,
}

impl Forms {
    /// The source string for `form`, if the candidate has one.
    fn get(&self, form: Form) -> Option<&str> {
        match form {
            Form::Full => Some(self.full.as_str()),
            Form::Signature => self.signature.as_deref(),
        }
    }
}

/// One fitted segment: a candidate, the form chosen, its final source string,
/// and that string's estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The ranked candidate this segment renders.
    pub candidate: Candidate,
    /// The chosen representation.
    pub form: Form,
    /// The final source string, exactly as it will be emitted.
    pub source: String,
    /// `estimate_tokens(&source)`.
    pub estimated_tokens: u64,
}

/// The greedy fit of a ranked candidate list into a token budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fitted {
    /// The requested budget.
    pub budget_tokens: u64,
    /// Segments in inclusion order (rank order), the target first.
    pub segments: Vec<Segment>,
    /// The sum of every segment's estimate; never above `budget_tokens`.
    pub estimated_tokens: u64,
}

/// The forms the target may take under `collapse`, in the order tried.
///
/// The target always tries `full` first, even under `always` (spec §16.4:
/// "`--collapse always` still prefers the target in full if it fits").
fn target_forms(collapse: Collapse) -> &'static [Form] {
    match collapse {
        Collapse::Auto | Collapse::Always => &[Form::Full, Form::Signature],
        Collapse::Never => &[Form::Full],
    }
}

/// The forms any non-target candidate may take under `collapse`, in the order
/// tried.
fn related_forms(collapse: Collapse) -> &'static [Form] {
    match collapse {
        Collapse::Auto => &[Form::Full, Form::Signature],
        Collapse::Always => &[Form::Signature],
        Collapse::Never => &[Form::Full],
    }
}

/// The first allowed form of `forms` whose estimate fits in `remaining`.
fn first_fitting(forms: &Forms, allowed: &[Form], remaining: u64) -> Option<(Form, String, u64)> {
    allowed.iter().find_map(|&form| {
        let source = forms.get(form)?;
        let estimate = estimate_tokens(source);
        (estimate <= remaining).then(|| (form, source.to_string(), estimate))
    })
}

/// Greedily fits ranked `entries` into `budget_tokens` (spec §16.4 items 1
/// and 2).
///
/// `entries` are in rank order and the first must be the target (reason
/// `target`), as [`crate::context::collect_ranked`] returns them. A later
/// entry is always treated as a related candidate. The result's
/// `estimated_tokens` never exceeds `budget_tokens`; a segment whose estimate
/// equals the remaining budget fits.
///
/// Errors: `budget_too_small` (exit 8) when no allowed target form fits, with
/// `required_tokens` the smallest estimate among the target's allowed and
/// available forms; `general` when `entries` does not start with the target.
pub fn fit(
    entries: &[(Candidate, Forms)],
    collapse: Collapse,
    budget_tokens: u64,
) -> Result<Fitted, CliError> {
    let Some((target, target_source)) = entries.first() else {
        return Err(CliError::general(
            "context fitting received no candidates",
            "Collect candidates before fitting; the target is always first.",
        ));
    };
    if target.reason != Reason::Target {
        return Err(CliError::general(
            format!(
                "context fitting expected the target first, found reason `{}`",
                target.reason.as_str()
            ),
            "Pass candidates in rank order, as collection returns them.",
        ));
    }

    let allowed = target_forms(collapse);
    let Some((form, source, estimate)) = first_fitting(target_source, allowed, budget_tokens)
    else {
        return Err(target_too_large(
            target_source,
            allowed,
            collapse,
            budget_tokens,
        ));
    };
    let mut used = estimate;
    let mut segments = vec![Segment {
        candidate: target.clone(),
        form,
        source,
        estimated_tokens: estimate,
    }];

    let allowed = related_forms(collapse);
    for (candidate, forms) in &entries[1..] {
        // Skip, but keep going: a smaller later candidate may still fit.
        let remaining = budget_tokens - used;
        if let Some((form, source, estimate)) = first_fitting(forms, allowed, remaining) {
            used += estimate;
            segments.push(Segment {
                candidate: candidate.clone(),
                form,
                source,
                estimated_tokens: estimate,
            });
        }
    }

    debug_assert!(used <= budget_tokens);
    Ok(Fitted {
        budget_tokens,
        segments,
        estimated_tokens: used,
    })
}

/// The `budget_too_small` error for a target none of whose allowed forms fit.
fn target_too_large(
    forms: &Forms,
    allowed: &[Form],
    collapse: Collapse,
    budget_tokens: u64,
) -> CliError {
    // `full` is always allowed and always available, so the minimum exists.
    let required = allowed
        .iter()
        .filter_map(|&form| forms.get(form).map(estimate_tokens))
        .min()
        .unwrap_or_else(|| estimate_tokens(&forms.full));
    let collapsed = forms.signature.as_deref().map(estimate_tokens);
    let hint = match (collapse, collapsed) {
        (Collapse::Never, Some(signature)) if signature < required => format!(
            "Re-run with `--tokens {required}` or higher, or allow the collapsed target \
             ({signature} estimated tokens) with `--collapse auto`."
        ),
        _ => format!("Re-run with `--tokens {required}` or higher."),
    };
    CliError::budget_too_small(
        format!(
            "the context target needs at least {required} estimated tokens, but the budget is \
             {budget_tokens}"
        ),
        hint,
        budget_tokens,
        required,
    )
}

/// Builds each candidate's [`Forms`] from the snapshot's stored source bytes.
///
/// `full` is the declaration span sliced from the stored bytes, never the
/// file on disk. `signature` is built through
/// [`rivet_languages::signature_summary`], dispatched by the file's language,
/// from the stored signature, the stored declaration spans of the candidate
/// and its direct members, and the stored source text; the candidate's stored
/// doc comment, when it has one, is prepended on its own line, because spec
/// §16.2 puts the doc comment in the signature form and the adapter's summary
/// does not include it. The signature is unavailable (`None`) when the row has
/// no stored signature, its file maps to no language with a renderer, or the
/// stored bytes are not UTF-8 (the spans would not index the decoded text).
///
/// The result is parallel to `candidates`.
pub fn candidate_forms(store: &Store, candidates: &[Candidate]) -> Result<Vec<Forms>, CliError> {
    let symbols = store.list_symbols().map_err(crate::index::store_error)?;
    // Members per parent in `list_symbols` order; the language orders them.
    let mut members: HashMap<&str, Vec<&SymbolRow>> = HashMap::new();
    for row in &symbols {
        if let Some(parent) = row.parent_id.as_deref() {
            members.entry(parent).or_default().push(row);
        }
    }

    let mut sources: HashMap<String, Vec<u8>> = HashMap::new();
    let mut result = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let row = &candidate.symbol;
        if !sources.contains_key(&row.file) {
            let bytes = stored_source(store, &row.file)?;
            sources.insert(row.file.clone(), bytes);
        }
        let bytes = &sources[&row.file];
        let full = span_text(bytes, row)?;
        let signature = signature_form(
            row,
            members.get(row.id.as_str()).map_or(&[][..], Vec::as_slice),
            bytes,
        );
        result.push(Forms { full, signature });
    }
    Ok(result)
}

/// Collects forms and fits them in one step (see [`candidate_forms`] and
/// [`fit`]).
pub fn fit_candidates(
    store: &Store,
    candidates: &[Candidate],
    collapse: Collapse,
    budget_tokens: u64,
) -> Result<Fitted, CliError> {
    let forms = candidate_forms(store, candidates)?;
    let entries: Vec<(Candidate, Forms)> = candidates.iter().cloned().zip(forms).collect();
    fit(&entries, collapse, budget_tokens)
}

/// The spec §16.2 signature form of `row`: its doc comment (if any), then the
/// language's collapsed summary.
fn signature_form(row: &SymbolRow, members: &[&SymbolRow], bytes: &[u8]) -> Option<String> {
    row.signature.as_ref()?;
    let language = rivet_languages::language_for_path(&row.file)?;
    let source = std::str::from_utf8(bytes).ok()?;
    let symbol = extracted(row)?;
    let members: Vec<ExtractedSymbol> = members
        .iter()
        .map(|member| extracted(member))
        .collect::<Option<_>>()?;
    let summary = rivet_languages::signature_summary(language, &symbol, &members, source)?;
    Some(match row.doc_comment.as_deref() {
        Some(doc) => format!("{doc}\n{summary}"),
        None => summary,
    })
}

/// Bridges a stored row to the adapter's input record.
///
/// `parent_index` is `None`: the summary reads spans, kinds, names, and
/// signatures, never parent indices, and members are passed separately.
fn extracted(row: &SymbolRow) -> Option<ExtractedSymbol> {
    Some(ExtractedSymbol {
        qualified_name: row.qualified_name.clone(),
        name: row.name.clone(),
        kind: row.kind,
        span: Span::new(row.start_byte, row.end_byte).ok()?,
        parent_index: None,
        signature: row.signature.clone(),
        doc_comment: row.doc_comment.clone(),
    })
}

#[cfg(test)]
mod tests {
    use rivet_core::{Collapse, Resolution, SymbolKind};
    use rivet_store::SymbolRow;

    use super::{Fitted, Form, Forms, estimate_tokens, fit};
    use crate::context::{Candidate, Reason};

    fn candidate(name: &str, reason: Reason) -> Candidate {
        Candidate {
            symbol: SymbolRow {
                id: format!("F.php#{name}"),
                file: "F.php".to_string(),
                name: name.to_string(),
                lookup_name: name.to_lowercase(),
                qualified_name: name.to_string(),
                kind: SymbolKind::Function,
                parent_id: None,
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
                end_line: 1,
                signature: None,
                doc_comment: None,
            },
            reason,
            resolution: Resolution::Exact,
        }
    }

    /// A string of exactly `bytes` ASCII bytes.
    fn bytes(count: usize) -> String {
        "x".repeat(count)
    }

    fn forms(full: usize, signature: Option<usize>) -> Forms {
        Forms {
            full: bytes(full),
            signature: signature.map(bytes),
        }
    }

    /// `(name, form)` per segment, for exact assertions.
    fn shape(fitted: &Fitted) -> Vec<(String, Form)> {
        fitted
            .segments
            .iter()
            .map(|segment| (segment.candidate.symbol.name.clone(), segment.form))
            .collect()
    }

    #[test]
    fn estimate_rounds_up_per_three_bytes() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("ab"), 1);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 2);
        assert_eq!(estimate_tokens("abcdef"), 2);
        assert_eq!(estimate_tokens("abcdefg"), 3);
    }

    #[test]
    fn estimate_counts_utf8_bytes_not_characters() {
        // The ellipsis is three UTF-8 bytes: one token alone, two with a space.
        assert_eq!("…".len(), 3);
        assert_eq!(estimate_tokens("…"), 1);
        assert_eq!(estimate_tokens(" …"), 2);
        // ` { … }` is 1 + 1 + 1 + 3 + 1 + 1 = 8 bytes: ceil(8/3) = 3.
        assert_eq!(" { … }".len(), 8);
        assert_eq!(estimate_tokens(" { … }"), 3);
        // "é" is two bytes and "漢" three: "é漢" is 5 bytes, 2 tokens, although
        // it is only 2 characters.
        assert_eq!(estimate_tokens("é漢"), 2);
        // A 4-byte scalar: 4 bytes, 2 tokens.
        assert_eq!(estimate_tokens("😀"), 2);
    }

    #[test]
    fn target_fitting_exactly_at_the_budget_fits_in_full() {
        // full: 30 bytes = 10 tokens; signature: 9 bytes = 3 tokens.
        let entries = vec![(candidate("t", Reason::Target), forms(30, Some(9)))];
        let fitted = fit(&entries, Collapse::Auto, 10).expect("fits exactly");
        assert_eq!(shape(&fitted), vec![("t".to_string(), Form::Full)]);
        assert_eq!(fitted.estimated_tokens, 10);
        assert_eq!(fitted.segments[0].source, bytes(30));
    }

    #[test]
    fn target_one_token_under_fails_over_to_its_signature() {
        let entries = vec![(candidate("t", Reason::Target), forms(30, Some(9)))];
        for collapse in [Collapse::Auto, Collapse::Always] {
            let fitted = fit(&entries, collapse, 9).expect("signature fits");
            assert_eq!(shape(&fitted), vec![("t".to_string(), Form::Signature)]);
            assert_eq!(fitted.segments[0].source, bytes(9));
            assert_eq!(fitted.estimated_tokens, 3);
        }
        // `never` has no fallback.
        let error = fit(&entries, Collapse::Never, 9).unwrap_err();
        assert_eq!((error.code, error.exit), ("budget_too_small", 8));
    }

    #[test]
    fn always_still_emits_the_target_in_full_when_it_fits() {
        let entries = vec![
            (candidate("t", Reason::Target), forms(30, Some(9))),
            (candidate("a", Reason::Callee), forms(30, Some(9))),
        ];
        let fitted = fit(&entries, Collapse::Always, 100).expect("fits");
        assert_eq!(
            shape(&fitted),
            vec![
                ("t".to_string(), Form::Full),
                ("a".to_string(), Form::Signature),
            ]
        );
        assert_eq!(fitted.estimated_tokens, 13);
    }

    /// One candidate set, three modes, three different results.
    #[test]
    fn the_three_modes_differ_on_one_candidate_set() {
        // Tokens: t full 10 / sig 3; a full 20 / sig 4; b full 5 / sig 2;
        // c full 4, no signature.
        let entries = vec![
            (candidate("t", Reason::Target), forms(30, Some(9))),
            (candidate("a", Reason::Type), forms(60, Some(12))),
            (candidate("b", Reason::Callee), forms(15, Some(6))),
            (candidate("c", Reason::Caller), forms(12, None)),
        ];
        let budget = 22;

        // auto: t full (10), a full 20 > 12 left -> a sig (4), b full (5),
        // c full 4 > 3 left -> no signature -> skipped. Total 19.
        let auto = fit(&entries, Collapse::Auto, budget).expect("auto");
        assert_eq!(
            shape(&auto),
            vec![
                ("t".to_string(), Form::Full),
                ("a".to_string(), Form::Signature),
                ("b".to_string(), Form::Full),
            ]
        );
        assert_eq!(auto.estimated_tokens, 19);

        // always: t full (10), a sig (4), b sig (2), c has no signature ->
        // skipped. Total 16.
        let always = fit(&entries, Collapse::Always, budget).expect("always");
        assert_eq!(
            shape(&always),
            vec![
                ("t".to_string(), Form::Full),
                ("a".to_string(), Form::Signature),
                ("b".to_string(), Form::Signature),
            ]
        );
        assert_eq!(always.estimated_tokens, 16);

        // never: t full (10), a full 20 > 12 -> skipped, b full (5), c full
        // (4). Total 19.
        let never = fit(&entries, Collapse::Never, budget).expect("never");
        assert_eq!(
            shape(&never),
            vec![
                ("t".to_string(), Form::Full),
                ("b".to_string(), Form::Full),
                ("c".to_string(), Form::Full),
            ]
        );
        assert_eq!(never.estimated_tokens, 19);
    }

    #[test]
    fn a_skipped_large_candidate_does_not_stop_a_smaller_later_one() {
        let entries = vec![
            (candidate("t", Reason::Target), forms(3, None)),
            (candidate("big", Reason::Type), forms(300, Some(60))),
            (candidate("small", Reason::Caller), forms(6, Some(3))),
        ];
        let fitted = fit(&entries, Collapse::Auto, 5).expect("fits");
        assert_eq!(
            shape(&fitted),
            vec![
                ("t".to_string(), Form::Full),
                ("small".to_string(), Form::Full),
            ]
        );
        assert_eq!(fitted.estimated_tokens, 3);
    }

    #[test]
    fn never_reports_the_full_estimate_and_auto_the_signature_estimate() {
        let entries = vec![(candidate("t", Reason::Target), forms(31, Some(10)))];
        // full 31 bytes = 11 tokens; signature 10 bytes = 4 tokens.
        let never = fit(&entries, Collapse::Never, 3).unwrap_err();
        assert_eq!((never.code, never.exit), ("budget_too_small", 8));
        assert_eq!(
            never.extra.get("budget_tokens"),
            Some(&serde_json::json!(3))
        );
        assert_eq!(
            never.extra.get("required_tokens"),
            Some(&serde_json::json!(11))
        );

        for collapse in [Collapse::Auto, Collapse::Always] {
            let error = fit(&entries, collapse, 3).unwrap_err();
            assert_eq!((error.code, error.exit), ("budget_too_small", 8));
            assert_eq!(
                error.extra.get("required_tokens"),
                Some(&serde_json::json!(4))
            );
        }

        // Without a signature, `auto`'s only allowed form is full.
        let bare = vec![(candidate("t", Reason::Target), forms(31, None))];
        let error = fit(&bare, Collapse::Auto, 3).unwrap_err();
        assert_eq!(
            error.extra.get("required_tokens"),
            Some(&serde_json::json!(11))
        );
    }

    #[test]
    fn zero_budget_fits_only_an_empty_target() {
        let entries = vec![(candidate("t", Reason::Target), forms(1, Some(1)))];
        let error = fit(&entries, Collapse::Auto, 0).unwrap_err();
        assert_eq!(
            error.extra.get("required_tokens"),
            Some(&serde_json::json!(1))
        );
    }

    #[test]
    fn fitting_requires_the_target_first() {
        let entries = vec![(candidate("a", Reason::Callee), forms(3, None))];
        let error = fit(&entries, Collapse::Auto, 10).unwrap_err();
        assert_eq!(error.code, "general");
        let error = fit(&[], Collapse::Auto, 10).unwrap_err();
        assert_eq!(error.code, "general");
    }

    /// Every budget from 0 to past the sum of all full forms, in every mode,
    /// on a candidate set with uneven sizes (including multibyte strings and
    /// a missing signature): a success never exceeds the budget, its total is
    /// the sum of its segments, each segment's estimate is its string's, no
    /// segment is a partial body, and an error happens only when no allowed
    /// target form fits.
    #[test]
    fn no_success_ever_exceeds_the_budget_across_a_sweep() {
        let multibyte = |chars: usize| "é…".repeat(chars);
        let mut entries = vec![(
            candidate("t", Reason::Target),
            Forms {
                full: multibyte(9),
                signature: Some(multibyte(2)),
            },
        )];
        let sizes: [(usize, Option<usize>); 9] = [
            (100, Some(20)),
            (7, Some(4)),
            (1, None),
            (47, Some(47)),
            (13, Some(2)),
            (250, None),
            (2, Some(1)),
            (64, Some(9)),
            (5, Some(8)),
        ];
        for (index, (full, signature)) in sizes.into_iter().enumerate() {
            entries.push((
                candidate(&format!("c{index}"), Reason::Callee),
                forms(full, signature),
            ));
        }
        let total_full: u64 = entries
            .iter()
            .map(|(_, forms)| estimate_tokens(&forms.full))
            .sum();

        for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
            let mut previous_total = 0;
            for budget in 0..=total_full + 5 {
                match fit(&entries, collapse, budget) {
                    Ok(fitted) => {
                        assert!(fitted.estimated_tokens <= budget, "{collapse:?} {budget}");
                        let sum: u64 = fitted.segments.iter().map(|s| s.estimated_tokens).sum();
                        assert_eq!(sum, fitted.estimated_tokens);
                        assert_eq!(fitted.budget_tokens, budget);
                        assert_eq!(fitted.segments[0].candidate.reason, Reason::Target);
                        for segment in &fitted.segments {
                            assert_eq!(segment.estimated_tokens, estimate_tokens(&segment.source));
                            let forms = &entries
                                .iter()
                                .find(|(c, _)| c.symbol.id == segment.candidate.symbol.id)
                                .expect("segment names an entry")
                                .1;
                            let expected = match segment.form {
                                Form::Full => Some(forms.full.as_str()),
                                Form::Signature => forms.signature.as_deref(),
                            };
                            assert_eq!(Some(segment.source.as_str()), expected);
                        }
                        // Under `never` every segment is full, under `always`
                        // every non-target segment is a signature.
                        for segment in &fitted.segments[1..] {
                            match collapse {
                                Collapse::Never => assert_eq!(segment.form, Form::Full),
                                Collapse::Always => assert_eq!(segment.form, Form::Signature),
                                Collapse::Auto => {}
                            }
                        }
                        // One more token changes the greedy walk only at the
                        // first candidate whose decision differs, which then
                        // consumes the budget exactly; so the total never
                        // shrinks as the budget grows, and when it changes it
                        // becomes the budget.
                        assert!(fitted.estimated_tokens >= previous_total);
                        if fitted.estimated_tokens != previous_total {
                            assert_eq!(fitted.estimated_tokens, budget, "{collapse:?}");
                        }
                        previous_total = fitted.estimated_tokens;
                    }
                    Err(error) => {
                        assert_eq!(error.code, "budget_too_small");
                        let required = error.extra["required_tokens"].as_u64().unwrap();
                        assert!(required > budget, "{collapse:?} {budget}");
                        let full = estimate_tokens(&entries[0].1.full);
                        let signature = estimate_tokens(entries[0].1.signature.as_ref().unwrap());
                        let expected = if collapse == Collapse::Never {
                            full
                        } else {
                            full.min(signature)
                        };
                        assert_eq!(required, expected);
                    }
                }
            }
        }
    }

    #[test]
    fn fitting_is_deterministic() {
        let entries = vec![
            (candidate("t", Reason::Target), forms(30, Some(9))),
            (candidate("a", Reason::Type), forms(60, Some(12))),
            (candidate("b", Reason::Callee), forms(15, Some(6))),
        ];
        for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
            for budget in [10, 15, 30, 100] {
                let first = fit(&entries, collapse, budget).expect("fits");
                let second = fit(&entries, collapse, budget).expect("fits");
                assert_eq!(first, second);
            }
        }
    }
}
