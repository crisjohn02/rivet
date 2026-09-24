//! The TypeScript definition walk (T42).
//!
//! A `.scm` query matches a node wherever it occurs, but whether a TypeScript
//! node is a definition depends on where it sits and on its siblings, so the
//! definitions come from this walk instead. It starts at the file's top level
//! and descends only into declaration bodies:
//!
//! - the file itself and an identifier-named `namespace`/`module` body
//!   (statements);
//! - a named class body, including a class expression bound to a `const`
//!   (members);
//! - an interface body and an enum body (members).
//!
//! It never descends into a function or method body, an arrow function, an
//! object literal, a block, an anonymous class or function, a string-named
//! ambient module, or any other expression. So function-local declarations,
//! anonymous classes and their members, object-literal methods, and
//! declarations nested in a top-level block are never symbols. An
//! `export_statement` or `ambient_declaration` (`declare`) wrapper is
//! transparent, and the symbol's span is the wrapper, so it includes `export`,
//! `export default`, and `declare`.
//!
//! Overload signatures fold after the walk ([`fold_overloads`]). The walk
//! records each function or method with its overload identity and its body
//! scope; the fold keeps the implementation of each overload set, or its first
//! signature when it has none.
//!
//! Only identifier names make symbols. A member named by a string or number
//! literal (`"content-type": string`) or a computed name (`[Symbol.iterator]`)
//! is skipped, as a string-named ambient module is, and so is a `const`
//! binding to a destructuring pattern.

use std::collections::BTreeMap;

use rivet_core::{ExtractedSymbol, Span, SymbolKind};
use tree_sitter::Node;

use super::signature;

/// Extracts every named definition under `root`, in declaration start order.
pub(super) fn extract_symbols(source: &[u8], root: Node<'_>) -> Vec<ExtractedSymbol> {
    let mut walk = Walk {
        source,
        records: Vec::new(),
    };
    walk.statements(root, &Scope::top(root));
    let keep = fold_overloads(&walk.records);
    freeze(walk.records, &keep)
}

/// Where a declaration sits.
struct Scope {
    /// The enclosing container's qualified name, or `None` at file level.
    prefix: Option<String>,
    /// The enclosing container's index in [`Walk::records`].
    parent: Option<usize>,
    /// The body node that holds the declaration. Overload signatures fold
    /// only with an implementation in the same body.
    body: usize,
}

impl Scope {
    /// The file (or global) scope, whose declarations have no prefix.
    fn top(body: Node<'_>) -> Scope {
        Scope {
            prefix: None,
            parent: None,
            body: body.id(),
        }
    }
}

/// Whether a method is a getter, a setter, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Accessor {
    Neither,
    Get,
    Set,
}

/// What makes two functions or methods one overload set, and whether this
/// declaration is an implementation.
#[derive(Debug, Clone, Copy)]
struct Callable {
    accessor: Accessor,
    is_static: bool,
    /// Whether the declaration has a body. A bodyless one is a signature.
    implementation: bool,
}

/// One definition before overload folding and ordering.
struct Record {
    kind: SymbolKind,
    name: String,
    qualified_name: String,
    span: Span,
    name_span: Span,
    parent: Option<usize>,
    body: usize,
    callable: Option<Callable>,
    signature: String,
    doc_comment: Option<String>,
}

/// A definition the walk found, before it gets its scope.
struct Def<'t> {
    kind: SymbolKind,
    /// The name node; its text is the short name and its range the name span.
    name: Node<'t>,
    /// The qualified-name component: the name, or a dotted namespace path.
    local: String,
    /// The node whose range is the symbol's span; its doc comment attaches.
    span: Node<'t>,
    signature: String,
    callable: Option<Callable>,
}

struct Walk<'s> {
    source: &'s [u8],
    records: Vec<Record>,
}

impl Walk<'_> {
    /// Visits the statements of the file or of a namespace body.
    fn statements(&mut self, block: Node<'_>, scope: &Scope) {
        for statement in named_children(block) {
            self.statement(statement, statement, scope);
        }
    }

    /// Visits one statement. `outer` is the node whose range becomes the
    /// span: the statement itself, or the `export`/`declare` wrapper around
    /// it.
    fn statement(&mut self, node: Node<'_>, outer: Node<'_>, scope: &Scope) {
        match node.kind() {
            // `export default <expression>`, `export { ... }`, `export =`,
            // and `export * from` have no `declaration` field and declare
            // nothing: an anonymous default-exported class or function is not
            // a symbol, and neither are its members.
            "export_statement" => {
                if let Some(declaration) = node.child_by_field_name("declaration") {
                    self.statement(declaration, outer, scope);
                }
            }
            "ambient_declaration" => self.ambient(node, outer, scope),
            // A bare `namespace X {}` statement parses as an expression
            // statement holding the namespace. The statement is the span, as
            // for any other statement, so a doc comment above it attaches
            // (the namespace node has no preceding sibling to find).
            "expression_statement" => {
                if let Some(inner) = named_children(node)
                    .into_iter()
                    .find(|child| child.kind() == "internal_module")
                {
                    self.statement(inner, outer, scope);
                }
            }
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                let Some(name) = identifier_name(node) else {
                    return;
                };
                let callable = Callable {
                    accessor: Accessor::Neither,
                    is_static: false,
                    implementation: node.child_by_field_name("body").is_some(),
                };
                self.push(
                    Def {
                        kind: SymbolKind::Function,
                        name,
                        local: self.text(name),
                        span: outer,
                        signature: signature::declaration(outer, node, self.source),
                        callable: Some(callable),
                    },
                    scope,
                );
            }
            "class_declaration" | "abstract_class_declaration" => {
                let Some(name) = identifier_name(node) else {
                    return;
                };
                let def = Def {
                    kind: SymbolKind::Class,
                    name,
                    local: self.text(name),
                    span: outer,
                    signature: signature::declaration(outer, node, self.source),
                    callable: None,
                };
                if let Some(index) = self.push(def, scope)
                    && let Some(body) = node.child_by_field_name("body")
                {
                    self.class_body(body, &self.member_scope(index, body));
                }
            }
            "interface_declaration" => self.interface(node, outer, scope),
            "enum_declaration" => self.enumeration(node, outer, scope),
            "internal_module" | "module" => self.module(node, outer, scope),
            "type_alias_declaration" => {
                let Some(name) = identifier_name(node) else {
                    return;
                };
                self.push(
                    Def {
                        kind: SymbolKind::TypeAlias,
                        name,
                        local: self.text(name),
                        span: outer,
                        signature: signature::declaration(outer, node, self.source),
                        callable: None,
                    },
                    scope,
                );
            }
            "lexical_declaration" => self.consts(node, outer, scope),
            _ => {}
        }
    }

    /// A `declare` wrapper. `declare global { ... }` augments the global
    /// scope: the block is no symbol, and its declarations are named as
    /// top-level ones. Any other ambient declaration is named exactly as the
    /// same declaration without `declare`.
    fn ambient(&mut self, node: Node<'_>, outer: Node<'_>, scope: &Scope) {
        let mut cursor = node.walk();
        let global = node
            .children(&mut cursor)
            .any(|child| !child.is_named() && child.kind() == "global");
        let children = named_children(node);
        if global {
            if let Some(block) = children
                .into_iter()
                .find(|child| child.kind() == "statement_block")
            {
                self.statements(block, &Scope::top(block));
            }
            return;
        }
        if let Some(declaration) = children.into_iter().find(|child| child.kind() != "comment") {
            self.statement(declaration, outer, scope);
        }
    }

    /// `const` bindings. Each identifier-named declarator is one symbol with
    /// the whole statement's span: a `function` when bound to an arrow
    /// function, function expression, or generator function; a `class` when
    /// bound to a class expression, whose members are then walked; otherwise
    /// a `const`. `let` and `var` bindings are not symbols.
    fn consts(&mut self, node: Node<'_>, outer: Node<'_>, scope: &Scope) {
        if node
            .child_by_field_name("kind")
            .is_none_or(|kind| kind.kind() != "const")
        {
            return;
        }
        let declarators: Vec<Node<'_>> = named_children(node)
            .into_iter()
            .filter(|child| child.kind() == "variable_declarator")
            .collect();
        let Some(&first) = declarators.first() else {
            return;
        };
        for declarator in declarators {
            let Some(name) = declarator
                .child_by_field_name("name")
                .filter(|name| name.kind() == "identifier")
            else {
                continue;
            };
            let value = declarator.child_by_field_name("value");
            let kind = match value.map(|value| value.kind()) {
                Some("arrow_function" | "function_expression" | "generator_function") => {
                    SymbolKind::Function
                }
                Some("class") => SymbolKind::Class,
                _ => SymbolKind::Const,
            };
            let def = Def {
                kind,
                name,
                local: self.text(name),
                span: outer,
                signature: signature::binding(outer, first, declarator, self.source),
                callable: None,
            };
            if let Some(index) = self.push(def, scope)
                && kind == SymbolKind::Class
                && let Some(body) = value.and_then(|value| value.child_by_field_name("body"))
            {
                self.class_body(body, &self.member_scope(index, body));
            }
        }
    }

    /// The members of a named class: methods (including the constructor,
    /// accessors, and overload signatures), fields, and the constructor's
    /// parameter properties. Index signatures and static blocks are not
    /// symbols.
    fn class_body(&mut self, body: Node<'_>, scope: &Scope) {
        for member in named_children(body) {
            match member.kind() {
                "method_definition" | "method_signature" | "abstract_method_signature" => {
                    self.method(member, scope);
                }
                "public_field_definition" => self.property(member, scope),
                _ => {}
            }
        }
    }

    /// A method, getter, setter, or constructor, with or without a body.
    fn method(&mut self, node: Node<'_>, scope: &Scope) {
        let Some(name) = member_name(node) else {
            return;
        };
        // `get`, `set`, and `static` are anonymous keyword tokens before the
        // name; a method *named* `get` or `static` has a named name node.
        let mut accessor = Accessor::Neither;
        let mut is_static = false;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.start_byte() >= name.start_byte() {
                break;
            }
            if child.is_named() {
                continue;
            }
            match child.kind() {
                "get" => accessor = Accessor::Get,
                "set" => accessor = Accessor::Set,
                "static" => is_static = true,
                _ => {}
            }
        }
        let implementation = node.child_by_field_name("body").is_some();
        let local = self.text(name);
        let is_constructor = local == "constructor";
        let def = Def {
            kind: SymbolKind::Method,
            name,
            local,
            span: node,
            signature: signature::declaration(node, node, self.source),
            callable: Some(Callable {
                accessor,
                is_static,
                implementation,
            }),
        };
        if self.push(def, scope).is_some() && is_constructor && implementation {
            self.parameter_properties(node, scope);
        }
    }

    /// A class field or an interface property signature.
    fn property(&mut self, node: Node<'_>, scope: &Scope) {
        let Some(name) = member_name(node) else {
            return;
        };
        self.push(
            Def {
                kind: SymbolKind::Property,
                name,
                local: self.text(name),
                span: node,
                signature: signature::declaration(node, node, self.source),
                callable: None,
            },
            scope,
        );
    }

    /// The parameter properties of a constructor implementation: parameters
    /// with an accessibility modifier, `readonly`, or `override`. Each is a
    /// `property` of the class spanning its parameter.
    fn parameter_properties(&mut self, constructor: Node<'_>, scope: &Scope) {
        let Some(parameters) = constructor.child_by_field_name("parameters") else {
            return;
        };
        for parameter in named_children(parameters) {
            if !matches!(
                parameter.kind(),
                "required_parameter" | "optional_parameter"
            ) {
                continue;
            }
            let mut cursor = parameter.walk();
            let is_property = parameter.children(&mut cursor).any(|child| {
                matches!(child.kind(), "accessibility_modifier" | "override_modifier")
                    || (!child.is_named() && child.kind() == "readonly")
            });
            let Some(name) = parameter
                .child_by_field_name("pattern")
                .filter(|pattern| pattern.kind() == "identifier")
            else {
                continue;
            };
            if !is_property {
                continue;
            }
            self.push(
                Def {
                    kind: SymbolKind::Property,
                    name,
                    local: self.text(name),
                    span: parameter,
                    signature: signature::declaration(parameter, parameter, self.source),
                    callable: None,
                },
                scope,
            );
        }
    }

    /// An interface and its property and method signatures. Call, construct,
    /// and index signatures have no name and are not symbols.
    fn interface(&mut self, node: Node<'_>, outer: Node<'_>, scope: &Scope) {
        let Some(name) = identifier_name(node) else {
            return;
        };
        let def = Def {
            kind: SymbolKind::Interface,
            name,
            local: self.text(name),
            span: outer,
            signature: signature::declaration(outer, node, self.source),
            callable: None,
        };
        let Some(index) = self.push(def, scope) else {
            return;
        };
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let members = self.member_scope(index, body);
        for member in named_children(body) {
            match member.kind() {
                "property_signature" => self.property(member, &members),
                "method_signature" => self.method(member, &members),
                _ => {}
            }
        }
    }

    /// An enum and its members, each a `const` spanning the member.
    fn enumeration(&mut self, node: Node<'_>, outer: Node<'_>, scope: &Scope) {
        let Some(name) = identifier_name(node) else {
            return;
        };
        let def = Def {
            kind: SymbolKind::Enum,
            name,
            local: self.text(name),
            span: outer,
            signature: signature::declaration(outer, node, self.source),
            callable: None,
        };
        let Some(index) = self.push(def, scope) else {
            return;
        };
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let members = self.member_scope(index, body);
        for member in named_children(body) {
            // A bare member is its name node; an initialized one is an
            // `enum_assignment` with a `name` field.
            let name = match member.kind() {
                "property_identifier" => member,
                "enum_assignment" => match member
                    .child_by_field_name("name")
                    .filter(|name| name.kind() == "property_identifier")
                {
                    Some(name) => name,
                    None => continue,
                },
                _ => continue,
            };
            self.push(
                Def {
                    kind: SymbolKind::Const,
                    name,
                    local: self.text(name),
                    span: member,
                    signature: signature::declaration(member, member, self.source),
                    callable: None,
                },
                &members,
            );
        }
    }

    /// A `namespace` or `module` block. An identifier name is one segment; a
    /// dotted name (`namespace A.B.C`) is one `module` symbol whose short name
    /// is the last segment and whose qualified name has every segment (no
    /// symbols for `A` or `A.B`). A string name (`declare module "x"`) is a
    /// module specifier, not an identifier: neither the module nor anything
    /// in it is a symbol.
    fn module(&mut self, node: Node<'_>, outer: Node<'_>, scope: &Scope) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let Some(segments) = dotted_segments(name) else {
            return;
        };
        let Some(&last) = segments.last() else {
            return;
        };
        let local = segments
            .iter()
            .map(|segment| self.text(*segment))
            .collect::<Vec<_>>()
            .join(".");
        let def = Def {
            kind: SymbolKind::Module,
            name: last,
            local,
            span: outer,
            signature: signature::declaration(outer, node, self.source),
            callable: None,
        };
        if let Some(index) = self.push(def, scope)
            && let Some(body) = node.child_by_field_name("body")
        {
            self.statements(body, &self.member_scope(index, body));
        }
    }

    /// The scope of the members of the container at `index`, whose body is
    /// `body`.
    fn member_scope(&self, index: usize, body: Node<'_>) -> Scope {
        Scope {
            prefix: Some(self.records[index].qualified_name.clone()),
            parent: Some(index),
            body: body.id(),
        }
    }

    /// Records a definition and returns its index, or `None` for an empty
    /// span (impossible in a tree that passed the parse policy).
    fn push(&mut self, def: Def<'_>, scope: &Scope) -> Option<usize> {
        let span = node_span(def.span)?;
        let name_span = node_span(def.name)?;
        let qualified_name = match &scope.prefix {
            Some(prefix) => format!("{prefix}.{}", def.local),
            None => def.local,
        };
        self.records.push(Record {
            kind: def.kind,
            name: self.text(def.name),
            qualified_name,
            span,
            name_span,
            parent: scope.parent,
            body: scope.body,
            callable: def.callable,
            signature: def.signature,
            doc_comment: signature::doc_comment(def.span, self.source),
        });
        Some(self.records.len() - 1)
    }

    fn text(&self, node: Node<'_>) -> String {
        String::from_utf8_lossy(&self.source[node.start_byte()..node.end_byte()]).into_owned()
    }
}

/// Which records survive overload folding (decision 5).
///
/// Functions and methods in the same body with the same qualified name,
/// accessor, and static-ness are one overload set. When a set has an
/// implementation (a declaration with a body), its bodyless signatures are
/// not symbols, so the implementation keeps its own span and signature. When
/// it has none (a `.d.ts` file, an ambient or abstract declaration, interface
/// method overloads), only its first signature in source order is a symbol.
/// Nothing records how many signatures folded, so IDs stay stable when one is
/// added or removed. A getter and a setter differ in accessor and are never
/// one set; nor are a function and a same-named `const` arrow function.
fn fold_overloads(records: &[Record]) -> Vec<bool> {
    type Key<'r> = (usize, &'r str, SymbolKind, Accessor, bool);
    let mut sets: BTreeMap<Key<'_>, Vec<usize>> = BTreeMap::new();
    for (index, record) in records.iter().enumerate() {
        if let Some(callable) = record.callable {
            sets.entry((
                record.body,
                record.qualified_name.as_str(),
                record.kind,
                callable.accessor,
                callable.is_static,
            ))
            .or_default()
            .push(index);
        }
    }
    let implementation = |index: usize| {
        records[index]
            .callable
            .is_some_and(|callable| callable.implementation)
    };
    let mut keep = vec![true; records.len()];
    for members in sets.values() {
        if members.iter().any(|&index| implementation(index)) {
            for &index in members {
                keep[index] = implementation(index);
            }
        } else {
            // `members` is in walk order, which is source order.
            for &index in &members[1..] {
                keep[index] = false;
            }
        }
    }
    keep
}

/// Orders the kept records by `(start_byte, end_byte)` and freezes them, with
/// parent indices remapped into the result.
///
/// The walk is pre-order, so this sort is stable and only makes the order
/// explicit; members of one multi-name `const` statement share a span and keep
/// their source order. A folded signature has no members, so every kept
/// record's parent is kept too.
fn freeze(records: Vec<Record>, keep: &[bool]) -> Vec<ExtractedSymbol> {
    let mut order: Vec<usize> = (0..records.len()).filter(|&index| keep[index]).collect();
    order.sort_by_key(|&index| {
        (
            records[index].span.start_byte(),
            records[index].span.end_byte(),
        )
    });
    let mut position: Vec<Option<usize>> = vec![None; records.len()];
    for (new, &old) in order.iter().enumerate() {
        position[old] = Some(new);
    }
    let mut records: Vec<Option<Record>> = records.into_iter().map(Some).collect();
    order
        .iter()
        .map(|&old| {
            let record = records[old].take().expect("each record is frozen once");
            ExtractedSymbol {
                qualified_name: record.qualified_name,
                name: record.name,
                kind: record.kind,
                span: record.span,
                name_span: Some(record.name_span),
                parent_index: record.parent.and_then(|parent| position[parent]),
                signature: Some(record.signature),
                doc_comment: record.doc_comment,
            }
        })
        .collect()
}

/// The `name` field of a declaration when it is an identifier.
fn identifier_name(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("name")
        .filter(|name| matches!(name.kind(), "identifier" | "type_identifier"))
}

/// The `name` field of a class or interface member when it is an identifier
/// or an ES private name; string, number, and computed names are skipped.
fn member_name(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("name").filter(|name| {
        matches!(
            name.kind(),
            "property_identifier" | "private_property_identifier"
        )
    })
}

/// The identifier segments of a namespace name, or `None` for a string name.
fn dotted_segments(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    match node.kind() {
        "identifier" | "property_identifier" => Some(vec![node]),
        "nested_identifier" | "member_expression" => {
            let mut segments = dotted_segments(node.child_by_field_name("object")?)?;
            segments.push(node.child_by_field_name("property")?);
            Some(segments)
        }
        _ => None,
    }
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn node_span(node: Node<'_>) -> Option<Span> {
    Span::new(node.start_byte() as u32, node.end_byte() as u32).ok()
}
