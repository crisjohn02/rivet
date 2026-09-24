//! The TypeScript use, scope, and import walk (T43).
//!
//! A documented scope walk rather than a `.scm` query, because shadowing and
//! receiver hints depend on which declarations each lexical scope holds
//! (docs/ADDING-A-LANGUAGE.md "Implementation checklist"). One walk serves
//! both grammar variants. It records:
//!
//! - **Uses.** Every identifier written in code, classified by its syntactic
//!   position:
//!   - [`RefKind::Call`]: a callee (`f()`, the member of `a.f()`), a
//!     capitalized JSX component name (`<Button />`, the member of
//!     `<ui.Button />`), and a decorator (`@Injectable()` and `@observable`
//!     alike, since a decorator is applied by being called);
//!   - [`RefKind::Type`]: a name in a type position (annotations, generic
//!     arguments, heritage clauses, `as`/`satisfies`, `typeof x`), a `new`
//!     target, and the class operand of `instanceof`, as in PHP;
//!   - [`RefKind::Import`]: the local binding an import creates (the alias
//!     when there is one), and the re-exported name of a re-export specifier;
//!   - [`RefKind::Read`] / [`RefKind::Write`]: a member read or written
//!     through a receiver (`a.b`, `a.b = 1`, `a.b += 1`, `a.b++`);
//!   - [`RefKind::Write`]: a bare name assigned or updated (`x = 1`, `x++`,
//!     destructuring assignment);
//!   - [`RefKind::Unknown`]: any other identifier in code, such as a name
//!     before `.`, an argument, or a shorthand property.
//!
//!   A member use records its receiver's source text (without `?.`). Comments,
//!   string literals, template-literal text, JSX text, attribute names, string
//!   attribute values, lowercase intrinsic element names, and closing tag
//!   names are never uses; template interpolations and JSX expression
//!   containers are walked as code. Declaration names (functions, classes,
//!   variables, parameters, members, type parameters, enum members, object
//!   keys) are never uses.
//! - **Containers.** A use's container is the innermost function, method,
//!   class, interface, or enum symbol whose span contains it, as for PHP.
//!   Anonymous functions and classes are not symbols, so they are transparent:
//!   a use in a callback inside a named function belongs to that function, and
//!   one in a top-level IIFE or an anonymous default export has none.
//! - **Scopes.** The module (`top:file`), each namespace and ambient module
//!   body, each function, method, arrow function, and class static block, and
//!   each block, loop, `catch`, `switch`, class, interface, type alias,
//!   signature, mapped type, or conditional type that binds a name. Each
//!   records the names it binds as [`LocalBinding`]s: `let`, `const`, class,
//!   enum, interface, and type alias declarations in their block; `var` and
//!   function declarations in the nearest function, namespace, or module
//!   scope (a block-level function declaration is placed there too, which
//!   can only over-report shadowing); parameters, type parameters, and a named
//!   function or class expression's own name in its scope. A name binds for
//!   its whole scope, wherever it is declared. The symbols among a scope's
//!   locals are its `declares`.
//! - **Imports.** Named, default, namespace, `require`, and type-only imports,
//!   re-export specifiers, and `export *`, as [`ModuleImport`]s of the scope
//!   they are written in, with the specifier as written. `require(...)` in an
//!   expression is an ordinary call of `require`.
//! - **Receiver hints**: [`UseHint::This`] for `this.m()` inside a named
//!   class (not inside a nested `function` or object-literal method, and not
//!   in an anonymous or function-local class), with whether that `this` is
//!   in a static context (T45); [`UseHint::Typed`] for a receiver that is a
//!   parameter or variable with one explicit named type, or `this.f` where
//!   `f` is a field or parameter property with one; and
//!   [`UseHint::NewExpr`] for a `const` initialized by `new C(...)`. The
//!   typed and `new` hints carry the span of their type name's `type` use
//!   (T45), so the resolver looks the class up where it is written. A union,
//!   array, or other structural type records no hint, and neither does a `let`
//!   or `var` bound by `new`, which can be reassigned.
//! - **Member sides** (T45): whether each class and interface member symbol
//!   is static, as [`MemberSide`]s of the module scope. A constructor is on
//!   neither side and is not recorded.
//! - **Exports** (T44): each local `export` statement of the module scope (or
//!   of a string-named ambient module body) as [`ModuleExport`]s: the names a
//!   declaration exports, `export default` of a named declaration or of an
//!   identifier, and `export { a as b }` specifiers; `export default
//!   <expression>` and anonymous defaults are recorded with no local name.
//!   Re-exports stay [`ModuleImport`]s.
//! - **Value-position `type` uses** (T44): the span of each `type` use that
//!   names a value (a `new` target, an `instanceof` operand, a `typeof`
//!   operand, a class `extends` expression), in its scope's
//!   `value_type_uses`.
//! - **Global declarations** (T44): a script file (no top-level `import` or
//!   `export`) and a `declare global` block declare globals, which can merge
//!   with declarations in other files. Their names are locals, but no scope's
//!   `declares` lists their symbols, so no same-file binding claims one.
//! - **Ambient module bodies** (T44): the body of `declare module "x" {}` is
//!   marked `ambient_module`, because it also sees the exports of the module
//!   it declares or augments.
//!
//! Nothing here resolves anything: every use leaves the adapter unbound.

use std::collections::HashMap;
use std::rc::Rc;

use rivet_core::extract::{
    BindingSpace, ExtractedScope, ExtractedUse, LocalBinding, MemberSide, ModuleExport,
    ModuleImport, ModuleImportKind, ScopeFacts, TypedOrigin, UseHint,
};
use rivet_core::{ExtractedSymbol, RefKind, Span, SymbolKind};
use tree_sitter::Node;

/// The key of the module scope. Every other scope key is
/// `{container}:{ordinal}`: the index of the innermost container symbol where
/// the scope opens (or `top`), and the scope's one-based pre-order ordinal.
pub const MODULE_SCOPE_KEY: &str = "top:file";

/// Extracts the uses and lexical scopes of one parsed, error-free file.
///
/// `symbols` must be the file's already-extracted symbols: uses record the
/// index of their innermost container and scopes record declarations by
/// symbol index. Uses are in `(start_byte, end_byte)` order and scopes in
/// scope-key order. Returns `None` when the file yields more than `max_uses`
/// uses (spec §27); the walk stops at the first use over the bound.
pub(super) fn extract_uses(
    source: &[u8],
    root: Node<'_>,
    symbols: &[ExtractedSymbol],
    max_uses: u64,
) -> Option<(Vec<ExtractedUse>, Vec<ExtractedScope>)> {
    let mut walker = Walker::new(source, symbols, max_uses);
    walker.module(root);
    if walker.exceeded {
        return None;
    }
    Some(walker.finish())
}

/// One explicit named type or `new` target as written, with the span of the
/// `type` use its name records (T45): the resolver looks the class up through
/// that use, in the scope where the annotation or `new` is written.
#[derive(Debug, Clone)]
struct NamedType {
    /// The name as written: `Foo`, `ns.Foo` (a generic's `Foo<T>` as `Foo`).
    spelling: String,
    /// The span of the last identifier of the name, where its `type` use is.
    span: Option<Span>,
}

/// What a local binding says about the value it holds, for receiver hints.
#[derive(Debug, Clone)]
enum Detail {
    /// A parameter, with its one explicit named type.
    Parameter(Option<NamedType>),
    /// A variable, with its one explicit named type annotation and, for a
    /// `const` initialized by `new C(...)`, the class as written.
    Variable {
        annotation: Option<NamedType>,
        new_class: Option<NamedType>,
    },
    /// Anything else: nothing a hint can use.
    Other,
}

/// One local binding while its scope is being built.
struct Local {
    binding: LocalBinding,
    detail: Detail,
}

/// One scope while the walk builds it.
struct ScopeBuild {
    key: String,
    parent: Option<usize>,
    /// Whether `var` and function declarations land here: the module, a
    /// namespace or ambient module body, a function, or a static block.
    var_scope: bool,
    /// Whether this is a module's own scope, whose `export` statements are
    /// the module's exports (T44): the file's module scope, or the body of a
    /// string-named ambient module. A namespace body is not: its `export`
    /// exports a namespace member.
    module: bool,
    locals: Vec<Local>,
    declares: Vec<usize>,
    imports: Vec<ModuleImport>,
    exports: Vec<ModuleExport>,
    /// The spans of `type` uses in this scope that name values (T44).
    value_type_uses: Vec<Span>,
}

/// The single named types of one named class's fields.
#[derive(Default)]
struct Fields {
    /// Instance fields and constructor parameter properties.
    instance: HashMap<String, Option<NamedType>>,
    /// Static fields.
    statics: HashMap<String, Option<NamedType>>,
}

impl Fields {
    /// Records `name: spelling`; a name declared twice keeps no type.
    fn insert(
        map: &mut HashMap<String, Option<NamedType>>,
        name: String,
        spelling: Option<NamedType>,
    ) {
        map.entry(name)
            .and_modify(|existing| *existing = None)
            .or_insert(spelling);
    }
}

/// What `this` names at a position.
#[derive(Clone)]
enum This {
    /// An instance of a named class, or its constructor when `is_static`.
    Class { fields: Rc<Fields>, is_static: bool },
    /// Something no hint describes: the module, a `function`, an object
    /// literal, or an anonymous or function-local class.
    Opaque,
}

/// A receiver hint, possibly waiting for the whole scope chain.
enum Hint {
    Now(UseHint),
    /// The receiver is this bare name; its hint is decided after the walk,
    /// once every scope holds all of its declarations.
    Lookup(String),
}

struct Walker<'s> {
    source: &'s [u8],
    symbols: &'s [ExtractedSymbol],
    /// Container-kind symbols by span; the lowest index for a shared span.
    containers: HashMap<(u32, u32), usize>,
    /// Every symbol by its name span.
    by_name_span: HashMap<(u32, u32), usize>,
    max_uses: u64,
    exceeded: bool,
    uses: Vec<ExtractedUse>,
    /// Uses whose receiver hint is a bare-name lookup: (use, name, scope).
    pending: Vec<(usize, String, usize)>,
    scopes: Vec<ScopeBuild>,
    scope_stack: Vec<usize>,
    container_stack: Vec<usize>,
    this_stack: Vec<This>,
    /// For each class body being walked, its fields when it is a named class.
    class_stack: Vec<Option<Rc<Fields>>>,
    next_ordinal: u32,
    /// Whether the file is a script: it has no top-level `import` or
    /// `export`, so its top-level declarations are global (T44).
    script: bool,
    /// The side of each named class and interface member symbol (T45).
    member_sides: Vec<MemberSide>,
    /// How many `declare global` blocks enclose the current position (T44).
    global_depth: u32,
}

impl<'s> Walker<'s> {
    fn new(source: &'s [u8], symbols: &'s [ExtractedSymbol], max_uses: u64) -> Walker<'s> {
        let mut containers = HashMap::new();
        let mut by_name_span = HashMap::new();
        for (index, symbol) in symbols.iter().enumerate() {
            if is_container(symbol.kind) {
                containers
                    .entry((symbol.span.start_byte(), symbol.span.end_byte()))
                    .or_insert(index);
            }
            if let Some(name) = symbol.name_span {
                by_name_span
                    .entry((name.start_byte(), name.end_byte()))
                    .or_insert(index);
            }
        }
        Walker {
            source,
            symbols,
            containers,
            by_name_span,
            max_uses,
            exceeded: false,
            uses: Vec::new(),
            pending: Vec::new(),
            scopes: Vec::new(),
            scope_stack: Vec::new(),
            container_stack: Vec::new(),
            this_stack: vec![This::Opaque],
            class_stack: Vec::new(),
            next_ordinal: 0,
            script: false,
            member_sides: Vec::new(),
            global_depth: 0,
        }
    }

    // -----------------------------------------------------------------
    // Scopes, containers, and records.
    // -----------------------------------------------------------------

    /// Walks the whole file in the module scope.
    fn module(&mut self, root: Node<'_>) {
        // A file with no top-level `import` or `export` statement is a script
        // (a side-effect `import "./x"` counts: it makes the file a module).
        self.script = !named_children(root)
            .iter()
            .any(|child| matches!(child.kind(), "import_statement" | "export_statement"));
        self.scopes.push(ScopeBuild {
            key: MODULE_SCOPE_KEY.to_string(),
            parent: None,
            var_scope: true,
            module: true,
            locals: Vec::new(),
            declares: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            value_type_uses: Vec::new(),
        });
        self.scope_stack.push(0);
        self.statements(root);
        self.scope_stack.pop();
    }

    /// Opens a scope nested in the current one and returns its index.
    fn open_scope(&mut self, var_scope: bool) -> usize {
        self.next_ordinal += 1;
        let container = match self.container_stack.last() {
            Some(index) => index.to_string(),
            None => "top".to_string(),
        };
        self.scopes.push(ScopeBuild {
            key: format!("{container}:{}", self.next_ordinal),
            parent: self.scope_stack.last().copied(),
            var_scope,
            module: false,
            locals: Vec::new(),
            declares: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            value_type_uses: Vec::new(),
        });
        let index = self.scopes.len() - 1;
        self.scope_stack.push(index);
        index
    }

    fn close_scope(&mut self) {
        self.scope_stack.pop();
    }

    fn current_scope(&self) -> usize {
        *self.scope_stack.last().expect("the module scope is open")
    }

    /// The nearest scope where `var` and function declarations land.
    fn var_scope(&self) -> usize {
        self.scope_stack
            .iter()
            .rev()
            .copied()
            .find(|&index| self.scopes[index].var_scope)
            .unwrap_or(0)
    }

    /// Records that `scope` binds the name `name`.
    fn declare(&mut self, scope: usize, name: Node<'_>, space: BindingSpace, detail: Detail) {
        let Some(span) = span_of(name) else {
            return;
        };
        // A class or interface member is never lexically bound, even when a
        // local shares its declaration: a constructor parameter property is a
        // parameter inside the constructor, not a reference to the property.
        // A global declaration (T44) is not module-local, so it is in no
        // scope's `declares` either; its name is still a local below.
        if !self.script
            && self.global_depth == 0
            && let Some(&symbol) = self.by_name_span.get(&(span.start_byte(), span.end_byte()))
            && self.symbols[symbol].parent_index.is_none_or(|parent| {
                matches!(
                    self.symbols[parent].kind,
                    SymbolKind::Module | SymbolKind::Enum
                )
            })
        {
            self.scopes[scope].declares.push(symbol);
        }
        let name = match name.kind() {
            // A string enum member keeps its unquoted text.
            "string" => string_content(name, self.source),
            _ => self.text(name),
        };
        self.scopes[scope].locals.push(Local {
            binding: LocalBinding { name, space, span },
            detail,
        });
    }

    /// Records one use and returns its index, or `None` once the use bound
    /// is exceeded (or for an empty span).
    fn push_use(
        &mut self,
        node: Node<'_>,
        ref_kind: RefKind,
        receiver: Option<String>,
        hint: UseHint,
    ) -> Option<usize> {
        if self.exceeded {
            return None;
        }
        let span = span_of(node)?;
        if self.uses.len() as u64 >= self.max_uses {
            self.exceeded = true;
            return None;
        }
        let scope = self.current_scope();
        self.uses.push(ExtractedUse {
            spelling: self.text(node),
            ref_kind,
            span,
            containing_symbol_index: self.container_stack.last().copied(),
            receiver,
            scope_key: self.scopes[scope].key.clone(),
            hint,
        });
        Some(self.uses.len() - 1)
    }

    /// Records a use with no receiver.
    fn bare(&mut self, node: Node<'_>, ref_kind: RefKind) {
        self.push_use(node, ref_kind, None, UseHint::Unresolved);
    }

    /// Records a member use of `property` through `object`, with the
    /// receiver's text and hint.
    fn member_use(&mut self, property: Node<'_>, object: Node<'_>, ref_kind: RefKind) {
        let receiver = self.text(object);
        match self.receiver_hint(object) {
            Hint::Now(hint) => {
                self.push_use(property, ref_kind, Some(receiver), hint);
            }
            Hint::Lookup(name) => {
                let scope = self.current_scope();
                if let Some(index) =
                    self.push_use(property, ref_kind, Some(receiver), UseHint::Unresolved)
                {
                    self.pending.push((index, name, scope));
                }
            }
        }
    }

    /// The receiver hint `object` gives a member use.
    fn receiver_hint(&self, object: Node<'_>) -> Hint {
        match object.kind() {
            "this" => Hint::Now(match self.this_stack.last() {
                Some(This::Class { is_static, .. }) => UseHint::This {
                    is_static: Some(*is_static),
                },
                _ => UseHint::Unresolved,
            }),
            "member_expression" => {
                let inner = object.child_by_field_name("object");
                let property = object.child_by_field_name("property");
                if let (Some(inner), Some(property), Some(This::Class { fields, is_static })) =
                    (inner, property, self.this_stack.last())
                    && inner.kind() == "this"
                {
                    let map = if *is_static {
                        &fields.statics
                    } else {
                        &fields.instance
                    };
                    if let Some(Some(ty)) = map.get(&self.text(property)) {
                        return Hint::Now(UseHint::Typed {
                            type_spelling: ty.spelling.clone(),
                            origin: TypedOrigin::Property,
                            name_span: ty.span,
                        });
                    }
                }
                Hint::Now(UseHint::Unresolved)
            }
            "identifier" => Hint::Lookup(self.text(object)),
            _ => Hint::Now(UseHint::Unresolved),
        }
    }

    /// Enters `node` as a container when a container symbol spans it.
    fn enter_container(&mut self, node: Node<'_>) -> bool {
        let key = (node.start_byte() as u32, node.end_byte() as u32);
        match self.containers.get(&key) {
            Some(&index) if self.container_stack.last() != Some(&index) => {
                self.container_stack.push(index);
                true
            }
            _ => false,
        }
    }

    // -----------------------------------------------------------------
    // The generic visit.
    // -----------------------------------------------------------------

    /// Visits one node: classifies it, or walks its children.
    fn visit(&mut self, node: Node<'_>) {
        if self.exceeded {
            return;
        }
        let entered = self.enter_container(node);
        self.dispatch(node);
        if entered {
            self.container_stack.pop();
        }
    }

    fn dispatch(&mut self, node: Node<'_>) {
        match node.kind() {
            // Literal text, comments, keywords, and names that are never uses
            // on their own: a property name is a use only as the member of a
            // member expression, handled there.
            "comment"
            | "string"
            | "number"
            | "regex"
            | "jsx_text"
            | "html_comment"
            | "hash_bang_line"
            | "this"
            | "super"
            | "true"
            | "false"
            | "null"
            | "undefined"
            | "predefined_type"
            | "literal_type"
            | "property_identifier"
            | "private_property_identifier"
            | "shorthand_property_identifier_pattern"
            | "statement_identifier"
            | "jsx_closing_element"
            | "jsx_namespace_name"
            | "meta_property"
            | "import"
            | "this_type"
            | "break_statement"
            | "continue_statement"
            | "empty_statement"
            | "debugger_statement" => {}
            "identifier" | "shorthand_property_identifier" => self.bare(node, RefKind::Unknown),
            "type_identifier" => self.bare(node, RefKind::Type),
            "template_string" | "template_literal_type" => {
                for child in named_children(node) {
                    if matches!(child.kind(), "template_substitution" | "template_type") {
                        self.visit(child);
                    }
                }
            }
            "nested_type_identifier" => self.dotted(node, RefKind::Type),
            "nested_identifier" => self.dotted(node, RefKind::Unknown),
            "member_expression" => self.member(node, RefKind::Read),
            "call_expression" => self.call(node),
            "new_expression" => self.new_expression(node),
            "assignment_expression" | "augmented_assignment_expression" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.target(left);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.visit(right);
                }
            }
            "update_expression" => {
                if let Some(argument) = node.child_by_field_name("argument") {
                    self.target(argument);
                }
            }
            "binary_expression" => self.binary(node),
            "type_query" => {
                for child in named_children(node) {
                    self.typed_operand(child);
                }
            }
            "decorator" => {
                for child in named_children(node) {
                    self.callee(child);
                }
            }
            "lexical_declaration" | "variable_declaration" => self.variables(node),
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                self.function_declaration(node)
            }
            "function_expression" | "generator_function" => {
                let name = node.child_by_field_name("name");
                self.function_body(node, Some(This::Opaque), name);
            }
            "arrow_function" => self.function_body(node, None, None),
            "method_definition" | "method_signature" | "abstract_method_signature" => {
                self.method(node)
            }
            "call_signature" | "construct_signature" | "function_type" | "constructor_type" => {
                self.signature(node)
            }
            "class_declaration" | "abstract_class_declaration" | "class" => self.class(node),
            "public_field_definition" => self.field(node),
            "class_static_block" => self.static_block(node),
            "interface_declaration" => self.interface(node),
            "type_alias_declaration" => self.type_alias(node),
            "enum_declaration" => self.enumeration(node),
            "internal_module" | "module" => self.namespace(node),
            "ambient_declaration" => self.ambient(node),
            "import_statement" => self.import(node),
            "export_statement" => self.export(node),
            "import_alias" => self.import_alias(node),
            "statement_block" => self.block(node),
            "for_statement" => self.for_statement(node),
            "for_in_statement" => self.for_in(node),
            "catch_clause" => self.catch_clause(node),
            "switch_body" => self.switch_body(node),
            // A parameter outside a function handler (none is expected):
            // walk its type, default, and decorators, declaring nothing.
            "required_parameter" | "optional_parameter" => self.parameter(node, None),
            "type_parameter" => self.type_parameter_parts(node),
            "index_signature" => self.index_signature(node),
            "infer_type" => self.infer(node),
            "conditional_type" => self.conditional_type(node),
            "type_predicate" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    self.visit(ty);
                }
            }
            // `asserts x` names a parameter; only a predicate's type is code.
            "asserts" => {
                for child in named_children(node) {
                    if child.kind() == "type_predicate" {
                        self.visit(child);
                    }
                }
            }
            "jsx_opening_element" | "jsx_self_closing_element" => self.jsx_element(node),
            // A destructuring pattern outside a declaration or an assignment
            // (none is expected): walk defaults and computed keys only.
            "object_pattern"
            | "array_pattern"
            | "pair_pattern"
            | "rest_pattern"
            | "assignment_pattern"
            | "object_assignment_pattern" => {
                self.declare_pattern(node, None, BindingSpace::Value, Detail::Other)
            }
            _ => self.children(node),
        }
    }

    /// Visits every named child.
    fn children(&mut self, node: Node<'_>) {
        for child in named_children(node) {
            self.visit(child);
        }
    }

    /// Visits the statements of a block, program, or body in the current
    /// scope.
    fn statements(&mut self, node: Node<'_>) {
        self.children(node);
    }

    // -----------------------------------------------------------------
    // Expressions.
    // -----------------------------------------------------------------

    /// A member expression used as `ref_kind`: the object is walked as code
    /// (a name before `.` is `unknown`), and the member is the use.
    fn member(&mut self, node: Node<'_>, ref_kind: RefKind) {
        let object = node.child_by_field_name("object");
        if let Some(object) = object {
            self.visit(object);
        }
        let property = node.child_by_field_name("property");
        if let (Some(object), Some(property)) = (object, property)
            && matches!(
                property.kind(),
                "property_identifier" | "private_property_identifier"
            )
        {
            self.member_use(property, object, ref_kind);
        }
    }

    /// A dotted name (`ns.Type`, `A.B`): every segment before the last is an
    /// `unknown` name before `.`; the last is `last_kind` with the rest as its
    /// receiver.
    fn dotted(&mut self, node: Node<'_>, last_kind: RefKind) {
        let (object, last) = if node.kind() == "nested_type_identifier" {
            (
                node.child_by_field_name("module"),
                node.child_by_field_name("name"),
            )
        } else {
            (
                node.child_by_field_name("object"),
                node.child_by_field_name("property"),
            )
        };
        if let Some(object) = object {
            match object.kind() {
                "nested_identifier" | "nested_type_identifier" => {
                    self.dotted(object, RefKind::Unknown)
                }
                "identifier" => self.bare(object, RefKind::Unknown),
                _ => self.visit(object),
            }
        }
        if let (Some(object), Some(last)) = (object, last) {
            let receiver = self.text(object);
            self.push_use(last, last_kind, Some(receiver), UseHint::Unresolved);
        }
    }

    /// A callee position: a name is a `call`, a member expression's member is
    /// a `call`, anything else is walked as code.
    fn callee(&mut self, node: Node<'_>) {
        match node.kind() {
            "identifier" => self.bare(node, RefKind::Call),
            "member_expression" => self.member(node, RefKind::Call),
            _ => self.visit(node),
        }
    }

    /// A position that names a class or value as a type (`new C`,
    /// `instanceof C`, `extends C`, `typeof x`). The use is a `type` use, as
    /// in PHP, and its span is a value-position type use of the current scope
    /// (T44): the name is looked up among values.
    fn typed_operand(&mut self, node: Node<'_>) {
        match node.kind() {
            "identifier" => {
                self.bare(node, RefKind::Type);
                self.value_type_use(node);
            }
            "member_expression" => {
                self.member(node, RefKind::Type);
                if let Some(property) = node.child_by_field_name("property")
                    && matches!(
                        property.kind(),
                        "property_identifier" | "private_property_identifier"
                    )
                {
                    self.value_type_use(property);
                }
            }
            _ => self.visit(node),
        }
    }

    /// Records `name`, a `type` use just recorded in the current scope, as
    /// one that names a value (T44).
    fn value_type_use(&mut self, name: Node<'_>) {
        if let Some(span) = span_of(name) {
            let scope = self.current_scope();
            self.scopes[scope].value_type_uses.push(span);
        }
    }

    fn call(&mut self, node: Node<'_>) {
        let function = node.child_by_field_name("function");
        if let Some(function) = function {
            self.callee(function);
        }
        for child in named_children(node) {
            if Some(child) != function {
                self.visit(child);
            }
        }
    }

    fn new_expression(&mut self, node: Node<'_>) {
        let constructor = node.child_by_field_name("constructor");
        if let Some(constructor) = constructor {
            self.typed_operand(constructor);
        }
        for child in named_children(node) {
            if Some(child) != constructor {
                self.visit(child);
            }
        }
    }

    /// `x instanceof C` records `C` as a type use, as PHP does.
    fn binary(&mut self, node: Node<'_>) {
        let instanceof = node
            .child_by_field_name("operator")
            .is_some_and(|operator| operator.kind() == "instanceof");
        let right = node.child_by_field_name("right");
        for child in named_children(node) {
            if instanceof && Some(child) == right {
                self.typed_operand(child);
            } else {
                self.visit(child);
            }
        }
    }

    /// An assignment target: a bare name or a member is a `write`; a
    /// destructuring pattern writes each name it binds.
    fn target(&mut self, node: Node<'_>) {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                self.bare(node, RefKind::Write)
            }
            "member_expression" => self.member(node, RefKind::Write),
            "parenthesized_expression"
            | "non_null_expression"
            | "object_pattern"
            | "array_pattern"
            | "rest_pattern" => {
                for child in named_children(node) {
                    self.target(child);
                }
            }
            "pair_pattern" => {
                if let Some(key) = node.child_by_field_name("key")
                    && key.kind() == "computed_property_name"
                {
                    self.visit(key);
                }
                if let Some(value) = node.child_by_field_name("value") {
                    self.target(value);
                }
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.target(left);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.visit(right);
                }
            }
            _ => self.visit(node),
        }
    }

    // -----------------------------------------------------------------
    // Declarations and scopes.
    // -----------------------------------------------------------------

    /// Declares every name a binding pattern binds in `scope` (or, with no
    /// scope, declares nothing), walking defaults and computed keys as code.
    /// `detail` applies to a pattern that is one plain name.
    fn declare_pattern(
        &mut self,
        node: Node<'_>,
        scope: Option<usize>,
        space: BindingSpace,
        detail: Detail,
    ) {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                if let Some(scope) = scope {
                    self.declare(scope, node, space, detail);
                }
            }
            "object_pattern" | "array_pattern" | "rest_pattern" => {
                for child in named_children(node) {
                    self.declare_pattern(child, scope, space, Detail::Other);
                }
            }
            "pair_pattern" => {
                if let Some(key) = node.child_by_field_name("key")
                    && key.kind() == "computed_property_name"
                {
                    self.visit(key);
                }
                if let Some(value) = node.child_by_field_name("value") {
                    self.declare_pattern(value, scope, space, Detail::Other);
                }
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.declare_pattern(left, scope, space, Detail::Other);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.visit(right);
                }
            }
            "this" | "comment" => {}
            _ => self.visit(node),
        }
    }

    /// `const`, `let`, and `var` declarations.
    fn variables(&mut self, node: Node<'_>) {
        let keyword = match node.kind() {
            "variable_declaration" => "var".to_string(),
            _ => node
                .child_by_field_name("kind")
                .map(|kind| self.text(kind))
                .unwrap_or_default(),
        };
        let scope = if keyword == "var" {
            self.var_scope()
        } else {
            self.current_scope()
        };
        for child in named_children(node) {
            if child.kind() != "variable_declarator" {
                self.visit(child);
                continue;
            }
            let ty = child.child_by_field_name("type");
            let value = child.child_by_field_name("value");
            let detail = Detail::Variable {
                annotation: ty.and_then(|ty| named_type(ty, self.source)),
                new_class: if keyword == "const" {
                    value.and_then(|value| new_class(value, self.source))
                } else {
                    None
                },
            };
            if let Some(name) = child.child_by_field_name("name") {
                self.declare_pattern(name, Some(scope), BindingSpace::Value, detail);
            }
            if let Some(ty) = ty {
                self.visit(ty);
            }
            if let Some(value) = value {
                self.visit(value);
            }
        }
    }

    /// A function declaration or overload signature: its name binds in the
    /// nearest function, namespace, or module scope.
    fn function_declaration(&mut self, node: Node<'_>) {
        let name = node.child_by_field_name("name");
        if let Some(name) = name {
            let scope = self.var_scope();
            self.declare(scope, name, BindingSpace::Value, Detail::Other);
        }
        if node.child_by_field_name("body").is_some() {
            self.function_body(node, Some(This::Opaque), None);
        } else {
            self.signature(node);
        }
    }

    /// A function, method, or arrow function with a body: one scope holding
    /// its own name (a named function expression), type parameters,
    /// parameters, and body declarations. `this` is pushed when the function
    /// has its own `this`; an arrow function keeps the enclosing one.
    fn function_body(&mut self, node: Node<'_>, this: Option<This>, own_name: Option<Node<'_>>) {
        let scope = self.open_scope(true);
        let pushed = this.is_some();
        if let Some(this) = this {
            self.this_stack.push(this);
        }
        if let Some(name) = own_name {
            self.declare(scope, name, BindingSpace::Value, Detail::Other);
        }
        if let Some(parameters) = node.child_by_field_name("type_parameters") {
            self.type_parameters(parameters, scope);
        }
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.parameters(parameters, Some(scope));
        }
        if let Some(parameter) = node.child_by_field_name("parameter") {
            self.declare_pattern(
                parameter,
                Some(scope),
                BindingSpace::Value,
                Detail::Parameter(None),
            );
        }
        if let Some(return_type) = node.child_by_field_name("return_type") {
            self.visit(return_type);
        }
        if let Some(body) = node.child_by_field_name("body") {
            if body.kind() == "statement_block" {
                self.statements(body);
            } else {
                self.visit(body);
            }
        }
        if pushed {
            self.this_stack.pop();
        }
        self.close_scope();
    }

    /// A bodyless signature (an overload, an interface or abstract method,
    /// a call or construct signature, a function or constructor type): a
    /// scope only for its type parameters; parameter names bind nothing, since
    /// no body can refer to them.
    fn signature(&mut self, node: Node<'_>) {
        let type_parameters = node.child_by_field_name("type_parameters");
        let name = node.child_by_field_name("name");
        if let Some(parameters) = type_parameters {
            let scope = self.open_scope(false);
            self.type_parameters(parameters, scope);
        }
        for child in named_children(node) {
            if Some(child) == type_parameters {
                continue;
            }
            if Some(child) == name {
                if child.kind() == "computed_property_name" {
                    self.visit(child);
                }
                continue;
            }
            match child.kind() {
                "formal_parameters" => self.parameters(child, None),
                _ => self.visit(child),
            }
        }
        if type_parameters.is_some() {
            self.close_scope();
        }
    }

    /// The parameters of a function, declared in `scope` when there is one.
    fn parameters(&mut self, node: Node<'_>, scope: Option<usize>) {
        for child in named_children(node) {
            match child.kind() {
                "required_parameter" | "optional_parameter" => self.parameter(child, scope),
                _ => self.visit(child),
            }
        }
    }

    /// One parameter: its pattern binds in `scope`; its decorators, type, and
    /// default are code.
    fn parameter(&mut self, node: Node<'_>, scope: Option<usize>) {
        let pattern = node.child_by_field_name("pattern");
        let ty = node.child_by_field_name("type");
        for child in named_children(node) {
            if Some(child) == pattern {
                let detail = Detail::Parameter(ty.and_then(|ty| named_type(ty, self.source)));
                self.declare_pattern(child, scope, BindingSpace::Value, detail);
            } else {
                self.visit(child);
            }
        }
    }

    /// Type parameters: each name binds a type in `scope`; constraints and
    /// defaults are code.
    fn type_parameters(&mut self, node: Node<'_>, scope: usize) {
        let parameters = named_children(node);
        for parameter in &parameters {
            if parameter.kind() == "type_parameter"
                && let Some(name) = parameter.child_by_field_name("name")
            {
                self.declare(scope, name, BindingSpace::Type, Detail::Other);
            }
        }
        for parameter in parameters {
            self.visit(parameter);
        }
    }

    /// A type parameter's constraint and default; its name is a declaration.
    fn type_parameter_parts(&mut self, node: Node<'_>) {
        let name = node.child_by_field_name("name");
        for child in named_children(node) {
            if Some(child) != name {
                self.visit(child);
            }
        }
    }

    /// A method or method signature. A class method's `this` is the class
    /// (its constructor when static); an object-literal method's is opaque.
    fn method(&mut self, node: Node<'_>) {
        let name = node.child_by_field_name("name");
        if node.child_by_field_name("body").is_none() {
            self.signature(node);
            return;
        }
        if let Some(name) = name
            && name.kind() == "computed_property_name"
        {
            self.visit(name);
        }
        let in_class = node
            .parent()
            .is_some_and(|parent| parent.kind() == "class_body");
        let this = if in_class {
            self.class_this(has_modifier(node, name, "static"))
        } else {
            This::Opaque
        };
        self.function_body(node, Some(this), None);
    }

    /// What `this` names inside the class body being walked.
    fn class_this(&self, is_static: bool) -> This {
        match self.class_stack.last() {
            Some(Some(fields)) => This::Class {
                fields: Rc::clone(fields),
                is_static,
            },
            _ => This::Opaque,
        }
    }

    /// A class declaration or expression.
    fn class(&mut self, node: Node<'_>) {
        let is_expression = node.kind() == "class";
        let name = node.child_by_field_name("name");
        let type_parameters = node.child_by_field_name("type_parameters");
        let body = node.child_by_field_name("body");
        // Decorators are evaluated outside the class scope.
        for child in named_children(node) {
            if child.kind() == "decorator" {
                self.visit(child);
            }
        }
        if !is_expression && let Some(name) = name {
            let scope = self.current_scope();
            self.declare(scope, name, BindingSpace::Both, Detail::Other);
        }
        let opened = type_parameters.is_some() || (is_expression && name.is_some());
        if opened {
            let scope = self.open_scope(false);
            if is_expression && let Some(name) = name {
                self.declare(scope, name, BindingSpace::Both, Detail::Other);
            }
            if let Some(parameters) = type_parameters {
                self.type_parameters(parameters, scope);
            }
        }
        for child in named_children(node) {
            if child.kind() == "class_heritage" {
                self.heritage(child);
            }
        }
        if let Some(body) = body {
            self.record_member_sides(body);
            let fields = self
                .class_symbol(node)
                .map(|_| Rc::new(collect_fields(body, self.source)));
            self.class_stack.push(fields);
            self.children(body);
            self.class_stack.pop();
        }
        if opened {
            self.close_scope();
        }
    }

    /// Records the side ([`MemberSide`]) of each member symbol a class or
    /// interface body declares (T45): a class member is static when declared
    /// `static`, and every interface member and constructor parameter
    /// property is an instance member. A constructor is on neither side and
    /// is not recorded. Members of an anonymous or function-local class are
    /// not symbols, so nothing is recorded for them.
    fn record_member_sides(&mut self, body: Node<'_>) {
        for member in named_children(body) {
            let Some(name) = member.child_by_field_name("name") else {
                continue;
            };
            if member.kind() == "method_definition" && self.text(name) == "constructor" {
                let parameters = member
                    .child_by_field_name("parameters")
                    .map(named_children)
                    .unwrap_or_default();
                for parameter in parameters {
                    if let Some(pattern) = parameter.child_by_field_name("pattern")
                        && let Some(symbol) = self.member_symbol(pattern)
                    {
                        self.member_sides.push(MemberSide {
                            symbol,
                            is_static: false,
                        });
                    }
                }
                continue;
            }
            let Some(symbol) = self.member_symbol(name) else {
                continue;
            };
            if self.symbols[symbol].kind == SymbolKind::Method
                && self.symbols[symbol].name == "constructor"
            {
                continue;
            }
            let is_static = has_modifier(member, Some(name), "static");
            self.member_sides.push(MemberSide { symbol, is_static });
        }
    }

    /// The method or property symbol of a class or interface whose name is
    /// `name`, if there is one.
    fn member_symbol(&self, name: Node<'_>) -> Option<usize> {
        let symbol = *self
            .by_name_span
            .get(&(name.start_byte() as u32, name.end_byte() as u32))?;
        let parent = self.symbols[symbol].parent_index?;
        (matches!(
            self.symbols[symbol].kind,
            SymbolKind::Method | SymbolKind::Property
        ) && matches!(
            self.symbols[parent].kind,
            SymbolKind::Class | SymbolKind::Interface
        ))
        .then_some(symbol)
    }

    /// The class symbol `node` declares, if it is one: a named class reached
    /// by the definition walk, or a class expression bound to a `const`.
    fn class_symbol(&self, node: Node<'_>) -> Option<usize> {
        let name = match node.kind() {
            "class" => node
                .parent()
                .filter(|parent| parent.kind() == "variable_declarator")?
                .child_by_field_name("name")?,
            _ => node.child_by_field_name("name")?,
        };
        let index = *self
            .by_name_span
            .get(&(name.start_byte() as u32, name.end_byte() as u32))?;
        (self.symbols[index].kind == SymbolKind::Class).then_some(index)
    }

    /// `extends` names a class as a type use; `implements` lists types.
    fn heritage(&mut self, node: Node<'_>) {
        for clause in named_children(node) {
            if clause.kind() == "extends_clause" {
                for child in named_children(clause) {
                    match child.kind() {
                        "type_arguments" => self.visit(child),
                        _ => self.typed_operand(child),
                    }
                }
            } else {
                self.visit(clause);
            }
        }
    }

    /// A class field: its type and initializer are code, with the class's
    /// `this`; its name is a declaration.
    fn field(&mut self, node: Node<'_>) {
        let name = node.child_by_field_name("name");
        let this = self.class_this(has_modifier(node, name, "static"));
        self.this_stack.push(this);
        for child in named_children(node) {
            if Some(child) == name && child.kind() != "computed_property_name" {
                continue;
            }
            self.visit(child);
        }
        self.this_stack.pop();
    }

    /// A class static block: a function-like scope whose `this` is the class.
    fn static_block(&mut self, node: Node<'_>) {
        self.open_scope(true);
        let this = self.class_this(true);
        self.this_stack.push(this);
        if let Some(body) = node.child_by_field_name("body") {
            self.statements(body);
        }
        self.this_stack.pop();
        self.close_scope();
    }

    fn interface(&mut self, node: Node<'_>) {
        let name = node.child_by_field_name("name");
        if let Some(name) = name {
            let scope = self.current_scope();
            self.declare(scope, name, BindingSpace::Type, Detail::Other);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.record_member_sides(body);
        }
        self.type_scoped(node, name);
    }

    fn type_alias(&mut self, node: Node<'_>) {
        let name = node.child_by_field_name("name");
        if let Some(name) = name {
            let scope = self.current_scope();
            self.declare(scope, name, BindingSpace::Type, Detail::Other);
        }
        self.type_scoped(node, name);
    }

    /// Walks an interface or type alias other than its name, in a scope of
    /// its own when it has type parameters.
    fn type_scoped(&mut self, node: Node<'_>, name: Option<Node<'_>>) {
        let type_parameters = node.child_by_field_name("type_parameters");
        if let Some(parameters) = type_parameters {
            let scope = self.open_scope(false);
            self.type_parameters(parameters, scope);
        }
        for child in named_children(node) {
            if Some(child) != name && Some(child) != type_parameters {
                self.visit(child);
            }
        }
        if type_parameters.is_some() {
            self.close_scope();
        }
    }

    /// An enum: its name binds in the enclosing scope; its members bind in a
    /// scope of their own, where their initializers can name them.
    fn enumeration(&mut self, node: Node<'_>) {
        if let Some(name) = node.child_by_field_name("name") {
            let scope = self.current_scope();
            self.declare(scope, name, BindingSpace::Both, Detail::Other);
        }
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let scope = self.open_scope(false);
        for member in named_children(body) {
            match member.kind() {
                "property_identifier" | "string" => {
                    self.declare(scope, member, BindingSpace::Value, Detail::Other)
                }
                "enum_assignment" => {
                    let name = member.child_by_field_name("name");
                    if let Some(name) = name {
                        self.declare(scope, name, BindingSpace::Value, Detail::Other);
                    }
                    for child in named_children(member) {
                        if Some(child) != name {
                            self.visit(child);
                        }
                    }
                }
                _ => self.visit(member),
            }
        }
        self.close_scope();
    }

    /// A `namespace`, `module`, or ambient `declare module`: an identifier
    /// name (the first segment of a dotted one) binds in the enclosing scope;
    /// the body is a scope of its own. A string-named module binds nothing.
    fn namespace(&mut self, node: Node<'_>) {
        if let Some(name) = node.child_by_field_name("name") {
            let first = leftmost_segment(name);
            if let Some(first) = first {
                let scope = self.current_scope();
                self.declare(scope, first, BindingSpace::Both, Detail::Other);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            let scope = self.open_scope(true);
            // A string-named ambient module's body is a module's own scope,
            // whose `export`s are that module's exports (T44).
            self.scopes[scope].module = node
                .child_by_field_name("name")
                .is_some_and(|name| name.kind() == "string");
            self.statements(body);
            self.close_scope();
        }
    }

    /// `declare ...`: `declare global { ... }` declares into the current
    /// scope; any other ambient declaration is walked as its declaration.
    fn ambient(&mut self, node: Node<'_>) {
        let mut cursor = node.walk();
        let global = node
            .children(&mut cursor)
            .any(|child| !child.is_named() && child.kind() == "global");
        if global {
            self.global_depth += 1;
        }
        for child in named_children(node) {
            if global && child.kind() == "statement_block" {
                self.statements(child);
            } else {
                self.visit(child);
            }
        }
        if global {
            self.global_depth -= 1;
        }
    }

    /// A nested block: a scope of its own when it binds a name.
    fn block(&mut self, node: Node<'_>) {
        let opened = binds_lexically(node);
        if opened {
            self.open_scope(false);
        }
        self.statements(node);
        if opened {
            self.close_scope();
        }
    }

    fn for_statement(&mut self, node: Node<'_>) {
        let opened = node
            .child_by_field_name("initializer")
            .is_some_and(|initializer| initializer.kind() == "lexical_declaration");
        if opened {
            self.open_scope(false);
        }
        self.children(node);
        if opened {
            self.close_scope();
        }
    }

    /// `for (... in/of ...)`: a `let`/`const` binding gets the loop's scope,
    /// a `var` binding the nearest function scope, and a bare target is
    /// written.
    fn for_in(&mut self, node: Node<'_>) {
        let keyword = node.child_by_field_name("kind").map(|kind| self.text(kind));
        let left = node.child_by_field_name("left");
        let opened = matches!(keyword.as_deref(), Some("let" | "const"));
        if opened {
            self.open_scope(false);
        }
        for child in named_children(node) {
            if Some(child) != left {
                continue;
            }
            match keyword.as_deref() {
                Some("var") => {
                    let scope = self.var_scope();
                    self.declare_pattern(child, Some(scope), BindingSpace::Value, Detail::Other);
                }
                Some(_) => {
                    let scope = self.current_scope();
                    self.declare_pattern(child, Some(scope), BindingSpace::Value, Detail::Other);
                }
                None => self.target(child),
            }
        }
        for child in named_children(node) {
            if Some(child) != left {
                self.visit(child);
            }
        }
        if opened {
            self.close_scope();
        }
    }

    /// `catch (e) { ... }`: the parameter and the body share one scope.
    fn catch_clause(&mut self, node: Node<'_>) {
        let Some(parameter) = node.child_by_field_name("parameter") else {
            self.children(node);
            return;
        };
        let scope = self.open_scope(false);
        self.declare_pattern(parameter, Some(scope), BindingSpace::Value, Detail::Other);
        for child in named_children(node) {
            if child == parameter {
                continue;
            }
            if child.kind() == "statement_block" {
                self.statements(child);
            } else {
                self.visit(child);
            }
        }
        self.close_scope();
    }

    /// A `switch` body is one block for the declarations of all its cases.
    fn switch_body(&mut self, node: Node<'_>) {
        let opened = named_children(node)
            .into_iter()
            .any(|case| binds_lexically(case));
        if opened {
            self.open_scope(false);
        }
        self.children(node);
        if opened {
            self.close_scope();
        }
    }

    /// An index signature, or a mapped type, whose key name binds a type in
    /// a scope of its own. A plain index signature's key name binds nothing.
    fn index_signature(&mut self, node: Node<'_>) {
        let name = node.child_by_field_name("name");
        let mapped = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "mapped_type_clause");
        if let Some(clause) = mapped {
            let scope = self.open_scope(false);
            let key = clause.child_by_field_name("name");
            if let Some(key) = key {
                self.declare(scope, key, BindingSpace::Type, Detail::Other);
            }
            for child in named_children(clause) {
                if Some(child) != key {
                    self.visit(child);
                }
            }
        }
        for child in named_children(node) {
            if Some(child) != name && Some(child) != mapped {
                self.visit(child);
            }
        }
        if mapped.is_some() {
            self.close_scope();
        }
    }

    /// A conditional type whose `extends` clause infers a name gets a scope
    /// for it; the name is visible in the whole conditional type, which can
    /// only over-report shadowing.
    fn conditional_type(&mut self, node: Node<'_>) {
        let opened = node
            .child_by_field_name("right")
            .is_some_and(contains_infer);
        if opened {
            self.open_scope(false);
        }
        self.children(node);
        if opened {
            self.close_scope();
        }
    }

    /// `infer U`: `U` binds a type; a constraint is code.
    fn infer(&mut self, node: Node<'_>) {
        let children = named_children(node);
        let name = children
            .iter()
            .copied()
            .find(|child| child.kind() == "type_identifier");
        if let Some(name) = name {
            let scope = self.current_scope();
            self.declare(scope, name, BindingSpace::Type, Detail::Other);
        }
        for child in children {
            if Some(child) != name {
                self.visit(child);
            }
        }
    }

    // -----------------------------------------------------------------
    // Imports and exports.
    // -----------------------------------------------------------------

    fn import(&mut self, node: Node<'_>) {
        let type_only = has_token(node, "type");
        let specifier = node
            .child_by_field_name("source")
            .map(|source| string_content(source, self.source))
            .unwrap_or_default();
        for child in named_children(node) {
            match child.kind() {
                "import_clause" => self.import_clause(child, &specifier, type_only),
                "import_require_clause" => {
                    let specifier = child
                        .child_by_field_name("source")
                        .map(|source| string_content(source, self.source))
                        .unwrap_or_default();
                    if let Some(local) = named_children(child)
                        .into_iter()
                        .find(|part| part.kind() == "identifier")
                    {
                        self.import_binding(
                            local,
                            ModuleImportKind::Require,
                            None,
                            &specifier,
                            type_only,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn import_clause(&mut self, clause: Node<'_>, specifier: &str, type_only: bool) {
        for part in named_children(clause) {
            match part.kind() {
                "identifier" => self.import_binding(
                    part,
                    ModuleImportKind::Default,
                    Some("default".to_string()),
                    specifier,
                    type_only,
                ),
                "namespace_import" => {
                    if let Some(local) = named_children(part)
                        .into_iter()
                        .find(|child| child.kind() == "identifier")
                    {
                        self.import_binding(
                            local,
                            ModuleImportKind::Namespace,
                            None,
                            specifier,
                            type_only,
                        );
                    }
                }
                "named_imports" => {
                    for item in named_children(part) {
                        if item.kind() != "import_specifier" {
                            continue;
                        }
                        let name = item.child_by_field_name("name");
                        let local = item.child_by_field_name("alias").or(name);
                        let (Some(name), Some(local)) = (name, local) else {
                            continue;
                        };
                        if local.kind() != "identifier" {
                            continue;
                        }
                        let imported = self.export_name(name);
                        let kind = if imported == "default" {
                            ModuleImportKind::Default
                        } else {
                            ModuleImportKind::Named
                        };
                        self.import_binding(
                            local,
                            kind,
                            Some(imported),
                            specifier,
                            type_only || has_token(item, "type"),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// One import binding: an `import` use on the local name and a
    /// [`ModuleImport`] in the current scope.
    fn import_binding(
        &mut self,
        local: Node<'_>,
        kind: ModuleImportKind,
        imported: Option<String>,
        specifier: &str,
        type_only: bool,
    ) {
        let Some(span) = span_of(local) else {
            return;
        };
        self.bare(local, RefKind::Import);
        let scope = self.current_scope();
        let local = self.text(local);
        self.scopes[scope].imports.push(ModuleImport {
            kind,
            local: Some(local),
            imported,
            exported: None,
            specifier: specifier.to_string(),
            type_only,
            span,
        });
    }

    fn export(&mut self, node: Node<'_>) {
        let source = node.child_by_field_name("source");
        let specifier = source.map(|source| string_content(source, self.source));
        let type_only = has_token(node, "type");
        // `export as namespace X` names a global, not a use.
        let as_namespace = has_token(node, "namespace");
        if specifier.is_none() {
            self.record_exports(node, type_only);
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        for child in children {
            match child.kind() {
                "export_clause" => match &specifier {
                    Some(specifier) => self.re_exports(child, specifier, type_only),
                    None => {
                        // `export { a as b }` names the local `a`.
                        for item in named_children(child) {
                            if let Some(name) = item.child_by_field_name("name")
                                && name.kind() == "identifier"
                                && self.text(name) != "default"
                            {
                                self.bare(name, RefKind::Unknown);
                            }
                        }
                    }
                },
                "namespace_export" => {
                    let exported = named_children(child)
                        .into_iter()
                        .find(|part| matches!(part.kind(), "identifier" | "string"));
                    if let (Some(exported), Some(specifier)) = (exported, &specifier) {
                        self.re_export_all(exported, Some(exported), specifier, type_only);
                    }
                }
                "*" if !child.is_named() => {
                    if let Some(specifier) = &specifier {
                        self.re_export_all(child, None, specifier, type_only);
                    }
                }
                "identifier" if as_namespace => {}
                _ if child.is_named() && Some(child) != source => self.visit(child),
                _ => {}
            }
        }
    }

    /// Records what one local `export` statement exports (T44), when it is
    /// written in a module's own scope and outside `declare global`.
    ///
    /// - `export <declaration>`: each name the declaration binds, under its
    ///   own name.
    /// - `export default <named declaration>`: its name, as `default`.
    /// - `export default <identifier>`: the identifier, as `default`.
    /// - `export default <expression>` or an anonymous default: `default`
    ///   with no local name.
    /// - `export { a, b as c, d as default }`: each specifier's local name
    ///   under its exported name.
    ///
    /// `export =` and `export as namespace` record nothing.
    fn record_exports(&mut self, node: Node<'_>, type_only: bool) {
        let scope = self.current_scope();
        if self.global_depth > 0 || !self.scopes[scope].module {
            return;
        }
        let mut cursor = node.walk();
        let default_keyword = node
            .children(&mut cursor)
            .find(|child| !child.is_named() && child.kind() == "default");
        let mut exports = Vec::new();
        if let Some(keyword) = default_keyword {
            let local = match node.child_by_field_name("declaration") {
                Some(declaration) => match declared_names(declaration).as_slice() {
                    [name] => Some(*name),
                    _ => None,
                },
                None => node
                    .child_by_field_name("value")
                    .filter(|value| value.kind() == "identifier"),
            };
            let span = match local {
                Some(name) => span_of(name),
                None => span_of(keyword),
            };
            if let Some(span) = span {
                exports.push(ModuleExport {
                    exported: "default".to_string(),
                    local: local.map(|name| self.text(name)),
                    type_only,
                    span,
                });
            }
        } else if let Some(declaration) = node.child_by_field_name("declaration") {
            for name in declared_names(declaration) {
                if let Some(span) = span_of(name) {
                    let text = self.text(name);
                    exports.push(ModuleExport {
                        exported: text.clone(),
                        local: Some(text),
                        type_only: false,
                        span,
                    });
                }
            }
        } else {
            for clause in named_children(node) {
                if clause.kind() != "export_clause" {
                    continue;
                }
                for item in named_children(clause) {
                    let Some(name) = item
                        .child_by_field_name("name")
                        .filter(|name| name.kind() == "identifier")
                    else {
                        continue;
                    };
                    let Some(span) = span_of(name) else {
                        continue;
                    };
                    let local = self.text(name);
                    let exported = item
                        .child_by_field_name("alias")
                        .map(|alias| self.export_name(alias))
                        .unwrap_or_else(|| local.clone());
                    exports.push(ModuleExport {
                        exported,
                        local: Some(local),
                        type_only: type_only || has_token(item, "type"),
                        span,
                    });
                }
            }
        }
        self.scopes[scope].exports.extend(exports);
    }

    /// `export { a as b } from "m"`: each specifier is a re-export, and its
    /// re-exported name is an `import` use (decision 1), unless it is the
    /// `default` keyword or a string.
    fn re_exports(&mut self, clause: Node<'_>, specifier: &str, type_only: bool) {
        for item in named_children(clause) {
            if item.kind() != "export_specifier" {
                continue;
            }
            let Some(name) = item.child_by_field_name("name") else {
                continue;
            };
            let Some(span) = span_of(name) else {
                continue;
            };
            let imported = self.export_name(name);
            let exported = item
                .child_by_field_name("alias")
                .map(|alias| self.export_name(alias))
                .unwrap_or_else(|| imported.clone());
            if name.kind() == "identifier" && imported != "default" {
                self.bare(name, RefKind::Import);
            }
            let scope = self.current_scope();
            self.scopes[scope].imports.push(ModuleImport {
                kind: ModuleImportKind::ReExport,
                local: None,
                imported: Some(imported),
                exported: Some(exported),
                specifier: specifier.to_string(),
                type_only: type_only || has_token(item, "type"),
                span,
            });
        }
    }

    /// `export * from "m"` (span on `*`) or `export * as ns from "m"`: no
    /// binding and no use.
    fn re_export_all(
        &mut self,
        at: Node<'_>,
        exported: Option<Node<'_>>,
        specifier: &str,
        type_only: bool,
    ) {
        let Some(span) = span_of(at) else {
            return;
        };
        let exported = exported.map(|exported| self.export_name(exported));
        let scope = self.current_scope();
        self.scopes[scope].imports.push(ModuleImport {
            kind: ModuleImportKind::ReExportAll,
            local: None,
            imported: None,
            exported,
            specifier: specifier.to_string(),
            type_only,
            span,
        });
    }

    /// `import r = A.B;`: `r` binds a local alias; the aliased name is code.
    /// It is not a module import, so it records no [`ModuleImport`].
    fn import_alias(&mut self, node: Node<'_>) {
        let children = named_children(node);
        let local = children
            .iter()
            .copied()
            .find(|child| child.kind() == "identifier");
        if let Some(local) = local {
            let scope = self.current_scope();
            self.declare(scope, local, BindingSpace::Both, Detail::Other);
        }
        for child in children {
            if Some(child) != local {
                self.visit(child);
            }
        }
    }

    /// An export or import name as written: an identifier's text or a
    /// string's contents.
    fn export_name(&self, node: Node<'_>) -> String {
        match node.kind() {
            "string" => string_content(node, self.source),
            _ => self.text(node),
        }
    }

    // -----------------------------------------------------------------
    // JSX.
    // -----------------------------------------------------------------

    /// An opening or self-closing element: a capitalized or dotted name is a
    /// component `call`; a lowercase or namespaced name is an intrinsic
    /// element. Attribute names and string values are not uses; expression
    /// containers are code.
    fn jsx_element(&mut self, node: Node<'_>) {
        let name = node.child_by_field_name("name");
        if let Some(name) = name {
            match name.kind() {
                "identifier" => {
                    if !self
                        .text(name)
                        .starts_with(|c: char| c.is_ascii_lowercase())
                    {
                        self.bare(name, RefKind::Call);
                    }
                }
                "member_expression" => self.member(name, RefKind::Call),
                "nested_identifier" => self.dotted(name, RefKind::Call),
                _ => {}
            }
        }
        for child in named_children(node) {
            if Some(child) != name {
                self.visit(child);
            }
        }
    }

    // -----------------------------------------------------------------
    // Finishing.
    // -----------------------------------------------------------------

    /// Decides the pending receiver hints, sorts the uses, and freezes the
    /// scopes.
    fn finish(mut self) -> (Vec<ExtractedUse>, Vec<ExtractedScope>) {
        for (index, name, scope) in std::mem::take(&mut self.pending) {
            let hint = self.lookup_hint(scope, &name);
            self.uses[index].hint = hint;
        }
        let mut uses = std::mem::take(&mut self.uses);
        uses.sort_by(|a, b| {
            (a.span.start_byte(), a.span.end_byte()).cmp(&(b.span.start_byte(), b.span.end_byte()))
        });
        debug_assert!(
            uses.windows(2).all(|pair| pair[0].span != pair[1].span),
            "an identifier is recorded as at most one use"
        );
        uses.dedup_by(|b, a| a.span == b.span);

        let mut member_sides = std::mem::take(&mut self.member_sides);
        member_sides.sort_by_key(|side| side.symbol);
        member_sides.dedup_by_key(|side| side.symbol);
        let keys: Vec<String> = self.scopes.iter().map(|scope| scope.key.clone()).collect();
        let mut scopes: Vec<ExtractedScope> = self
            .scopes
            .into_iter()
            .map(|scope| {
                let mut locals: Vec<LocalBinding> = scope
                    .locals
                    .into_iter()
                    .map(|local| local.binding)
                    .collect();
                locals.sort_by_key(|local| (local.span.start_byte(), local.span.end_byte()));
                let mut declares = scope.declares;
                declares.sort_unstable();
                declares.dedup();
                let mut module_imports = scope.imports;
                module_imports
                    .sort_by_key(|import| (import.span.start_byte(), import.span.end_byte()));
                let mut module_exports = scope.exports;
                module_exports
                    .sort_by_key(|export| (export.span.start_byte(), export.span.end_byte()));
                let mut value_type_uses = scope.value_type_uses;
                value_type_uses.sort_by_key(|span| (span.start_byte(), span.end_byte()));
                value_type_uses.dedup();
                ExtractedScope {
                    scope_key: scope.key,
                    parent_scope_key: scope.parent.map(|parent| keys[parent].clone()),
                    facts: ScopeFacts {
                        locals,
                        module_imports,
                        module_exports,
                        value_type_uses,
                        // A module's own scope other than the file's is a
                        // string-named ambient module body.
                        ambient_module: scope.module && scope.parent.is_some(),
                        // Recorded once per file, in its module scope (T45).
                        member_sides: if scope.parent.is_none() {
                            std::mem::take(&mut member_sides)
                        } else {
                            Vec::new()
                        },
                        declares,
                        ..ScopeFacts::default()
                    },
                }
            })
            .collect();
        scopes.sort_by(|a, b| a.scope_key.cmp(&b.scope_key));
        (uses, scopes)
    }

    /// The receiver hint of a bare name in `scope`: the nearest scope that
    /// binds it as a value decides. An import binding, a name bound twice in
    /// that scope, or a binding with no single explicit type or `new` gives
    /// no hint.
    fn lookup_hint(&self, scope: usize, name: &str) -> UseHint {
        let mut current = Some(scope);
        while let Some(index) = current {
            let scope = &self.scopes[index];
            if scope
                .imports
                .iter()
                .any(|import| import.local.as_deref() == Some(name))
            {
                return UseHint::Unresolved;
            }
            let mut matches = scope.locals.iter().filter(|local| {
                local.binding.name == name && local.binding.space != BindingSpace::Type
            });
            if let Some(local) = matches.next() {
                if matches.next().is_some() {
                    return UseHint::Unresolved;
                }
                return match &local.detail {
                    Detail::Parameter(Some(ty)) => UseHint::Typed {
                        type_spelling: ty.spelling.clone(),
                        origin: TypedOrigin::Parameter,
                        name_span: ty.span,
                    },
                    Detail::Variable {
                        annotation: Some(ty),
                        ..
                    } => UseHint::Typed {
                        type_spelling: ty.spelling.clone(),
                        origin: TypedOrigin::Variable,
                        name_span: ty.span,
                    },
                    Detail::Variable {
                        new_class: Some(class),
                        ..
                    } => UseHint::NewExpr {
                        class_spelling: class.spelling.clone(),
                        use_block: None,
                        name_span: class.span,
                    },
                    _ => UseHint::Unresolved,
                };
            }
            current = scope.parent;
        }
        UseHint::Unresolved
    }

    fn text(&self, node: Node<'_>) -> String {
        text_of(node, self.source)
    }
}

/// Whether a symbol of `kind` is a use container (the PHP container rule).
fn is_container(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Class
            | SymbolKind::Interface
            | SymbolKind::Enum
    )
}

/// The single named types of a named class's fields and constructor
/// parameter properties.
fn collect_fields(body: Node<'_>, source: &[u8]) -> Fields {
    let mut fields = Fields::default();
    for member in named_children(body) {
        match member.kind() {
            "public_field_definition" => {
                let name = member.child_by_field_name("name");
                let Some(name) = name.filter(|name| {
                    matches!(
                        name.kind(),
                        "property_identifier" | "private_property_identifier"
                    )
                }) else {
                    continue;
                };
                let spelling = member
                    .child_by_field_name("type")
                    .and_then(|ty| named_type(ty, source));
                let map = if has_modifier(member, Some(name), "static") {
                    &mut fields.statics
                } else {
                    &mut fields.instance
                };
                Fields::insert(map, text_of(name, source), spelling);
            }
            "method_definition" => {
                let is_constructor = member
                    .child_by_field_name("name")
                    .is_some_and(|name| text_of(name, source) == "constructor");
                let Some(parameters) = member
                    .child_by_field_name("parameters")
                    .filter(|_| is_constructor)
                else {
                    continue;
                };
                for parameter in named_children(parameters) {
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
                    let spelling = parameter
                        .child_by_field_name("type")
                        .and_then(|ty| named_type(ty, source));
                    Fields::insert(&mut fields.instance, text_of(name, source), spelling);
                }
            }
            _ => {}
        }
    }
    fields
}

/// The one named type a type annotation (or type) names: `Foo`, `ns.Foo`, or
/// the generic `Foo` of `Foo<T>`. Any other type (a union, an array, a
/// literal, a function type, a predefined type) names none. The span is the
/// last identifier's (`Foo` of `ns.Foo`), where the name's `type` use is.
fn named_type(node: Node<'_>, source: &[u8]) -> Option<NamedType> {
    let ty = if node.kind() == "type_annotation" {
        named_children(node).into_iter().next()?
    } else {
        node
    };
    let name = match ty.kind() {
        "type_identifier" | "nested_type_identifier" => ty,
        "generic_type" => ty.child_by_field_name("name")?,
        _ => return None,
    };
    let last = match name.kind() {
        "nested_type_identifier" => name.child_by_field_name("name"),
        _ => Some(name),
    };
    Some(NamedType {
        spelling: text_of(name, source),
        span: last.and_then(span_of),
    })
}

/// The class a `new C(...)` initializer names, as written, with the span of
/// its `type` use (`C`, or the last name of `new ns.C()`).
fn new_class(value: Node<'_>, source: &[u8]) -> Option<NamedType> {
    if value.kind() != "new_expression" {
        return None;
    }
    let constructor = value.child_by_field_name("constructor")?;
    let last = match constructor.kind() {
        "identifier" => Some(constructor),
        "member_expression" => constructor.child_by_field_name("property"),
        _ => return None,
    };
    Some(NamedType {
        spelling: text_of(constructor, source),
        span: last.and_then(span_of),
    })
}

/// Whether a block (or a `switch` case) binds a name lexically: a `let` or
/// `const`, a class, an enum, an interface, a type alias, or an alias
/// directly in it. `var` and function declarations bind in the enclosing
/// function scope instead.
fn binds_lexically(node: Node<'_>) -> bool {
    named_children(node).into_iter().any(|child| {
        matches!(
            child.kind(),
            "lexical_declaration"
                | "class_declaration"
                | "abstract_class_declaration"
                | "enum_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "import_alias"
        )
    })
}

/// Whether a type contains an `infer` declaration.
fn contains_infer(node: Node<'_>) -> bool {
    if node.kind() == "infer_type" {
        return true;
    }
    named_children(node).into_iter().any(contains_infer)
}

/// Whether `node` has the anonymous keyword `token` before its `name` child
/// (`static`), or anywhere when it has no name.
fn has_modifier(node: Node<'_>, name: Option<Node<'_>>, token: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if name.is_some_and(|name| child.start_byte() >= name.start_byte()) {
            break;
        }
        if !child.is_named() && child.kind() == token {
            return true;
        }
    }
    false
}

/// Whether `node` has the anonymous keyword `token` as a direct child.
fn has_token(node: Node<'_>, token: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| !child.is_named() && child.kind() == token)
}

/// The name nodes a declaration binds in its scope, for its exports (T44):
/// the name of a function, class, interface, type alias, or enum; every name
/// a `const`, `let`, or `var` declarator's pattern binds; the first segment of
/// a namespace name; the alias of `import X = ...`; and, through `declare`,
/// the names of the declaration it wraps. A string-named module and `declare
/// global` bind none.
fn declared_names(node: Node<'_>) -> Vec<Node<'_>> {
    match node.kind() {
        "function_declaration"
        | "generator_function_declaration"
        | "function_signature"
        | "class_declaration"
        | "abstract_class_declaration"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration" => node.child_by_field_name("name").into_iter().collect(),
        "lexical_declaration" | "variable_declaration" => {
            let mut names = Vec::new();
            for declarator in named_children(node) {
                if declarator.kind() == "variable_declarator"
                    && let Some(pattern) = declarator.child_by_field_name("name")
                {
                    pattern_names(pattern, &mut names);
                }
            }
            names
        }
        "internal_module" | "module" => node
            .child_by_field_name("name")
            .and_then(leftmost_segment)
            .into_iter()
            .collect(),
        "import_alias" => named_children(node)
            .into_iter()
            .find(|child| child.kind() == "identifier")
            .into_iter()
            .collect(),
        "ambient_declaration" => named_children(node)
            .into_iter()
            .flat_map(declared_names)
            .collect(),
        _ => Vec::new(),
    }
}

/// Appends every name a binding pattern binds, in source order.
fn pattern_names<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => out.push(node),
        "object_pattern" | "array_pattern" | "rest_pattern" => {
            for child in named_children(node) {
                pattern_names(child, out);
            }
        }
        "pair_pattern" => {
            if let Some(value) = node.child_by_field_name("value") {
                pattern_names(value, out);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(left) = node.child_by_field_name("left") {
                pattern_names(left, out);
            }
        }
        _ => {}
    }
}

/// The first identifier of a (possibly dotted) namespace name, or `None` for
/// a string name.
fn leftmost_segment(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "identifier" => Some(node),
        "nested_identifier" | "member_expression" => {
            leftmost_segment(node.child_by_field_name("object")?)
        }
        _ => None,
    }
}

/// A string literal's contents without its quotes.
fn string_content(node: Node<'_>, source: &[u8]) -> String {
    let text = text_of(node, source);
    let trimmed = text
        .strip_prefix(['"', '\''])
        .and_then(|rest| rest.strip_suffix(['"', '\'']));
    trimmed.unwrap_or(&text).to_string()
}

fn span_of(node: Node<'_>) -> Option<Span> {
    Span::new(node.start_byte() as u32, node.end_byte() as u32).ok()
}

fn text_of(node: Node<'_>, source: &[u8]) -> String {
    String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]).into_owned()
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}
