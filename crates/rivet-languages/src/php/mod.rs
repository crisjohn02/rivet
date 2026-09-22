//! PHP named-definition and identifier-use extraction.
//!
//! T11 extracts namespaces, classes, interfaces, traits (as class-kind),
//! enums, functions, methods, properties, and constants into owned
//! [`ExtractedSymbol`] records with native qualified names
//! (`App\Services\SurveyService::launch`). T14 fills each record's collapsed
//! `signature` and attached `doc_comment`; T17 adds owned `ExtractedUse` and
//! `ExtractedImport` records with lexical receiver hints; T25 adds enum cases
//! and promoted constructor properties (both mapped onto existing contract
//! kinds). Cross-file resolution is a later task.
//!
//! Parse policy (docs/ARCHITECTURE.md "Parse and coverage policy"): a file
//! whose Tree-sitter tree contains any ERROR or MISSING node publishes no
//! facts and yields one `parse_error` diagnostic. A file that exceeds either
//! deterministic resource bound of [`ResourceLimits`] (spec §27: visited
//! syntax nodes, extracted uses) publishes no facts and yields one
//! `resource_limit` diagnostic instead. The node bound is checked first, so a
//! file that is both too large and malformed is `resource_limit`.

use std::collections::HashMap;
use std::sync::OnceLock;

use rivet_core::{Diagnostic, ExtractedFile, ExtractedSymbol, Span, SymbolKind};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

use crate::ResourceLimits;

mod namespaces;
mod signature;
mod uses;
pub use signature::{order_members, signature_summary};

/// The compiled-once symbol query for the pinned PHP grammar.
static SYMBOLS_QUERY: OnceLock<Query> = OnceLock::new();

/// The process-wide compiled symbol query.
fn symbols_query() -> &'static Query {
    SYMBOLS_QUERY.get_or_init(|| {
        let language = crate::grammar(crate::LanguageId::Php);
        Query::new(&language, include_str!("symbols.scm"))
            .expect("php symbols.scm must compile against the pinned grammar")
    })
}

/// Extract the named definitions of one parsed PHP file.
///
/// `source` must be exactly the bytes that produced `tree`. The result is
/// sorted by declaration start byte so persistence and tests are
/// deterministic. On a parse or resource failure the symbol list is empty and
/// a single diagnostic explains the skip.
///
/// Uses the spec's default [`ResourceLimits`]; see [`extract_with_limits`].
pub fn extract(source: &[u8], tree: &Tree) -> ExtractedFile {
    extract_with_limits(source, tree, ResourceLimits::DEFAULT)
}

/// [`extract`] with explicit resource bounds.
///
/// Both bounds are counts. The node count is taken by one pre-order visit of
/// the whole tree before any extraction, so a file over the node bound does no
/// extraction work at all. The use count is taken while the use walker records
/// uses: the walker stops recording, and stops descending, as soon as one use
/// more than the bound has been seen. Either way the result is no symbols,
/// uses, imports, or scopes and one `resource_limit` diagnostic.
pub fn extract_with_limits(source: &[u8], tree: &Tree, limits: ResourceLimits) -> ExtractedFile {
    let root = tree.root_node();
    let scan = scan_tree(root, limits.max_visited_nodes);
    if matches!(scan, Scan::ResourceLimit) {
        return resource_limit(format!(
            "visited more than {} Tree-sitter nodes",
            limits.max_visited_nodes
        ));
    }
    if root.has_error() {
        let (kind, byte) = match scan {
            Scan::Done(Some((kind, byte))) => (kind, byte),
            _ => ("ERROR".to_string(), root.start_byte() as u32),
        };
        return ExtractedFile {
            symbols: Vec::new(),
            uses: Vec::new(),
            imports: Vec::new(),
            scopes: Vec::new(),
            diagnostics: vec![Diagnostic {
                code: "parse_error".to_string(),
                detail: format!("{kind} at byte {byte}"),
                start_byte: Some(byte),
            }],
        };
    }

    let layout = namespaces::NamespaceLayout::new(root, source);
    let raw = collect_raw(source, root, &layout);
    let symbols = build_symbols(raw);
    let Some((uses, imports, scopes)) =
        uses::extract_uses(source, root, &symbols, &layout, limits.max_extracted_uses)
    else {
        return resource_limit(format!(
            "extracted more than {} uses",
            limits.max_extracted_uses
        ));
    };
    ExtractedFile {
        symbols,
        uses,
        imports,
        scopes,
        diagnostics: Vec::new(),
    }
}

/// The no-facts result of a file that exceeded a resource bound.
fn resource_limit(detail: String) -> ExtractedFile {
    ExtractedFile {
        symbols: Vec::new(),
        uses: Vec::new(),
        imports: Vec::new(),
        scopes: Vec::new(),
        diagnostics: vec![Diagnostic {
            code: "resource_limit".to_string(),
            detail,
            start_byte: None,
        }],
    }
}

/// A declaration before namespace/parent resolution.
struct RawSymbol {
    kind: SymbolKind,
    name: String,
    start_byte: u32,
    end_byte: u32,
    namespace: Option<String>,
    node_id: usize,
    container_id: Option<usize>,
    signature: String,
    doc_comment: Option<String>,
}

/// Walk the tree once with a `TreeCursor`, counting nodes and locating the
/// first ERROR or MISSING node in pre-order.
enum Scan {
    ResourceLimit,
    Done(Option<(String, u32)>),
}

///
/// Every node is counted, named or anonymous; more than `max_nodes` stops the
/// walk immediately.
fn scan_tree(root: Node<'_>, max_nodes: u64) -> Scan {
    let has_error = root.has_error();
    let mut first_error: Option<(String, u32)> = None;
    let mut visited: u64 = 0;
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        visited += 1;
        if visited > max_nodes {
            return Scan::ResourceLimit;
        }
        if has_error && first_error.is_none() && (node.is_error() || node.is_missing()) {
            first_error = Some((node.kind().to_string(), node.start_byte() as u32));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Scan::Done(first_error);
            }
        }
    }
}

/// Run the symbol query and turn captures into unresolved raw records.
fn collect_raw(
    source: &[u8],
    root: Node<'_>,
    layout: &namespaces::NamespaceLayout,
) -> Vec<RawSymbol> {
    let query = symbols_query();
    let capture_names = query.capture_names();
    let mut query_cursor = QueryCursor::new();
    let mut matches = query_cursor.matches(query, root, source);
    let mut raw: Vec<RawSymbol> = Vec::new();
    let mut child_cursor = root.walk();
    let mut element_cursor = root.walk();

    while let Some(m) = matches.next() {
        let mut kind: Option<SymbolKind> = None;
        let mut declaration: Option<Node> = None;
        let mut name_node: Option<Node> = None;
        for capture in m.captures() {
            match capture_names[capture.index as usize] {
                "symbol.module" => {
                    kind = Some(SymbolKind::Module);
                    declaration = Some(capture.node);
                }
                "symbol.class" => {
                    kind = Some(SymbolKind::Class);
                    declaration = Some(capture.node);
                }
                "symbol.interface" => {
                    kind = Some(SymbolKind::Interface);
                    declaration = Some(capture.node);
                }
                "symbol.enum" => {
                    kind = Some(SymbolKind::Enum);
                    declaration = Some(capture.node);
                }
                "symbol.function" => {
                    kind = Some(SymbolKind::Function);
                    declaration = Some(capture.node);
                }
                "symbol.method" => {
                    kind = Some(SymbolKind::Method);
                    declaration = Some(capture.node);
                }
                "symbol.property" => {
                    kind = Some(SymbolKind::Property);
                    declaration = Some(capture.node);
                }
                "symbol.const" => {
                    kind = Some(SymbolKind::Const);
                    declaration = Some(capture.node);
                }
                "symbol.name" => name_node = Some(capture.node),
                _ => {}
            }
        }
        let (Some(kind), Some(declaration)) = (kind, declaration) else {
            continue;
        };
        // Anonymous definitions are not addressable symbols in v0.1
        // (spec §10.1), so their members are skipped rather than emitted as
        // parentless top-level declarations.
        if matches!(
            kind,
            SymbolKind::Method | SymbolKind::Property | SymbolKind::Const
        ) && in_anonymous_container(declaration)
        {
            continue;
        }
        let start_byte = declaration.start_byte() as u32;
        let end_byte = declaration.end_byte() as u32;
        if start_byte >= end_byte {
            continue;
        }
        // One call fills the signature and doc comment shared by every member
        // of this declaration node; multi-member property/constant
        // declarations share the whole-node span as well.
        let (signature_text, doc_comment) = signature::declaration_header(declaration, source);
        let header = DeclarationHeader {
            start_byte,
            end_byte,
            signature: signature_text,
            doc_comment,
        };
        match kind {
            SymbolKind::Module => {
                // A global `namespace { }` block has no name and is not an
                // addressable symbol; `layout` still records it as a block.
                if let Some(name) = name_node {
                    let name = node_text(name, source);
                    raw.push(RawSymbol {
                        kind,
                        name,
                        start_byte,
                        end_byte,
                        namespace: None,
                        node_id: declaration.id(),
                        container_id: None,
                        signature: header.signature,
                        doc_comment: header.doc_comment,
                    });
                }
            }
            // A property or constant declaration can name several members at
            // once; walk its element list. Each member shares the whole
            // declaration span, per the adapter's "whole declaration node"
            // span rule.
            SymbolKind::Property => {
                // A promoted constructor property is one property declared in
                // a parameter list, so it has no `property_element` children.
                if declaration.kind() == "property_promotion_parameter" {
                    if let Some(name) = name_node {
                        raw.push(member(
                            kind,
                            &header,
                            node_text(name, source),
                            declaration.id(),
                            enclosing_container(declaration),
                        ));
                    }
                    continue;
                }
                for element in declaration.children(&mut child_cursor) {
                    if element.kind() != "property_element" {
                        continue;
                    }
                    let Some(name) = element.child_by_field_name("name") else {
                        continue;
                    };
                    raw.push(member(
                        kind,
                        &member_header(&header, declaration, element, source),
                        node_text(name, source),
                        declaration.id(),
                        enclosing_container(element),
                    ));
                }
            }
            SymbolKind::Const => {
                // An enum case is declared alone (`case Hearts;`), not through
                // a `const_element` list.
                if declaration.kind() == "enum_case" {
                    if let Some(name) = name_node {
                        raw.push(member(
                            kind,
                            &header,
                            node_text(name, source),
                            declaration.id(),
                            enclosing_container(declaration),
                        ));
                    }
                    continue;
                }
                for element in declaration.children(&mut child_cursor) {
                    if element.kind() != "const_element" {
                        continue;
                    }
                    // `const_element` has no `name` field in the pinned
                    // grammar; its name is a plain `name` child.
                    let Some(name) = element
                        .children(&mut element_cursor)
                        .find(|child| child.kind() == "name")
                    else {
                        continue;
                    };
                    raw.push(member(
                        kind,
                        &member_header(&header, declaration, element, source),
                        node_text(name, source),
                        declaration.id(),
                        enclosing_container(element),
                    ));
                }
            }
            _ => {
                let Some(name) = name_node else {
                    continue;
                };
                let container = match kind {
                    SymbolKind::Method => enclosing_container(declaration),
                    _ => None,
                };
                raw.push(member(
                    kind,
                    &header,
                    node_text(name, source),
                    declaration.id(),
                    container,
                ));
            }
        }
    }

    // Namespaces are siblings of the declarations they cover in the semicolon
    // form, so the shared layout associates by block range rather than
    // ancestry. A global `namespace { }` block yields no namespace, so its
    // declarations keep bare global names (AF1).
    for record in &mut raw {
        if record.kind == SymbolKind::Module {
            continue;
        }
        record.namespace = layout.namespace_at(record.start_byte);
    }
    raw
}

/// Builds a raw member record from its declaration node's shared fields.
fn member(
    kind: SymbolKind,
    header: &DeclarationHeader,
    name: String,
    node_id: usize,
    container: Option<Node<'_>>,
) -> RawSymbol {
    RawSymbol {
        kind,
        name,
        start_byte: header.start_byte,
        end_byte: header.end_byte,
        namespace: None,
        node_id,
        container_id: container.map(|node| node.id()),
        signature: header.signature.clone(),
        doc_comment: header.doc_comment.clone(),
    }
}

/// The shared declaration fields with a signature that names only `element`.
///
/// A multi-name property or constant declaration yields one symbol per element,
/// all sharing the whole declaration span and doc comment. Replacing only the
/// signature keeps each member's own name in its header without moving any
/// stored byte span.
fn member_header(
    header: &DeclarationHeader,
    declaration: Node<'_>,
    element: Node<'_>,
    source: &[u8],
) -> DeclarationHeader {
    DeclarationHeader {
        start_byte: header.start_byte,
        end_byte: header.end_byte,
        signature: signature::element_header(declaration, element, source),
        doc_comment: header.doc_comment.clone(),
    }
}

/// The span, signature, and doc comment shared by every member of one
/// declaration node (a property/constant declaration can name several).
struct DeclarationHeader {
    start_byte: u32,
    end_byte: u32,
    signature: String,
    doc_comment: Option<String>,
}

/// Sort records, resolve parents and qualified names, and freeze them into
/// owned [`ExtractedSymbol`]s.
fn build_symbols(mut raw: Vec<RawSymbol>) -> Vec<ExtractedSymbol> {
    raw.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then(a.end_byte.cmp(&b.end_byte))
    });

    let container_index: HashMap<usize, usize> = raw
        .iter()
        .enumerate()
        .filter(|(_, record)| is_container(record.kind))
        .map(|(index, record)| (record.node_id, index))
        .collect();

    // Top-level and container qualified names first, then members using their
    // container's already-computed name.
    let mut qualified_names: Vec<String> = raw
        .iter()
        .map(|record| qualified(record.namespace.as_deref(), &record.name))
        .collect();
    for (index, record) in raw.iter().enumerate() {
        if let Some(parent) = record
            .container_id
            .and_then(|id| container_index.get(&id).copied())
        {
            qualified_names[index] = format!("{}::{}", qualified_names[parent], record.name);
        }
    }

    raw.iter()
        .zip(qualified_names)
        .map(|(record, qualified_name)| {
            let parent_index = match record.kind {
                SymbolKind::Method | SymbolKind::Property | SymbolKind::Const => record
                    .container_id
                    .and_then(|id| container_index.get(&id).copied()),
                _ => None,
            };
            ExtractedSymbol {
                qualified_name,
                name: record.name.clone(),
                kind: record.kind,
                span: Span::new(record.start_byte, record.end_byte)
                    .expect("a declaration span is non-empty"),
                parent_index,
                signature: Some(record.signature.clone()),
                doc_comment: record.doc_comment.clone(),
            }
        })
        .collect()
}

/// The nearest enclosing class/interface/trait/enum declaration, if any.
///
/// Namespaces are deliberately not containers: the spec's parent field means
/// "parent class or module" only for members.
fn enclosing_container(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.parent();
    while let Some(candidate) = current {
        if is_container_node(candidate.kind()) {
            return Some(candidate);
        }
        current = candidate.parent();
    }
    None
}

fn is_container(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
    )
}

fn is_container_node(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration" | "interface_declaration" | "trait_declaration" | "enum_declaration"
    )
}

/// Reports whether `node` is a member of an anonymous class-like declaration.
///
/// The pinned grammar names an anonymous class body `anonymous_class`; it has
/// no `class_declaration` node, so its members would otherwise be emitted as
/// parentless top-level symbols. Such members are skipped because anonymous
/// definitions are not addressable in v0.1 (spec §10.1).
fn in_anonymous_container(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(candidate) = current {
        if candidate.kind() == "anonymous_class" {
            return true;
        }
        if is_container_node(candidate.kind()) {
            return false;
        }
        current = candidate.parent();
    }
    false
}

/// The case-folded form used for short-name (`lookup_name`) matching.
///
/// PHP class, interface, trait, enum, namespace, function, and method names
/// are case-insensitive, so their lookup names are lowercased. Property and
/// constant names are case-sensitive and keep their exact spelling (a PHP
/// property keeps its leading `$`). Enum cases are recorded as constants and
/// are case-sensitive too, matching PHP.
///
/// PHP folds identifier case for ASCII letters only, so `Ä` and `ä` name
/// different classes; the lowercasing here is ASCII-only to match (AF2). The
/// query side (`rivet_index::query`) and the resolver fold the same way.
pub fn lookup_name(name: &str, kind: SymbolKind) -> String {
    match kind {
        SymbolKind::Property | SymbolKind::Const => name.to_string(),
        _ => name.to_ascii_lowercase(),
    }
}

/// The normalized short name of a use spelling (AF4): the last segment of a
/// qualified spelling, with any `namespace\` prefix and leading `\` removed,
/// and without a leading `$`.
///
/// `\App\Foo`, `Sub\Missing\Foo`, and `namespace\Foo` all give `Foo`;
/// `$count` gives `count`. Case is left as written; folding depends on the
/// kind of declaration the use is compared with.
pub fn use_short_name(spelling: &str) -> &str {
    let last = spelling.rsplit('\\').next().unwrap_or(spelling);
    last.strip_prefix('$').unwrap_or(last)
}

/// Join a namespace and a short name in the PHP native form.
fn qualified(namespace: Option<&str>, local: &str) -> String {
    match namespace {
        Some(namespace) if !namespace.is_empty() => format!("{namespace}\\{local}"),
        _ => local.to_string(),
    }
}

fn node_text(node: Node<'_>, source: &[u8]) -> String {
    String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{lookup_name, use_short_name};
    use rivet_core::SymbolKind;

    #[test]
    fn use_short_name_keeps_the_last_segment_without_a_dollar() {
        assert_eq!(use_short_name("\\App\\Foo"), "Foo");
        assert_eq!(use_short_name("Sub\\Missing\\Foo"), "Foo");
        assert_eq!(use_short_name("namespace\\Foo"), "Foo");
        assert_eq!(use_short_name("LIMIT"), "LIMIT");
        assert_eq!(use_short_name("$count"), "count");
        assert_eq!(use_short_name("count"), "count");
    }

    #[test]
    fn lookup_name_folds_ascii_only_and_only_for_case_insensitive_kinds() {
        assert_eq!(
            lookup_name("SurveyService", SymbolKind::Class),
            "surveyservice"
        );
        assert_eq!(lookup_name("Launch", SymbolKind::Method), "launch");
        assert_eq!(lookup_name("ÄrgerNis", SymbolKind::Class), "Ärgernis");
        assert_eq!(lookup_name("Ä", SymbolKind::Function), "Ä");
        assert_eq!(lookup_name("$Items", SymbolKind::Property), "$Items");
        assert_eq!(lookup_name("MAX", SymbolKind::Const), "MAX");
    }
}
