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
//! facts and yields one `parse_error` diagnostic. A file that exceeds the
//! deterministic node cap yields one `resource_limit` diagnostic instead.

use std::collections::HashMap;
use std::sync::OnceLock;

use rivet_core::{Diagnostic, ExtractedFile, ExtractedSymbol, Span, SymbolKind};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

mod signature;
mod uses;
pub use signature::signature_summary;

/// The compiled-once symbol query for the pinned PHP grammar.
static SYMBOLS_QUERY: OnceLock<Query> = OnceLock::new();

/// Hard cap on Tree-sitter nodes visited per file, before any extraction.
const MAX_VISITED_NODES: u64 = 1_000_000;

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
pub fn extract(source: &[u8], tree: &Tree) -> ExtractedFile {
    let root = tree.root_node();
    let scan = scan_tree(root);
    if matches!(scan, Scan::ResourceLimit) {
        return ExtractedFile {
            symbols: Vec::new(),
            uses: Vec::new(),
            imports: Vec::new(),
            scopes: Vec::new(),
            diagnostics: vec![Diagnostic {
                code: "resource_limit".to_string(),
                detail: format!("visited more than {MAX_VISITED_NODES} Tree-sitter nodes"),
                start_byte: None,
            }],
        };
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

    let raw = collect_raw(source, root);
    let symbols = build_symbols(raw);
    let (uses, imports, scopes) = uses::extract_uses(source, root, &symbols);
    ExtractedFile {
        symbols,
        uses,
        imports,
        scopes,
        diagnostics: Vec::new(),
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

fn scan_tree(root: Node<'_>) -> Scan {
    let has_error = root.has_error();
    let mut first_error: Option<(String, u32)> = None;
    let mut visited: u64 = 0;
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        visited += 1;
        if visited > MAX_VISITED_NODES {
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
fn collect_raw(source: &[u8], root: Node<'_>) -> Vec<RawSymbol> {
    let query = symbols_query();
    let capture_names = query.capture_names();
    let mut query_cursor = QueryCursor::new();
    let mut matches = query_cursor.matches(query, root, source);
    let mut raw: Vec<RawSymbol> = Vec::new();
    let mut namespaces: Vec<(u32, Option<String>)> = Vec::new();
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
                if let Some(name) = name_node {
                    let name = node_text(name, source);
                    namespaces.push((start_byte, Some(name.clone())));
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
    // form, so associate by source order rather than ancestry.
    namespaces.sort_by_key(|(start, _)| *start);
    for record in &mut raw {
        if record.kind == SymbolKind::Module {
            continue;
        }
        record.namespace = namespace_at(&namespaces, record.start_byte);
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
pub fn lookup_name(name: &str, kind: SymbolKind) -> String {
    match kind {
        SymbolKind::Property | SymbolKind::Const => name.to_string(),
        _ => name.to_lowercase(),
    }
}

/// The active namespace name at `byte`, by source order.
fn namespace_at(namespaces: &[(u32, Option<String>)], byte: u32) -> Option<String> {
    let index = namespaces.partition_point(|(start, _)| *start < byte);
    if index == 0 {
        None
    } else {
        namespaces[index - 1].1.clone()
    }
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
