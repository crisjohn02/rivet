//! Shared reference pipeline for `rivet refs` (T23) and the `rivet symbol`
//! `calls`/`called_by` lists (T24).
//!
//! Both commands select extracted uses, derive each use's `resolution` from its
//! stored binding (`name_match` is query-relative and never persisted), dedupe
//! by `(file, start_byte, end_byte, ref_kind)`, sort by the contract key
//! `(file bytes, start_byte, end_byte, ref_kind, resolved_target or empty)`,
//! count, and slice. Keeping one copy here stops the two commands from
//! drifting.
//!
//! LR2: in reference mode an unresolved same-name use is excluded when
//! [`Evidence`] shows it cannot refer to the target (spec §11.5); the counts
//! of excluded uses are returned with the matches.
//!
//! SY1: a `symbol` call list is collected at `name_match` and then filtered to
//! its minimum tier by [`apply_call_list_minimum`], which counts the
//! `name_match` rows the filter hid.

use std::collections::{HashMap, HashSet};

use rivet_core::{RefKind, Resolution};
use rivet_index::{ClassRelation, Hierarchy, SubtypeIndex, form_compatible, lookup_name_matches};
use rivet_store::{BindingRow, ReceiverClassRow, Store, SymbolRow, UseRow};
use serde_json::{Map, Value, json};

use crate::index;
use crate::symbol::symbol_object;
use crate::transport::CliError;

/// The two reference modes (spec §11.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Uses bound to the target plus same-name unresolved uses.
    References,
    /// `references` plus same-name uses bound elsewhere.
    Candidates,
}

impl Mode {
    /// The contract spelling emitted as `mode`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Mode::References => "references",
            Mode::Candidates => "candidates",
        }
    }
}

/// How [`collect_matches`] chooses which uses to keep.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Selection {
    /// `refs`: reference/candidate matching relative to the query.
    Query(Mode),
    /// `symbol.calls`: uses whose `containing_symbol` is the target.
    Contained,
}

/// One reference before output shaping. Carries the parts the ordering,
/// counting, and filtering steps need without touching the store again.
#[derive(Debug, Clone)]
pub(crate) struct ReferenceMatch {
    pub(crate) file: String,
    pub(crate) start_byte: u32,
    pub(crate) end_byte: u32,
    pub(crate) line: u32,
    pub(crate) col: u32,
    pub(crate) containing_symbol: Option<String>,
    pub(crate) ref_kind: RefKind,
    pub(crate) resolution: Resolution,
    pub(crate) resolved_target: Option<String>,
    pub(crate) receiver: Option<String>,
}

/// A counted and sliced reference page.
pub(crate) struct Page<'a> {
    pub(crate) total: u64,
    pub(crate) truncated: bool,
    pub(crate) next_offset: Option<u64>,
    pub(crate) items: Vec<&'a ReferenceMatch>,
}

/// Loads every persisted use in deterministic `(file bytes, start_byte,
/// end_byte, ref_kind, use_id)` order.
///
/// Uses must be loaded in full because an alias use reaches the target through
/// its binding even when its spelling differs from the target's name, so a
/// name-only lookup would miss it.
pub(crate) fn all_uses(store: &Store) -> Result<Vec<UseRow>, CliError> {
    let mut rows = Vec::new();
    for file in store.list_files().map_err(index::store_error)? {
        rows.extend(
            store
                .list_uses_for_file(&file.path)
                .map_err(index::store_error)?,
        );
    }
    Ok(rows)
}

/// Loads the whole `bindings` table keyed by `use_id`.
///
/// A map is chosen over a new store lookup so the target's aliased uses can be
/// found without a second query per use. Binding rows have a unique `use_id`,
/// and the output order is imposed by sorting the uses, so map iteration order
/// never reaches output.
pub(crate) fn bindings_by_use_id(store: &Store) -> Result<HashMap<i64, BindingRow>, CliError> {
    let mut by_use = HashMap::new();
    for binding in store.list_bindings().map_err(index::store_error)? {
        by_use.insert(binding.use_id, binding);
    }
    Ok(by_use)
}

/// Snapshot facts reference mode reads to exclude uses by evidence (LR2).
pub(crate) struct Evidence {
    /// Determined receiver classes of unbound uses, keyed by `use_id`.
    receivers: HashMap<i64, ReceiverClassRow>,
    /// Every class-like declaration (traits included), keyed by canonical ID.
    class_like: HashMap<String, SymbolRow>,
    /// Every class-like's up-set, built once per query from the declared
    /// hierarchy. Built from the real hierarchy only when some receiver class
    /// was recorded, since only rule 2 reads it.
    subtypes: SubtypeIndex,
}

impl Evidence {
    /// Loads the evidence of the acquired snapshot. `uses` and `bindings`
    /// must be the snapshot's complete use and binding rows.
    pub(crate) fn load(
        store: &Store,
        uses: &[UseRow],
        bindings: &HashMap<i64, BindingRow>,
    ) -> Result<Evidence, CliError> {
        let symbols = store.list_symbols().map_err(index::store_error)?;
        let receivers: HashMap<i64, ReceiverClassRow> = store
            .list_receiver_classes()
            .map_err(index::store_error)?
            .into_iter()
            .map(|row| (row.use_id, row))
            .collect();
        let hierarchy = if receivers.is_empty() {
            Hierarchy::default()
        } else {
            let scopes = store.list_scopes().map_err(index::store_error)?;
            // `Hierarchy::from_rows` does not depend on input order.
            let binding_rows: Vec<BindingRow> = bindings.values().cloned().collect();
            Hierarchy::from_rows(&symbols, uses, &scopes, &binding_rows)
        };
        let class_like: HashMap<String, SymbolRow> = symbols
            .into_iter()
            .filter(|row| {
                matches!(
                    row.kind,
                    rivet_core::SymbolKind::Class
                        | rivet_core::SymbolKind::Interface
                        | rivet_core::SymbolKind::Enum
                )
            })
            .map(|row| (row.id.clone(), row))
            .collect();
        let subtypes = SubtypeIndex::new(&hierarchy, class_like.values());
        Ok(Evidence {
            receivers,
            class_like,
            subtypes,
        })
    }
}

/// How many same-name unresolved uses reference mode excluded, by the
/// evidence that excluded them (OUTPUT-CONTRACT `by_exclusion`). A use both
/// kinds of evidence exclude counts once, as `incompatible_form`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ExclusionCounts {
    /// Excluded by rule 1: the use form cannot name the target's kind.
    pub(crate) incompatible_form: u64,
    /// Excluded by rule 2: the receiver class is unrelated to the target's.
    pub(crate) unrelated_receiver: u64,
}

/// The sorted matches of one query plus the uses excluded by evidence.
pub(crate) struct Matches {
    pub(crate) matches: Vec<ReferenceMatch>,
    pub(crate) excluded: ExclusionCounts,
}

/// Selects, filters, and sorts the reference matches for one query.
///
/// `Selection::Query` is the T23 `refs` rule: a use bound to the target keeps
/// its stored resolution, a same-name use bound elsewhere is query-relative
/// `name_match` and only appears in candidate mode, and a same-name unresolved
/// use is `name_match`. `Selection::Contained` is the `symbol.calls` rule: a
/// use contained by the target is kept, with the resolution of its own store
/// binding (the call-to-callee evidence), or `name_match` when unresolved.
///
/// LR2: under `Selection::Query(Mode::References)` only, a same-name
/// unresolved use that survives the kind and resolution filters is excluded
/// when its form cannot name the target's kind
/// ([`rivet_index::form_compatible`]) or its determined receiver class is
/// unrelated to the target's class ([`ClassRelation::unrelated`]), and counted
/// in [`Matches::excluded`]. A bound use is never excluded, and exclusion
/// changes no tier. Candidate mode and `Selection::Contained` are unchanged.
pub(crate) fn collect_matches(
    uses: &[UseRow],
    bindings: &HashMap<i64, BindingRow>,
    evidence: &Evidence,
    target: &SymbolRow,
    selection: Selection,
    kinds: Option<&HashSet<RefKind>>,
    minimum: Resolution,
) -> Matches {
    // Dedupe by the contract key. The `uses` table already enforces the same
    // uniqueness, so this is a safety net; the first row for a key wins, and
    // `uses` is iterated in a deterministic order.
    let mut seen: HashSet<(String, u32, u32, RefKind)> = HashSet::new();
    let mut matches = Vec::new();
    let mut excluded = ExclusionCounts::default();

    let excluding = matches!(selection, Selection::Query(Mode::References));
    let target_parent = target
        .parent_id
        .as_deref()
        .and_then(|id| evidence.class_like.get(id));
    let relation = if excluding {
        ClassRelation::for_target(&evidence.subtypes, target, target_parent)
    } else {
        None
    };

    for row in uses {
        let Some(use_id) = row.use_id else {
            continue;
        };
        let binding = bindings.get(&use_id);

        let (resolution, resolved_target, include, unbound) = match selection {
            Selection::Query(mode) => {
                // Folded by the target declaration's kind, exactly as the
                // `rivet symbol` short-name lookup folds (AF4).
                let name_matches =
                    lookup_name_matches(&row.lookup_name, target.kind, &target.lookup_name);
                let bound_to_target = binding.is_some_and(|binding| binding.target_id == target.id);

                // A use bound to the target is always kept, even when its
                // spelling is an alias. A use bound elsewhere is query-relative
                // `name_match` and only appears in candidate mode. An unresolved
                // use follows its normalized unqualified name in both modes.
                if bound_to_target {
                    let binding = binding.expect("bound_to_target requires a binding");
                    (
                        binding.resolution,
                        Some(binding.target_id.clone()),
                        true,
                        false,
                    )
                } else if let Some(binding) = binding {
                    (
                        Resolution::NameMatch,
                        Some(binding.target_id.clone()),
                        mode == Mode::Candidates && name_matches,
                        false,
                    )
                } else {
                    (Resolution::NameMatch, None, name_matches, true)
                }
            }
            Selection::Contained => {
                if row.containing_symbol.as_deref() != Some(target.id.as_str()) {
                    (Resolution::NameMatch, None, false, false)
                } else if let Some(binding) = binding {
                    // The call is contained by the target; its resolution
                    // describes the link to the callee, exactly the stored
                    // binding `refs` reports for a use bound to that callee.
                    (
                        binding.resolution,
                        Some(binding.target_id.clone()),
                        true,
                        false,
                    )
                } else {
                    (Resolution::NameMatch, None, true, false)
                }
            }
        };
        if !include {
            continue;
        }

        if let Some(kinds) = kinds
            && !kinds.contains(&row.ref_kind)
        {
            continue;
        }
        if !resolution.min_resolution(minimum) {
            continue;
        }

        let key = (row.file.clone(), row.start_byte, row.end_byte, row.ref_kind);
        if !seen.insert(key) {
            continue;
        }

        // Evidence-based exclusion (LR2): only an unresolved same-name use in
        // reference mode, after every filter, so each counted use is one the
        // page would otherwise have listed under the same filters.
        if excluding && unbound {
            if !form_compatible(target, target_parent, row) {
                excluded.incompatible_form += 1;
                continue;
            }
            if let Some(relation) = relation.as_ref()
                && let Some(receiver) = evidence.receivers.get(&use_id)
            {
                let receiver_row = receiver
                    .class_id
                    .as_deref()
                    .and_then(|id| evidence.class_like.get(id));
                if relation.unrelated(receiver, receiver_row) {
                    excluded.unrelated_receiver += 1;
                    continue;
                }
            }
        }

        matches.push(ReferenceMatch {
            file: row.file.clone(),
            start_byte: row.start_byte,
            end_byte: row.end_byte,
            line: row.line,
            col: row.col,
            containing_symbol: row.containing_symbol.clone(),
            ref_kind: row.ref_kind,
            resolution,
            resolved_target,
            receiver: row.receiver.clone(),
        });
    }

    // Contract ordering: `(file bytes, start_byte, end_byte, ref_kind,
    // resolved_target or empty)`.
    matches.sort_by(|a, b| {
        a.file
            .as_bytes()
            .cmp(b.file.as_bytes())
            .then(a.start_byte.cmp(&b.start_byte))
            .then(a.end_byte.cmp(&b.end_byte))
            .then_with(|| {
                a.ref_kind
                    .as_str()
                    .as_bytes()
                    .cmp(b.ref_kind.as_str().as_bytes())
            })
            .then_with(|| {
                a.resolved_target
                    .as_deref()
                    .unwrap_or("")
                    .as_bytes()
                    .cmp(b.resolved_target.as_deref().unwrap_or("").as_bytes())
            })
    });
    Matches { matches, excluded }
}

/// Counts and slices a sorted match list (OUTPUT-CONTRACT "Pagination and
/// resolution"). `total` counts every match before slicing; `truncated` covers
/// earlier pages too, so an offset beyond the end is an empty truncated page.
pub(crate) fn paginate(matches: &[ReferenceMatch], limit: u64, offset: u64) -> Page<'_> {
    let total = matches.len() as u64;
    let offset_index = usize::try_from(offset).unwrap_or(usize::MAX);
    let items: Vec<&ReferenceMatch> = matches
        .iter()
        .skip(offset_index)
        .take(limit as usize)
        .collect();
    let page_len = items.len() as u64;
    let truncated = total > page_len;
    let next_offset = if offset + page_len < total {
        Some(offset + page_len)
    } else {
        None
    };
    Page {
        total,
        truncated,
        next_offset,
        items,
    }
}

/// Builds one reference object (OUTPUT-CONTRACT "`rivet refs`") with keys in
/// contract order.
pub(crate) fn reference_object(
    store: &Store,
    reference: &ReferenceMatch,
) -> Result<Value, CliError> {
    let content_hash = store
        .get_file(&reference.file)
        .map_err(index::store_error)?
        .and_then(|file| file.content_hash);
    let containing_symbol = match &reference.containing_symbol {
        Some(id) => match store.get_symbol(id).map_err(index::store_error)? {
            Some(row) => symbol_object(store, &row).map_err(index::store_error)?,
            None => Value::Null,
        },
        None => Value::Null,
    };

    let mut object = Map::new();
    object.insert("file".to_string(), json!(reference.file));
    object.insert("content_hash".to_string(), json!(content_hash));
    object.insert("start_byte".to_string(), json!(reference.start_byte));
    object.insert("end_byte".to_string(), json!(reference.end_byte));
    object.insert("line".to_string(), json!(reference.line));
    object.insert("column".to_string(), json!(reference.col));
    object.insert("containing_symbol".to_string(), containing_symbol);
    object.insert("ref_kind".to_string(), json!(reference.ref_kind.as_str()));
    object.insert(
        "resolution".to_string(),
        json!(reference.resolution.as_str()),
    );
    object.insert(
        "resolved_target".to_string(),
        json!(reference.resolved_target),
    );
    object.insert("receiver".to_string(), json!(reference.receiver));
    Ok(Value::Object(object))
}

/// A `symbol` call list after its tier filter (SY1).
pub(crate) struct CallList {
    /// The rows at or above the minimum tier, still in contract order.
    pub(crate) matches: Vec<ReferenceMatch>,
    /// How many `name_match` rows the tier filter removed (OUTPUT-CONTRACT
    /// `hidden_name_match`).
    pub(crate) hidden_name_match: u64,
}

/// Applies `minimum` to a call list collected at `name_match`, so every row
/// the filter removes is one `--min-resolution name_match` lists (SY1).
///
/// Only removed `name_match` rows are counted: a `scoped` row that an explicit
/// `--min-resolution exact` removes is in no count. Filtering after collection
/// keeps the listed rows identical to collecting at `minimum`, because the
/// tier test is per row and the `uses` table is unique on the dedupe key; it
/// also keeps uses excluded by evidence (LR2), which no tier lists, out of the
/// count. The count is taken before pagination, so it does not depend on
/// `--limit`/`--offset`.
pub(crate) fn apply_call_list_minimum(
    matches: Vec<ReferenceMatch>,
    minimum: Resolution,
) -> CallList {
    let mut hidden_name_match = 0;
    let matches = matches
        .into_iter()
        .filter(|reference| {
            let kept = reference.resolution.min_resolution(minimum);
            if !kept && reference.resolution == Resolution::NameMatch {
                hidden_name_match += 1;
            }
            kept
        })
        .collect();
    CallList {
        matches,
        hidden_name_match,
    }
}

/// Builds a `{total, truncated, next_offset, hidden_name_match, items}` call
/// list, paginating its matches independently of any other list.
pub(crate) fn call_list_object(
    store: &Store,
    list: &CallList,
    limit: u64,
    offset: u64,
) -> Result<Value, CliError> {
    let page = paginate(&list.matches, limit, offset);
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|reference| reference_object(store, reference))
        .collect::<Result<_, _>>()?;
    let mut object = Map::new();
    object.insert("total".to_string(), json!(page.total));
    object.insert("truncated".to_string(), json!(page.truncated));
    object.insert("next_offset".to_string(), json!(page.next_offset));
    object.insert(
        "hidden_name_match".to_string(),
        json!(list.hidden_name_match),
    );
    object.insert("items".to_string(), Value::Array(items));
    Ok(Value::Object(object))
}

/// Parses `--min-resolution`, rejecting unknown values before any filesystem
/// work. Shared by `refs` and `symbol` (both support the flag). An absent flag
/// is `name_match`, the `refs` default; `symbol` substitutes its own call-list
/// default before calling this (SY1).
pub(crate) fn parse_min_resolution(value: Option<&str>) -> Result<Resolution, CliError> {
    match value {
        None => Ok(Resolution::NameMatch),
        Some(value) => value.parse::<Resolution>().map_err(|_| {
            CliError::invalid_arguments(
                format!(
                    "invalid value for `--min-resolution`: {value:?} (expected \"exact\", \"scoped\", or \"name_match\")"
                ),
                "Pass `--min-resolution exact`, `scoped`, or `name_match`.",
            )
        }),
    }
}
