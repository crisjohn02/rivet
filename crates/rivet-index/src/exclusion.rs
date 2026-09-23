//! Evidence-based exclusion in `refs --mode references` (LR2; spec §11.5).
//!
//! Reference mode keeps every unresolved use whose name matches the target.
//! This module decides when evidence shows such a use cannot refer to the
//! target, so reference mode may leave it out. There are exactly two kinds of
//! evidence:
//!
//! 1. [`form_compatible`]: the use's form (its `ref_kind` and whether it has a
//!    receiver) cannot name a declaration of the target's kind.
//! 2. [`ClassRelation::unrelated`]: the use's receiver class, as a receiver
//!    rule determined it ([`rivet_store::ReceiverClassRow`]), shares no
//!    possible instance with the class that declares the target member.
//!
//! Exclusion is not resolution. It never binds a use, never changes a tier,
//! and never applies to a use bound to the target; `--mode candidates` lists
//! every excluded use exactly as before.

use std::collections::BTreeSet;

use rivet_core::{RefKind, SymbolKind};
use rivet_store::{ReceiverClassRow, SymbolRow, UseRow};

use crate::hierarchy::{Hierarchy, Supertype};

/// Whether a declaration kind is class-like (a trait is class-kind).
fn is_class_like(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
    )
}

/// Whether an unresolved use of this form can name `target` (rule 1).
///
/// `target_parent` is the declaration that directly contains `target`, if
/// any; it tells a class constant from a global one. The table, derived from
/// the forms the PHP extractor records (`crates/rivet-languages/src/php/uses.rs`):
///
/// | Target | Compatible use forms |
/// |---|---|
/// | method | `call` or `read` with a receiver (`->`, `?->`, `::`); `__get` can dispatch a property read to a method |
/// | function | `call` without a receiver; `import` |
/// | property | `read` or `write` with a receiver |
/// | class constant or enum case | `read` with a receiver |
/// | global constant | `import` |
/// | class, interface, enum, trait | `type`; `import` |
/// | namespace (`module`), `struct` | every form |
///
/// A `unknown` use is compatible with every target: the extractor records a
/// bare constant name, a trait `use` inside a class body, and any other
/// unclassified identifier that way. `assignment` is never recorded by the
/// PHP extractor and is likewise never excluded.
pub fn form_compatible(
    target: &SymbolRow,
    target_parent: Option<&SymbolRow>,
    row: &UseRow,
) -> bool {
    let receiver = row.receiver.is_some();
    match row.ref_kind {
        RefKind::Unknown | RefKind::Assignment => return true,
        RefKind::Call | RefKind::Type | RefKind::Import | RefKind::Read | RefKind::Write => {}
    }
    match target.kind {
        // `__get` can dispatch a property read to a method, so a read with a
        // receiver is kept; `__set` never invokes a same-named method.
        SymbolKind::Method => matches!(row.ref_kind, RefKind::Call | RefKind::Read) && receiver,
        SymbolKind::Function => {
            (row.ref_kind == RefKind::Call && !receiver) || row.ref_kind == RefKind::Import
        }
        SymbolKind::Property => matches!(row.ref_kind, RefKind::Read | RefKind::Write) && receiver,
        SymbolKind::Const => {
            if target_parent.is_some_and(|parent| is_class_like(parent.kind)) {
                row.ref_kind == RefKind::Read && receiver
            } else {
                row.ref_kind == RefKind::Import
            }
        }
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum => {
            matches!(row.ref_kind, RefKind::Type | RefKind::Import)
        }
        SymbolKind::Module | SymbolKind::Struct => true,
    }
}

/// Whether `row` may be a PHP trait.
///
/// The PHP adapter records a trait as class-kind; only its stored signature,
/// the declaration header as written, tells the two apart. A class-kind
/// declaration is known not to be a trait only when its header, after any
/// leading attribute groups, starts with `class`, `final`, `abstract`, or
/// `readonly`. Anything else, including a missing signature or an attribute
/// group this scanner cannot delimit, may be a trait.
pub fn possibly_trait(row: &SymbolRow) -> bool {
    if row.kind != SymbolKind::Class {
        return false;
    }
    let Some(signature) = row.signature.as_deref() else {
        return true;
    };
    let Some(rest) = strip_attribute_groups(signature) else {
        return true;
    };
    let first = rest
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()
        .unwrap_or("");
    !matches!(
        first.to_ascii_lowercase().as_str(),
        "class" | "final" | "abstract" | "readonly"
    )
}

/// `text` without its leading `#[...]` attribute groups and whitespace, or
/// `None` when a group cannot be delimited: nested brackets and quoted
/// strings are tracked, and a comment inside a group gives up.
fn strip_attribute_groups(text: &str) -> Option<&str> {
    let mut rest = text.trim_start();
    while let Some(body) = rest.strip_prefix("#[") {
        let bytes = body.as_bytes();
        let mut depth = 1_u32;
        let mut index = 0;
        let mut quote: Option<u8> = None;
        while depth > 0 {
            let byte = *bytes.get(index)?;
            match quote {
                Some(open) => {
                    if byte == b'\\' {
                        index += 1;
                    } else if byte == open {
                        quote = None;
                    }
                }
                None => match byte {
                    b'\'' | b'"' => quote = Some(byte),
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    b'/' | b'#' => return None,
                    _ => {}
                },
            }
            index += 1;
        }
        rest = body.get(index..)?.trim_start();
    }
    Some(rest)
}

/// The up-set of one class-like X: X itself (unless anonymous) plus
/// [`Hierarchy::ancestors`] of X.
struct UpSet {
    /// Indexed IDs in the set.
    ids: BTreeSet<String>,
    /// ASCII-lowercased qualified names in the set.
    qnames: BTreeSet<String>,
    /// Whether some link in X's ancestor closure is not indexed, so the rest
    /// of the chain above it is unknown.
    unindexed_link: bool,
}

impl UpSet {
    fn new(own: Option<&SymbolRow>, ancestors: &[Supertype]) -> UpSet {
        let mut ids = BTreeSet::new();
        let mut qnames = BTreeSet::new();
        if let Some(row) = own {
            ids.insert(row.id.clone());
            qnames.insert(row.qualified_name.to_ascii_lowercase());
        }
        let mut unindexed_link = false;
        for entry in ancestors {
            match &entry.resolved {
                Some(id) => {
                    ids.insert(id.clone());
                }
                None => unindexed_link = true,
            }
            if let Some(qname) = &entry.qualified_name {
                qnames.insert(qname.to_ascii_lowercase());
            }
        }
        UpSet {
            ids,
            qnames,
            unindexed_link,
        }
    }

    /// Whether the class with `id` / `qname` is in the set.
    fn contains(&self, id: Option<&str>, qname: &str) -> bool {
        id.is_some_and(|id| self.ids.contains(id))
            || self.qnames.contains(&qname.to_ascii_lowercase())
    }
}

/// The up-set of every class-like of one snapshot, indexed and anonymous,
/// computed once and shared by every target (LR2, rule 2).
pub struct SubtypeIndex {
    sets: Vec<UpSet>,
    /// Whether some declared supertype anywhere has no qualified name.
    unknown_link: bool,
}

impl SubtypeIndex {
    /// Builds the index from the snapshot's hierarchy and its indexed
    /// class-like declarations.
    pub fn new<'r>(
        hierarchy: &Hierarchy,
        class_like: impl IntoIterator<Item = &'r SymbolRow>,
    ) -> SubtypeIndex {
        let mut sets: Vec<UpSet> = class_like
            .into_iter()
            .filter(|row| is_class_like(row.kind))
            .map(|row| UpSet::new(Some(row), &hierarchy.ancestors(&row.id)))
            .collect();
        sets.extend(
            hierarchy
                .anonymous_ancestors()
                .iter()
                .map(|(_, ancestors)| UpSet::new(None, ancestors)),
        );
        SubtypeIndex {
            sets,
            unknown_link: hierarchy.has_unknown_link(),
        }
    }
}

/// Rule 2 for one target member: whether a receiver class R is unrelated to
/// the class-like T that declares the target.
///
/// Exclusion must be sound: an excluded use cannot dispatch to the target
/// under the stated assumption (an unindexed class never extends or
/// implements an indexed one). So R and T are unrelated only when no object
/// could be an instance of both. With `up(X)` = X plus its ancestors, and the
/// subtypes of T = every class-like X, indexed or anonymous, with T in
/// `up(X)`, R and T are related when:
///
/// - (a) R is in `up(X)` for some subtype X of T (by indexed ID, or by
///   qualified name ignoring ASCII case); or
/// - (b) R is not indexed and some subtype X of T has an unindexed link in
///   its ancestor closure, above which R could sit; or
/// - any declared supertype anywhere has no qualified name: it could name any
///   class, so rule 2 never excludes in that snapshot; or
/// - T may be a trait, or R is an indexed declaration that may be a trait
///   ([`possibly_trait`]): trait use is not tracked.
pub struct ClassRelation {
    /// Union of the up-sets of every subtype of T.
    ids: BTreeSet<String>,
    qnames: BTreeSet<String>,
    /// Whether some subtype of T has an unindexed link.
    unindexed_link: bool,
}

impl ClassRelation {
    /// The relation for a member `target` whose direct container is
    /// `target_parent`, or `None` when rule 2 never applies: the target is not
    /// a method, property, or constant of a class-like, that class-like may be
    /// a trait, or the snapshot has a supertype with no qualified name.
    pub fn for_target(
        index: &SubtypeIndex,
        target: &SymbolRow,
        target_parent: Option<&SymbolRow>,
    ) -> Option<ClassRelation> {
        if !matches!(
            target.kind,
            SymbolKind::Method | SymbolKind::Property | SymbolKind::Const
        ) {
            return None;
        }
        let parent = target_parent?;
        if !is_class_like(parent.kind) || possibly_trait(parent) || index.unknown_link {
            return None;
        }
        let mut relation = ClassRelation {
            ids: BTreeSet::new(),
            qnames: BTreeSet::new(),
            unindexed_link: false,
        };
        for set in &index.sets {
            if set.contains(Some(&parent.id), &parent.qualified_name) {
                relation.ids.extend(set.ids.iter().cloned());
                relation.qnames.extend(set.qnames.iter().cloned());
                relation.unindexed_link |= set.unindexed_link;
            }
        }
        Some(relation)
    }

    /// Whether `receiver` is known to be unrelated to the target's class.
    ///
    /// `receiver_row` is the indexed declaration `receiver.class_id` names;
    /// a `class_id` that names no indexed class-like is never unrelated.
    pub fn unrelated(&self, receiver: &ReceiverClassRow, receiver_row: Option<&SymbolRow>) -> bool {
        let receiver_id = receiver.class_id.as_deref();
        if receiver_id.is_some() {
            match receiver_row {
                Some(row) if is_class_like(row.kind) && !possibly_trait(row) => {}
                _ => return false,
            }
        }
        let in_subtype_up_set = receiver_id.is_some_and(|id| self.ids.contains(id))
            || self
                .qnames
                .contains(&receiver.class_qname.to_ascii_lowercase());
        let above_unknown_link = receiver_id.is_none() && self.unindexed_link;
        !(in_subtype_up_set || above_unknown_link)
    }
}

#[cfg(test)]
mod tests {
    use super::{form_compatible, possibly_trait, strip_attribute_groups};
    use rivet_core::{RefKind, SymbolKind};
    use rivet_store::{SymbolRow, UseRow};

    fn symbol(kind: SymbolKind, signature: Option<&str>) -> SymbolRow {
        SymbolRow {
            id: "a.php#X".to_string(),
            file: "a.php".to_string(),
            name: "X".to_string(),
            lookup_name: "x".to_string(),
            qualified_name: "X".to_string(),
            kind,
            parent_id: None,
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            end_line: 1,
            signature: signature.map(str::to_string),
            doc_comment: None,
        }
    }

    fn use_of(ref_kind: RefKind, receiver: Option<&str>) -> UseRow {
        UseRow {
            use_id: Some(1),
            file: "b.php".to_string(),
            containing_symbol: None,
            scope_key: "top:file".to_string(),
            spelling: "x".to_string(),
            lookup_name: "x".to_string(),
            ref_kind,
            start_byte: 0,
            end_byte: 1,
            line: 1,
            col: 1,
            receiver: receiver.map(str::to_string),
            hint_json: "{\"kind\":\"unresolved\"}".to_string(),
        }
    }

    #[test]
    fn trait_detection_reads_the_header_after_attributes() {
        let class = |sig: &str| symbol(SymbolKind::Class, Some(sig));
        assert!(!possibly_trait(&class("class K")));
        assert!(!possibly_trait(&class("final class K extends B")));
        assert!(!possibly_trait(&class("abstract class K")));
        assert!(!possibly_trait(&class("readonly final class K")));
        assert!(!possibly_trait(&class("#[Attr(1)] final class K")));
        assert!(!possibly_trait(&class(
            "#[A('x]y'), B([1, [2]])] #[C] class K"
        )));
        assert!(possibly_trait(&class("trait T")));
        assert!(possibly_trait(&class("#[Attr] trait T")));
        // An attribute group that cannot be delimited is not proof.
        assert!(possibly_trait(&class("#[A /* ] */] class K")));
        assert!(possibly_trait(&class("#[A(")));
        assert!(possibly_trait(&symbol(SymbolKind::Class, None)));
        assert!(!possibly_trait(&symbol(SymbolKind::Interface, None)));
        assert!(!possibly_trait(&symbol(SymbolKind::Enum, Some("enum E"))));
        assert_eq!(strip_attribute_groups("#[A] #[B] class K"), Some("class K"));
    }

    #[test]
    fn form_table_matches_the_documented_rows() {
        use RefKind::*;
        let class = symbol(SymbolKind::Class, Some("class K"));
        // (target kind, is a class member, kept forms, excluded forms), each
        // form a `(ref_kind, has a receiver)` pair.
        type Forms = &'static [(RefKind, bool)];
        let cases: &[(SymbolKind, bool, Forms, Forms)] = &[
            (
                SymbolKind::Method,
                true,
                &[
                    (Call, true),
                    (Read, true),
                    (Unknown, false),
                    (Unknown, true),
                ],
                &[
                    (Call, false),
                    (Read, false),
                    (Write, true),
                    (Type, false),
                    (Import, false),
                ],
            ),
            (
                SymbolKind::Function,
                false,
                &[(Call, false), (Import, false), (Unknown, false)],
                &[(Call, true), (Read, true), (Type, false)],
            ),
            (
                SymbolKind::Property,
                true,
                &[(Read, true), (Write, true), (Unknown, false)],
                &[(Call, true), (Call, false), (Type, false), (Import, false)],
            ),
            (
                SymbolKind::Const,
                true,
                &[(Read, true), (Unknown, false)],
                &[(Write, true), (Call, true), (Type, false), (Import, false)],
            ),
            (
                SymbolKind::Const,
                false,
                &[(Import, false), (Unknown, false)],
                &[(Read, true), (Call, false), (Type, false)],
            ),
            (
                SymbolKind::Class,
                false,
                &[(Type, false), (Import, false), (Unknown, false)],
                &[(Call, false), (Call, true), (Read, true), (Write, true)],
            ),
            (
                SymbolKind::Module,
                false,
                &[(Call, true), (Type, false), (Read, true)],
                &[],
            ),
        ];
        for (kind, member, kept, excluded) in cases {
            let target = symbol(*kind, Some("x"));
            let parent = member.then_some(&class);
            for (ref_kind, receiver) in *kept {
                let row = use_of(*ref_kind, receiver.then_some("$x"));
                assert!(
                    form_compatible(&target, parent, &row),
                    "{kind:?} keeps {ref_kind:?} receiver={receiver}"
                );
            }
            for (ref_kind, receiver) in *excluded {
                let row = use_of(*ref_kind, receiver.then_some("$x"));
                assert!(
                    !form_compatible(&target, parent, &row),
                    "{kind:?} excludes {ref_kind:?} receiver={receiver}"
                );
            }
        }
    }
}
