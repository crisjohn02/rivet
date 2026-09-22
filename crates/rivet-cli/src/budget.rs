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
//! [`fit`] is the T28 primitive and knows nothing about overlap.
//! [`fit_context`] (T29, spec §16.4 items 3 and 4) is the same greedy walk
//! with overlap suppression, the `--limit` segment cap, and the omission
//! counts; [`ContextFit`] is the intermediate structure that
//! [`crate::context_cmd`] (T30) renders as JSON. With no overlapping candidates and a
//! limit of at least the candidate count, [`fit_context`] emits exactly
//! [`fit`]'s segments.
//!
//! Overlap rules applied by [`fit_context`] (two candidates in different
//! files never overlap). Each emitted segment *shows* byte regions of its
//! file: a `full` segment its whole declaration span; a `signature` its
//! declaration header (from the declaration start to the end of its stored
//! signature text in the source, or the whole span when that cannot be
//! located) and, for a container, each listed member's header.
//!
//! - *Containment.* A `full` emitted segment suppresses every later candidate
//!   whose span lies within its span, bounds included: an equal span is
//!   contained, because two symbols sharing one declaration span (the
//!   members of `public int $a, $b;`) have byte-identical full text, so the
//!   second would repeat the first's body verbatim. A `signature` segment is
//!   a derived summary, not a source slice, so span containment alone
//!   suppresses nothing: a method nested in a signature-emitted container,
//!   or a function declared in the body of a signature-emitted function, is
//!   still emitted. Only source the signature literally shows counts: a
//!   promoted property lies in its constructor's header, so it is overlap
//!   once that header is emitted.
//! - *Ancestors.* A later candidate whose span contains an emitted segment is
//!   an ancestor of it; its full body would repeat that segment, so only its
//!   signature form can be allowed, and only when its header does not show
//!   the emitted region. Under `never`, or with no such signature, it is
//!   skipped as `overlap`.
//! - *Container summaries.* A container's signature form omits every direct
//!   member whose header overlaps an emitted region: a member emitted as its
//!   own segment (in either form), a member inside an emitted `full`
//!   segment, and a member whose header holds an emitted segment (a
//!   constructor once one of its promoted properties is emitted). Members
//!   nested inside an omitted member are omitted with it, so a promoted
//!   property never resurfaces as a standalone line. The summary is always
//!   re-rendered through [`rivet_languages::signature_summary`] with the
//!   reduced member list, and the doc comment is prepended as in T28.
//!   Two signature regions of different declarations sharing one span (two
//!   names of one multi-name declaration) name different elements and do not
//!   overlap.
//! - *The ordering trap.* Candidates are visited in rank order, so a
//!   container can be emitted as a signature before one of its members is
//!   visited. When that member is later emitted, every already-emitted
//!   container summary listing it is re-rendered without it. The candidate's
//!   cost is the net change of the total: its own estimate plus the change of
//!   every summary it reduces. A candidate is accepted only when that net
//!   total still fits, so the budget holds by construction and does not
//!   depend on re-rendering being monotone.
//!
//! Omission counts cover every non-emitted candidate exactly once, in the
//! contract's order: `overlap` (it has an allowed form, and every allowed
//! form it has would repeat emitted source), then `limit` (the segment cap
//! was already reached), then `budget` (no allowed, non-overlapping form
//! fits, including a candidate with no allowed form available at all, such
//! as a candidate without a signature under `always`).

use std::collections::HashMap;
use std::sync::Arc;

use rivet_core::{Collapse, ExtractedSymbol, Span};
use rivet_languages::LanguageId;
use rivet_store::{Store, SymbolRow};

use crate::context::{Candidate, Collection, Reason};
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
    Ok(context_entries(store, candidates)?
        .into_iter()
        .map(|entry| entry.forms)
        .collect())
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

/// The contract's default `--limit` for `context`: at most 50 segments, the
/// target included.
pub const DEFAULT_SEGMENT_LIMIT: usize = 50;

/// One direct member of a container, as its summary renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    id: String,
    symbol: ExtractedSymbol,
    /// The end of the member's declaration header in the source (see
    /// [`header_end`]); `None` when it cannot be located.
    header_end: Option<u32>,
}

/// Everything needed to re-render a candidate's signature form with some of
/// its direct members omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Summary {
    language: LanguageId,
    symbol: ExtractedSymbol,
    /// Direct members in stored-row order; the language orders them when
    /// rendering.
    members: Vec<Member>,
    /// The declaring file's stored text.
    source: Arc<str>,
    doc_comment: Option<String>,
}

impl Summary {
    /// The signature form parts for `row`, its direct `members`, and the
    /// stored `source`, or `None` when the form is unavailable (see
    /// [`candidate_forms`]).
    fn build(row: &SymbolRow, members: &[&SymbolRow], source: &Arc<str>) -> Option<Summary> {
        row.signature.as_ref()?;
        let language = rivet_languages::language_for_path(&row.file)?;
        let symbol = extracted(row)?;
        let members = members
            .iter()
            .map(|member| {
                Some(Member {
                    id: member.id.clone(),
                    symbol: extracted(member)?,
                    header_end: header_end(member, source),
                })
            })
            .collect::<Option<_>>()?;
        Some(Summary {
            language,
            symbol,
            members,
            source: Arc::clone(source),
            doc_comment: row.doc_comment.clone(),
        })
    }

    /// The spec §16.2 signature form listing only the members for which
    /// `omitted` is false: the doc comment (if any), then the language's
    /// collapsed summary.
    fn render(&self, omitted: &[bool]) -> Option<String> {
        let members: Vec<ExtractedSymbol> = self
            .members
            .iter()
            .zip(omitted)
            .filter(|(_, omitted)| !**omitted)
            .map(|(member, _)| member.symbol.clone())
            .collect();
        let summary = rivet_languages::signature_summary(
            self.language,
            &self.symbol,
            &members,
            &self.source,
        )?;
        Some(match self.doc_comment.as_deref() {
            Some(doc) => format!("{doc}\n{summary}"),
            None => summary,
        })
    }
}

/// The byte offset where `row`'s declaration header ends in `source`: the
/// end of the stored signature's text, matched from the declaration start.
///
/// A stored signature is the source text from the declaration start to its
/// body (or end), with every whitespace run collapsed to one space, so its
/// space-separated words appear in the source in order, separated by
/// whitespace. `None` when the words do not match contiguously (the members
/// of a multi-name declaration, whose signature is the shared prefix plus
/// one element) or run past the declaration span; callers then treat the
/// whole span as the header, which can only over-report overlap.
fn header_end(row: &SymbolRow, source: &str) -> Option<u32> {
    let signature = row.signature.as_deref()?;
    let end = row.end_byte as usize;
    let text = source.get(row.start_byte as usize..end)?;
    let mut position = 0;
    for (index, word) in signature.split(' ').enumerate() {
        if index > 0 {
            let rest = &text[position..];
            let trimmed = rest.trim_start();
            if trimmed.len() == rest.len() {
                return None;
            }
            position += rest.len() - trimmed.len();
        }
        if !text[position..].starts_with(word) {
            return None;
        }
        position += word.len();
    }
    u32::try_from(row.start_byte as usize + position).ok()
}

/// One ranked candidate with its two representations, as [`fit_context`]
/// consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEntry {
    /// The ranked candidate.
    pub candidate: Candidate,
    /// Its full and (unreduced) signature forms.
    pub forms: Forms,
    /// The end of the candidate's declaration header; `None` when unknown,
    /// in which case its whole span is treated as its header.
    header_end: Option<u32>,
    /// The parts to re-render the signature with members omitted; `None`
    /// when the signature form is unavailable or was supplied as fixed text.
    summary: Option<Summary>,
}

impl ContextEntry {
    /// An entry whose signature form is fixed text. Its signature is never
    /// re-rendered and its header location is unknown, so its signature is
    /// treated as covering its whole span; entries built by
    /// [`context_entries`] know both.
    pub fn new(candidate: Candidate, forms: Forms) -> ContextEntry {
        ContextEntry {
            candidate,
            forms,
            header_end: None,
            summary: None,
        }
    }
}

/// Builds each candidate's [`ContextEntry`]: its [`Forms`] exactly as
/// [`candidate_forms`] describes them, plus what [`fit_context`] needs to
/// locate declaration headers and to re-render a container's summary
/// without separately emitted members.
///
/// The result is parallel to `candidates`.
pub fn context_entries(
    store: &Store,
    candidates: &[Candidate],
) -> Result<Vec<ContextEntry>, CliError> {
    let symbols = store.list_symbols().map_err(crate::index::store_error)?;
    // Members per parent in `list_symbols` order; the language orders them.
    let mut members: HashMap<&str, Vec<&SymbolRow>> = HashMap::new();
    for row in &symbols {
        if let Some(parent) = row.parent_id.as_deref() {
            members.entry(parent).or_default().push(row);
        }
    }

    // Stored bytes per file, and their UTF-8 text when they decode.
    let mut sources: HashMap<String, (Vec<u8>, Option<Arc<str>>)> = HashMap::new();
    let mut result = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let row = &candidate.symbol;
        if !sources.contains_key(&row.file) {
            let bytes = stored_source(store, &row.file)?;
            let text = std::str::from_utf8(&bytes).ok().map(Arc::<str>::from);
            sources.insert(row.file.clone(), (bytes, text));
        }
        let (bytes, text) = &sources[&row.file];
        let full = span_text(bytes, row)?;
        let summary = text.as_ref().and_then(|text| {
            Summary::build(
                row,
                members.get(row.id.as_str()).map_or(&[][..], Vec::as_slice),
                text,
            )
        });
        let signature = summary
            .as_ref()
            .and_then(|summary| summary.render(&vec![false; summary.members.len()]));
        result.push(ContextEntry {
            candidate: candidate.clone(),
            forms: Forms { full, signature },
            header_end: text.as_ref().and_then(|text| header_end(row, text)),
            summary,
        });
    }
    Ok(result)
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

/// Why explored candidates were not emitted (OUTPUT-CONTRACT `omitted`).
///
/// Every explored candidate that is not emitted is counted exactly once, in
/// the order `overlap`, `limit`, `budget`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Omitted {
    /// No allowed form fits the remaining budget, or no allowed form exists.
    pub budget: u64,
    /// Every allowed form would repeat source already emitted.
    pub overlap: u64,
    /// The segment limit was already reached.
    pub limit: u64,
}

/// The fitted context: the intermediate result T30 renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFit {
    /// The requested budget.
    pub budget_tokens: u64,
    /// The requested segment limit, the target included.
    pub limit: usize,
    /// Segments in inclusion order (rank order), the target first. Each
    /// segment's `source` is its final text, after overlap removal.
    pub segments: Vec<Segment>,
    /// The sum of every final segment estimate; never above `budget_tokens`.
    pub estimated_tokens: u64,
    /// The omission counts.
    pub omitted: Omitted,
    /// T27's `candidate_limit_reached`, passed through unchanged.
    pub candidate_limit_reached: bool,
}

/// The source region one emitted piece of text shows.
///
/// A `full` segment is one piece, its whole declaration span. A signature is
/// its declaration header, plus, for a container, one piece per listed
/// member (that member's header). Two pieces overlap when their byte ranges
/// intersect in the same file, with one exception: two *signature* pieces of
/// different declarations that share one span (two names of one multi-name
/// declaration, `public int $a, $b;`) name different elements and do not
/// overlap. A full piece sharing that span does overlap them both, since its
/// text holds every name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece<'a> {
    file: &'a str,
    id: &'a str,
    span: (u32, u32),
    region: (u32, u32),
    signature: bool,
    /// For a listed member of a container summary, its index in the
    /// container's member list.
    member: Option<usize>,
}

impl Piece<'_> {
    fn overlaps(&self, other: &Piece<'_>) -> bool {
        self.file == other.file
            && self.region.0 < other.region.1
            && other.region.0 < self.region.1
            && !(self.signature
                && other.signature
                && self.id != other.id
                && self.span == other.span)
    }
}

/// The header region of a declaration spanning `span`: up to `header_end`,
/// or the whole span when the header end is unknown.
fn header_region(span: (u32, u32), header_end: Option<u32>) -> (u32, u32) {
    match header_end {
        Some(end) if span.0 < end && end <= span.1 => (span.0, end),
        _ => span,
    }
}

/// The pieces `entry` shows in `form`. For a signature, `omitted` names the
/// container members left out of its summary.
fn pieces<'a>(entry: &'a ContextEntry, form: Form, omitted: Option<&[bool]>) -> Vec<Piece<'a>> {
    let row = &entry.candidate.symbol;
    let span = (row.start_byte, row.end_byte);
    let own = |region, signature| Piece {
        file: &row.file,
        id: &row.id,
        span,
        region,
        signature,
        member: None,
    };
    match form {
        Form::Full => vec![own(span, false)],
        Form::Signature => {
            let mut result = vec![own(header_region(span, entry.header_end), true)];
            if let (Some(summary), Some(omitted)) = (entry.summary.as_ref(), omitted) {
                for (index, (member, omitted)) in summary.members.iter().zip(omitted).enumerate() {
                    if *omitted {
                        continue;
                    }
                    let member_span = (
                        member.symbol.span.start_byte(),
                        member.symbol.span.end_byte(),
                    );
                    result.push(Piece {
                        file: &row.file,
                        id: &member.id,
                        span: member_span,
                        region: header_region(member_span, member.header_end),
                        signature: true,
                        member: Some(index),
                    });
                }
            }
            result
        }
    }
}

/// Adds to `omitted` every member strictly inside an omitted member, so a
/// promoted property never resurfaces as a standalone line once its
/// constructor is left out.
fn close_omitted(summary: &Summary, omitted: &mut [bool]) {
    loop {
        let mut changed = false;
        for index in 0..summary.members.len() {
            if omitted[index] {
                continue;
            }
            let inner = summary.members[index].symbol.span;
            let inside = summary
                .members
                .iter()
                .zip(omitted.iter())
                .any(|(outer, &o)| {
                    let outer = outer.symbol.span;
                    o && outer != inner
                        && outer.start_byte() <= inner.start_byte()
                        && inner.end_byte() <= outer.end_byte()
                });
            if inside {
                omitted[index] = true;
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// An emitted segment during fitting.
struct Emitted {
    segment: Segment,
    /// Index into the fitted entries.
    entry: usize,
    /// For a re-renderable signature: per member, whether it is omitted.
    omitted: Option<Vec<bool>>,
}

/// A proposed re-render of an already emitted summary.
struct Rerender {
    position: usize,
    omitted: Vec<bool>,
    source: String,
    estimate: u64,
}

/// A form of a candidate that repeats no emitted source, with its final text
/// and the summaries emitting it would reduce.
struct Proposal {
    form: Form,
    source: String,
    estimate: u64,
    omitted: Option<Vec<bool>>,
    rerenders: Vec<Rerender>,
}

/// Every piece shown by the emitted segments.
fn emitted_pieces<'a>(entries: &'a [ContextEntry], emitted: &[Emitted]) -> Vec<(usize, Piece<'a>)> {
    emitted
        .iter()
        .enumerate()
        .flat_map(|(position, e)| {
            pieces(&entries[e.entry], e.segment.form, e.omitted.as_deref())
                .into_iter()
                .map(move |piece| (position, piece))
        })
        .collect()
}

/// `entry` in `form` against the emitted segments: `None` when the form is
/// unavailable, `Some(Err(()))` when it would repeat emitted source, and
/// otherwise its final text and the re-renders it causes.
///
/// A container's own summary leaves out every member whose header overlaps
/// an emitted piece. Any other overlap with an emitted listed member line
/// re-renders that emitted summary without the member; overlap with any other
/// emitted piece (a full body, a declaration header) rules the form out.
fn propose(
    entries: &[ContextEntry],
    index: usize,
    form: Form,
    emitted: &[Emitted],
    shown: &[(usize, Piece<'_>)],
) -> Option<Result<Proposal, ()>> {
    let entry = &entries[index];
    let (source, omitted) = match (form, entry.summary.as_ref()) {
        (Form::Full, _) => (entry.forms.full.clone(), None),
        (Form::Signature, None) => (entry.forms.signature.clone()?, None),
        (Form::Signature, Some(summary)) => {
            entry.forms.signature.as_ref()?;
            let all = vec![false; summary.members.len()];
            let mut omitted: Vec<bool> = pieces(entry, form, Some(&all))
                .iter()
                .filter(|piece| piece.member.is_some())
                .map(|piece| shown.iter().any(|(_, other)| piece.overlaps(other)))
                .collect();
            close_omitted(summary, &mut omitted);
            let source = if omitted.iter().any(|&o| o) {
                summary.render(&omitted)?
            } else {
                entry.forms.signature.clone()?
            };
            (source, Some(omitted))
        }
    };

    let own = pieces(entry, form, omitted.as_deref());
    let mut reductions: Vec<(usize, usize)> = Vec::new();
    for piece in own.iter().filter(|piece| piece.member.is_none()) {
        for (position, other) in shown {
            if !piece.overlaps(other) {
                continue;
            }
            match other.member {
                Some(member) if emitted[*position].omitted.is_some() => {
                    reductions.push((*position, member));
                }
                _ => return Some(Err(())),
            }
        }
    }
    // A listed member of this summary overlapping an emitted piece was
    // already left out above, so only the candidate's own piece reduces
    // emitted summaries.
    let mut rerenders: Vec<Rerender> = Vec::new();
    reductions.sort_unstable();
    reductions.dedup();
    for (position, member) in reductions {
        let slot = match rerenders.iter().position(|r| r.position == position) {
            Some(slot) => slot,
            None => {
                let current = emitted[position].omitted.clone()?;
                rerenders.push(Rerender {
                    position,
                    omitted: current,
                    source: String::new(),
                    estimate: 0,
                });
                rerenders.len() - 1
            }
        };
        rerenders[slot].omitted[member] = true;
    }
    for rerender in &mut rerenders {
        let summary = entries[emitted[rerender.position].entry].summary.as_ref()?;
        close_omitted(summary, &mut rerender.omitted);
        rerender.source = summary.render(&rerender.omitted)?;
        rerender.estimate = estimate_tokens(&rerender.source);
    }
    let estimate = estimate_tokens(&source);
    Some(Ok(Proposal {
        form,
        source,
        estimate,
        omitted,
        rerenders,
    }))
}

/// Fits ranked `entries` into `budget_tokens` and at most `limit` segments,
/// suppressing overlap and counting omissions (spec §16.4 items 1 to 4; see
/// the module documentation for the exact rules).
///
/// `entries` are in rank order, the target first, as [`context_entries`]
/// builds them from [`crate::context::collect_ranked`]'s candidates.
/// `candidate_limit_reached` is T27's flag and is passed through unchanged.
/// The target is fitted exactly as in [`fit`] and is always the first
/// segment, so a limit of 1 emits only the target.
///
/// Errors: `invalid_arguments` (exit 2) for a limit of 0; otherwise as
/// [`fit`].
pub fn fit_context(
    entries: &[ContextEntry],
    collapse: Collapse,
    budget_tokens: u64,
    limit: usize,
    candidate_limit_reached: bool,
) -> Result<ContextFit, CliError> {
    if limit == 0 {
        return Err(CliError::invalid_arguments(
            "the context segment limit must admit at least the target",
            "Pass `--limit 1` or higher.",
        ));
    }
    let Some(target) = entries.first() else {
        return Err(CliError::general(
            "context fitting received no candidates",
            "Collect candidates before fitting; the target is always first.",
        ));
    };
    if target.candidate.reason != Reason::Target {
        return Err(CliError::general(
            format!(
                "context fitting expected the target first, found reason `{}`",
                target.candidate.reason.as_str()
            ),
            "Pass candidates in rank order, as collection returns them.",
        ));
    }

    let allowed = target_forms(collapse);
    let Some((form, source, estimate)) = first_fitting(&target.forms, allowed, budget_tokens)
    else {
        return Err(target_too_large(
            &target.forms,
            allowed,
            collapse,
            budget_tokens,
        ));
    };
    let mut used = estimate;
    let mut emitted = vec![Emitted {
        segment: Segment {
            candidate: target.candidate.clone(),
            form,
            source,
            estimated_tokens: estimate,
        },
        entry: 0,
        omitted: match (form, target.summary.as_ref()) {
            (Form::Signature, Some(summary)) => Some(vec![false; summary.members.len()]),
            _ => None,
        },
    }];
    let mut omitted = Omitted::default();

    for index in 1..entries.len() {
        // Overlap first: the allowed forms that exist and repeat nothing.
        let shown = emitted_pieces(entries, &emitted);
        let mut available = 0;
        let mut proposals = Vec::new();
        for &form in related_forms(collapse) {
            match propose(entries, index, form, &emitted, &shown) {
                None => {}
                Some(Err(())) => available += 1,
                Some(Ok(proposal)) => {
                    available += 1;
                    proposals.push(proposal);
                }
            }
        }
        if available > 0 && proposals.is_empty() {
            omitted.overlap += 1;
            continue;
        }
        // Then the limit.
        if emitted.len() >= limit {
            omitted.limit += 1;
            continue;
        }
        // Then the budget, on the net total after every re-render.
        let accepted = proposals.into_iter().find_map(|proposal| {
            let released: u64 = proposal
                .rerenders
                .iter()
                .map(|r| emitted[r.position].segment.estimated_tokens)
                .sum();
            let added: u64 = proposal.rerenders.iter().map(|r| r.estimate).sum();
            // `released` is part of `used`, so this never underflows.
            let total = used - released + added + proposal.estimate;
            (total <= budget_tokens).then_some((total, proposal))
        });
        let Some((total, proposal)) = accepted else {
            omitted.budget += 1;
            continue;
        };
        for rerender in proposal.rerenders {
            let current = &mut emitted[rerender.position];
            current.segment.source = rerender.source;
            current.segment.estimated_tokens = rerender.estimate;
            current.omitted = Some(rerender.omitted);
        }
        used = total;
        emitted.push(Emitted {
            segment: Segment {
                candidate: entries[index].candidate.clone(),
                form: proposal.form,
                source: proposal.source,
                estimated_tokens: proposal.estimate,
            },
            entry: index,
            omitted: proposal.omitted,
        });
    }

    let segments: Vec<Segment> = emitted.into_iter().map(|e| e.segment).collect();
    debug_assert!(used <= budget_tokens);
    debug_assert_eq!(
        used,
        segments.iter().map(|s| s.estimated_tokens).sum::<u64>()
    );
    Ok(ContextFit {
        budget_tokens,
        limit,
        segments,
        estimated_tokens: used,
        omitted,
        candidate_limit_reached,
    })
}

/// Builds entries from the snapshot and fits a T27 collection in one step
/// (see [`context_entries`] and [`fit_context`]).
pub fn fit_collection(
    store: &Store,
    collection: &Collection,
    collapse: Collapse,
    budget_tokens: u64,
    limit: usize,
) -> Result<ContextFit, CliError> {
    let entries = context_entries(store, &collection.candidates)?;
    fit_context(
        &entries,
        collapse,
        budget_tokens,
        limit,
        collection.candidate_limit_reached,
    )
}

#[cfg(test)]
mod tests {
    use rivet_core::{Collapse, Resolution, SymbolKind};
    use rivet_store::SymbolRow;

    use super::{ContextEntry, ContextFit, Fitted, Form, Forms, Omitted, estimate_tokens, fit};
    use super::{fit_context, header_end};
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

    /// A candidate in `file` spanning `[start, end)`.
    fn spanned(name: &str, reason: Reason, file: &str, start: u32, end: u32) -> Candidate {
        let mut candidate = candidate(name, reason);
        candidate.symbol.id = format!("{file}#{name}");
        candidate.symbol.file = file.to_string();
        candidate.symbol.start_byte = start;
        candidate.symbol.end_byte = end;
        candidate
    }

    fn entry(candidate: Candidate, full: usize, signature: Option<usize>) -> ContextEntry {
        ContextEntry::new(candidate, forms(full, signature))
    }

    fn context_shape(fitted: &ContextFit) -> Vec<(String, Form)> {
        fitted
            .segments
            .iter()
            .map(|segment| (segment.candidate.symbol.name.clone(), segment.form))
            .collect()
    }

    fn counts(budget: u64, overlap: u64, limit: u64) -> Omitted {
        Omitted {
            budget,
            overlap,
            limit,
        }
    }

    #[test]
    fn a_full_target_suppresses_contained_candidates_but_not_other_files() {
        let entries = vec![
            entry(spanned("k", Reason::Target, "F.php", 0, 100), 30, Some(9)),
            entry(spanned("m1", Reason::Callee, "F.php", 10, 20), 6, Some(3)),
            // Same span as the target: contained, bounds included.
            entry(spanned("same", Reason::Caller, "F.php", 0, 100), 6, Some(3)),
            // Same span in another file: never overlaps.
            entry(
                spanned("other", Reason::Caller, "G.php", 10, 20),
                6,
                Some(3),
            ),
            entry(spanned("m2", Reason::Caller, "F.php", 99, 100), 6, Some(3)),
            // Touching the end but outside.
            entry(
                spanned("after", Reason::Caller, "F.php", 100, 110),
                6,
                Some(3),
            ),
        ];
        for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
            let fitted = fit_context(&entries, collapse, 1_000, 50, true).expect("fits");
            let related = if collapse == Collapse::Always {
                Form::Signature
            } else {
                Form::Full
            };
            assert_eq!(
                context_shape(&fitted),
                vec![
                    ("k".to_string(), Form::Full),
                    ("other".to_string(), related),
                    ("after".to_string(), related),
                ]
            );
            assert_eq!(fitted.omitted, counts(0, 3, 0));
            assert!(fitted.candidate_limit_reached);
        }
    }

    /// An ancestor of an emitted segment may not be full. A fixed-text
    /// signature has no known header, so it is treated as covering its whole
    /// span: the ancestor's signature is allowed only when it does not
    /// contain the emitted segment's region, which here it does.
    #[test]
    fn an_ancestor_without_a_nonoverlapping_form_is_overlap() {
        let entries = vec![
            entry(spanned("m", Reason::Target, "F.php", 10, 20), 6, Some(3)),
            entry(spanned("k", Reason::Parent, "F.php", 0, 100), 30, Some(9)),
            entry(spanned("bare", Reason::Parent, "F.php", 5, 50), 12, None),
        ];
        for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
            let fitted = fit_context(&entries, collapse, 1_000, 50, false).expect("fits");
            assert_eq!(context_shape(&fitted), vec![("m".to_string(), Form::Full)]);
            // Under `always`, `bare` has no allowed form at all: budget.
            let expected = if collapse == Collapse::Always {
                counts(1, 1, 0)
            } else {
                counts(0, 2, 0)
            };
            assert_eq!(fitted.omitted, expected, "{collapse:?}");
        }
    }

    #[test]
    fn the_limit_counts_after_overlap_and_before_budget() {
        let entries = vec![
            entry(spanned("t", Reason::Target, "F.php", 0, 50), 30, Some(9)),
            entry(spanned("a", Reason::Callee, "G.php", 0, 10), 3, None),
            entry(spanned("inside", Reason::Callee, "F.php", 10, 20), 3, None),
            entry(spanned("b", Reason::Callee, "G.php", 20, 30), 3, None),
            entry(
                spanned("huge", Reason::Caller, "G.php", 40, 50),
                3_000,
                None,
            ),
            entry(spanned("c", Reason::Caller, "G.php", 60, 70), 3, None),
        ];
        let fitted = fit_context(&entries, Collapse::Auto, 100, 50, false).expect("fits");
        assert_eq!(
            context_shape(&fitted),
            vec![
                ("t".to_string(), Form::Full),
                ("a".to_string(), Form::Full),
                ("b".to_string(), Form::Full),
                ("c".to_string(), Form::Full),
            ]
        );
        assert_eq!(fitted.omitted, counts(1, 1, 0));

        // Limit 2: `inside` is still overlap; `b`, `huge`, and `c` are limit,
        // even `huge`, which would not fit either.
        let fitted = fit_context(&entries, Collapse::Auto, 100, 2, false).expect("fits");
        assert_eq!(
            context_shape(&fitted),
            vec![("t".to_string(), Form::Full), ("a".to_string(), Form::Full)]
        );
        assert_eq!(fitted.omitted, counts(0, 1, 3));

        // Limit 1: only the target, even when it is collapsed.
        let fitted = fit_context(&entries, Collapse::Auto, 9, 1, false).expect("fits");
        assert_eq!(
            context_shape(&fitted),
            vec![("t".to_string(), Form::Signature)]
        );
        // A signature target (fixed text) covers its whole span: `inside`
        // is overlap even here.
        assert_eq!(fitted.omitted, counts(0, 1, 4));

        let error = fit_context(&entries, Collapse::Auto, 100, 0, false).unwrap_err();
        assert_eq!((error.code, error.exit), ("invalid_arguments", 2));
        let error = fit_context(&entries, Collapse::Never, 9, 50, false).unwrap_err();
        assert_eq!((error.code, error.exit), ("budget_too_small", 8));
    }

    /// With no overlapping spans and a limit of at least the candidate
    /// count, `fit_context` emits exactly `fit`'s segments at every budget
    /// and mode, and every omission is `budget`.
    #[test]
    fn without_overlap_or_limit_the_result_is_t28s() {
        let sizes: [(usize, Option<usize>); 8] = [
            (30, Some(9)),
            (100, Some(20)),
            (7, Some(4)),
            (1, None),
            (47, Some(47)),
            (250, None),
            (2, Some(1)),
            (64, Some(9)),
        ];
        let entries: Vec<ContextEntry> = sizes
            .iter()
            .enumerate()
            .map(|(index, &(full, signature))| {
                let reason = if index == 0 {
                    Reason::Target
                } else {
                    Reason::Callee
                };
                let start = index as u32 * 10;
                entry(
                    spanned(&format!("c{index}"), reason, "F.php", start, start + 5),
                    full,
                    signature,
                )
            })
            .collect();
        let pairs: Vec<(Candidate, Forms)> = entries
            .iter()
            .map(|e| (e.candidate.clone(), e.forms.clone()))
            .collect();
        for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
            for budget in 0..=200 {
                for limit in [sizes.len(), 50] {
                    let t28 = fit(&pairs, collapse, budget);
                    let t29 = fit_context(&entries, collapse, budget, limit, false);
                    match (t28, t29) {
                        (Ok(t28), Ok(t29)) => {
                            assert_eq!(t28.segments, t29.segments, "{collapse:?} {budget}");
                            assert_eq!(t28.estimated_tokens, t29.estimated_tokens);
                            assert_eq!(
                                t29.omitted,
                                counts((sizes.len() - t29.segments.len()) as u64, 0, 0)
                            );
                        }
                        (Err(a), Err(b)) => {
                            assert_eq!(a.code, b.code);
                            assert_eq!(a.extra, b.extra);
                        }
                        _ => panic!("T28 and T29 disagree on success at {collapse:?} {budget}"),
                    }
                }
            }
        }
    }

    #[test]
    fn context_fitting_is_deterministic() {
        let entries = vec![
            entry(spanned("t", Reason::Target, "F.php", 0, 50), 30, Some(9)),
            entry(spanned("a", Reason::Type, "F.php", 10, 20), 60, Some(12)),
            entry(spanned("b", Reason::Callee, "G.php", 0, 10), 15, Some(6)),
        ];
        for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
            for budget in [10, 15, 30, 100] {
                for limit in [1, 2, 50] {
                    let first =
                        fit_context(&entries, collapse, budget, limit, false).expect("fits");
                    let second =
                        fit_context(&entries, collapse, budget, limit, false).expect("fits");
                    assert_eq!(first, second);
                }
            }
        }
    }

    /// The header ends where the stored signature's words end in the source,
    /// across any whitespace runs; a signature that is not a contiguous
    /// prefix of the span (a multi-name member) has no known header end.
    #[test]
    fn header_end_matches_the_collapsed_signature_across_whitespace() {
        let source = "<?php\n  public function f(\n    int $a,\n    int $b\n  ): void\n  {\n  }\n";
        let start = source.find("public").unwrap() as u32;
        let mut row = candidate("f", Reason::Target).symbol;
        row.start_byte = start;
        row.end_byte = source.rfind('}').unwrap() as u32 + 1;
        row.signature = Some("public function f( int $a, int $b ): void".to_string());
        let end = header_end(&row, source).expect("located");
        assert_eq!(
            &source[start as usize..end as usize],
            "public function f(\n    int $a,\n    int $b\n  ): void"
        );

        row.signature = Some("public function f( int $b".to_string());
        assert_eq!(header_end(&row, source), None);
        // Words must be separated by whitespace in the source too.
        row.signature = Some("public function f(int $a".to_string());
        assert_eq!(header_end(&row, source), None);
        row.signature = None;
        assert_eq!(header_end(&row, source), None);
    }
}
