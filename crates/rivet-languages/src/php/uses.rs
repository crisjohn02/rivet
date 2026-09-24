//! PHP identifier-use and import extraction (T17).
//!
//! This is a documented scope walk rather than a `.scm` query because receiver
//! hints need assignment order and shadowing facts (docs/ADDING-A-LANGUAGE.md
//! "Implementation checklist"). It classifies:
//!
//! - function calls, method calls, and static calls as [`RefKind::Call`];
//! - `new Foo(...)` and named types in parameters, returns, properties, and
//!   constants as [`RefKind::Type`];
//! - `use` declarations as [`RefKind::Import`] plus [`ExtractedImport`] rows;
//! - class-constant and property accesses as [`RefKind::Read`];
//! - property assignments as [`RefKind::Write`];
//! - any other reached identifier as [`RefKind::Unknown`].
//!
//! Comments, single-quoted strings, and the literal portions of interpolated
//! strings are skipped; expressions interpolated inside `"{...}"` are walked.
//! Declaration names and bare variable names are never uses.
//!
//! T21b records two more scope facts: a positional call argument that passes a
//! bare variable (so the resolver can decide whether the callee's parameter
//! rebinds it), and a per-scope `unanalysable` flag for a dynamic variable
//! write, a `$GLOBALS` write, `extract`, `eval`, or a dynamic-callee call.
//!
//! AF3 adds:
//!
//! - the origin of a typed receiver (parameter or property), and no typed
//!   receiver for a union, intersection, or DNF type, or a by-reference
//!   parameter;
//! - a reference taken to a variable (`$y = &$x`, a by-reference `foreach`
//!   over it) as a rebinding of that variable;
//! - constructor arguments as call arguments of `Class::__construct`, and
//!   the constructor arguments walked as code at all;
//! - per-function by-reference parameter positions read from the parse tree;
//!   and
//! - the facts behind the `global` rule: which scopes are global, the calls
//!   in them, `goto`, and every name a scope can rebind through `global` or
//!   `$GLOBALS`.
//!
//! AF4 adds:
//!
//! - anonymous class bodies are walked; their uses keep the nearest named
//!   container, but `$this`, `self`, and `static` inside them record no
//!   receiver evidence, because they name the anonymous class;
//! - the class named before `::` in a static call, a class-constant access, a
//!   static property access, or `::class` as a [`RefKind::Type`] use, and the
//!   member after it with a [`UseHint::NamedClass`] hint; `::class` itself is
//!   not a member use;
//! - the class operand of `instanceof` as a [`RefKind::Type`] use; and
//! - no use for an enum case's own name, which is a declaration.
//!
//! T36d adds:
//!
//! - each name in a class's `extends` and `implements` clauses, an enum's
//!   `implements` clause, and an interface's `extends` list as a
//!   [`RefKind::Type`] use, for named and anonymous classes alike; the use's
//!   container is the declared class-like (for an anonymous class, the
//!   nearest named container, since it is not a symbol);
//! - for a named class, interface, or enum, one [`DeclaredSupertype`] fact
//!   per such name in the scope that declares it, so the index can answer
//!   hierarchy questions from the same use and its binding; and
//! - an expression scope before `::` (`$obj::$p`, `$obj::C`,
//!   `$this->f()::$p`) walked as code, so the uses inside it are recorded;
//!   the expression itself is never a type use.
//!
//! LR2 adds one [`AnonymousSupertype`] fact per name in an anonymous class's
//! `extends`/`implements` clauses, in the scope of that name's type use, so
//! reference-mode exclusion sees anonymous classes as possible subtypes.

use std::collections::{BTreeMap, HashMap};

use rivet_core::extract::{
    AnonymousSupertype, CallArg, CallArgKind, CallReceiver, DeclaredSupertype, ExtractedImport,
    ExtractedScope, ExtractedUse, ImportKind, NewBinding, ParameterList, ReceiverEvidence,
    ScopeFacts, ScopeImport, SupertypeRelation, TypedBinding, TypedOrigin, UseHint,
};
use rivet_core::{ExtractedSymbol, RefKind, Span, SymbolKind};
use tree_sitter::Node;

use super::namespaces::{Attribution, NamespaceLayout};

/// The deterministic key of the top-level scope of a file with no namespace.
///
/// A namespaced file has one top-level scope per namespace block instead,
/// keyed `ns{block}:file` (AF1), plus `orphan:file` for any position that
/// cannot be attributed to exactly one block.
pub const FILE_SCOPE_KEY: &str = "top:file";

/// The scope-key prefix of positions no namespace block owns.
const ORPHAN_PREFIX: &str = "orphan";

/// The most recent binding of one variable within a function body.
enum Binding {
    /// The most recent assignment was `new X(...)`; the second value is the
    /// end byte of that `new` expression.
    New(String, u32),
    /// The variable is a typed parameter or promoted property.
    Typed(String),
    /// Known but unsupported for hints.
    Other,
}

/// The per-file use walker.
struct Walker<'a> {
    /// The most uses this file may record (spec §27).
    max_uses: u64,
    /// Set once one more use than `max_uses` was seen; the walk then stops.
    use_limit_exceeded: bool,
    source: &'a [u8],
    symbols: &'a [ExtractedSymbol],
    /// Which namespace block owns each byte (AF1).
    layout: &'a NamespaceLayout,
    /// Class-like node id -> property name (no `$`) -> written type spelling.
    property_types: HashMap<usize, HashMap<String, String>>,
    uses: Vec<ExtractedUse>,
    imports: Vec<ExtractedImport>,
    /// Owned lexical facts per scope key, filled while walking.
    scope_facts: BTreeMap<String, ScopeFacts>,
    /// Stack of lexical function-body variable scopes; the last is current.
    scopes: Vec<HashMap<String, Binding>>,
    /// Stack of enclosing class-like node ids, each with whether it is an
    /// anonymous class (AF4).
    class_stack: Vec<(usize, bool)>,
    /// Stack of enclosing function-body ordinals; `None` at file scope.
    body_stack: Vec<Option<u32>>,
    next_body_ordinal: u32,
    /// Function/method symbol index -> by-reference flag per parameter (AF3).
    parameter_lists: BTreeMap<usize, Vec<bool>>,
    /// Class-like symbol index -> its declared supertypes in source order
    /// (T36d).
    supertypes: BTreeMap<usize, Vec<DeclaredSupertype>>,
}

/// Extract uses, imports, and lexical scope facts from a parsed, error-free
/// PHP file.
///
/// `symbols` must be the already-built symbol list, because uses record the
/// index of their innermost named container and scope facts record
/// declarations by symbol index.
///
/// Returns `None` when the file yields more than `max_uses` uses (spec §27).
/// The walker stops recording and descending at the first use over the bound,
/// so an oversized file costs no more work than the bound allows.
pub fn extract_uses(
    source: &[u8],
    root: Node<'_>,
    symbols: &[ExtractedSymbol],
    layout: &NamespaceLayout,
    max_uses: u64,
) -> Option<(Vec<ExtractedUse>, Vec<ExtractedImport>, Vec<ExtractedScope>)> {
    let mut walker = Walker {
        max_uses,
        use_limit_exceeded: false,
        source,
        symbols,
        layout,
        property_types: collect_property_types(root, source),
        uses: Vec::new(),
        imports: Vec::new(),
        scope_facts: BTreeMap::new(),
        scopes: vec![HashMap::new()],
        class_stack: Vec::new(),
        body_stack: vec![None],
        next_body_ordinal: 0,
        parameter_lists: BTreeMap::new(),
        supertypes: BTreeMap::new(),
    };
    walker.visit(root, false);
    if walker.use_limit_exceeded {
        return None;
    }
    walker.uses.sort_by(|a, b| {
        a.span
            .start_byte()
            .cmp(&b.span.start_byte())
            .then(a.span.end_byte().cmp(&b.span.end_byte()))
    });
    let scopes = walker.finish_scopes();
    Some((walker.uses, walker.imports, scopes))
}

impl Walker<'_> {
    /// Visit one node, classifying it or descending generically.
    ///
    /// `write` is true while walking a binding target: the left side of an
    /// assignment, a `foreach` target, a `catch` parameter, and so on. A
    /// property access there becomes [`RefKind::Write`], and a bare variable
    /// mention there is recorded as a rebinding (T21a).
    fn visit(&mut self, node: Node<'_>, write: bool) {
        // Cooperative resource bound: once the use limit is exceeded the
        // result is discarded, so nothing below needs to run.
        if self.use_limit_exceeded {
            return;
        }
        match node.kind() {
            // Literal text and comments are never uses.
            "comment" | "string" | "nowdoc" | "string_content" | "escape_sequence" => {}
            // Interpolated strings keep their expressions as code.
            "encapsed_string" | "heredoc" => {
                for child in named_children(node) {
                    if !matches!(child.kind(), "string_content" | "escape_sequence") {
                        self.visit(child, false);
                    }
                }
            }
            // A variable mention is a read unless the walker reached it while
            // walking a binding target (T21a). A binding-target mention records
            // an extra binding fact, so a variable the walker cannot account
            // for never looks singly assigned to the `new`-receiver rule.
            "variable_name" => {
                if write {
                    self.bind_variable(node, Binding::Other);
                }
            }
            // `dynamic_variable_name` names a variable indirectly. A read is
            // harmless, but a write (`$$name = ...`, `${$expr} = ...`) rebinds
            // a variable the walker cannot name, so the whole scope is
            // unanalysable (T21b).
            "dynamic_variable_name" => {
                if write {
                    self.mark_unanalysable(node);
                }
            }
            // The closed set of PHP constructs that can bind a variable. Each
            // routes its target mentions through `write`, so the resolver's
            // "exactly one recorded assignment" test rejects a variable that is
            // rebound by any of them. A mention reached anywhere else is a read.
            "list_literal" => {
                for child in named_children(node) {
                    self.visit(child, true);
                }
            }
            "by_ref" => {
                for child in named_children(node) {
                    self.visit(child, true);
                }
            }
            "augmented_assignment_expression" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.visit(left, true);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.visit(right, false);
                }
            }
            // `$y = &$x` rebinds `$y`, and also makes `$x` an alias: any later
            // write through `$y` rebinds `$x`, so `$x` is rebound too (AF3).
            "reference_assignment_expression" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.visit(left, true);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.visit(right, false);
                    self.mark_referenced(right);
                }
            }
            // `$s++`/`++$s` rebind the variable as well as read it.
            "update_expression" => {
                if let Some(argument) = node.child_by_field_name("argument") {
                    self.visit(argument, true);
                }
            }
            "foreach_statement" => self.visit_foreach(node),
            "catch_clause" => self.visit_catch(node),
            "anonymous_function_use_clause" => self.visit_closure_uses(node),
            "unset_statement" => {
                for child in named_children(node) {
                    self.visit(child, true);
                }
            }
            // `global $x` rebinds the local `$x`, and lets this scope rebind
            // the global `$x` that file-scope code sees (AF3).
            "global_declaration" => {
                for child in named_children(node) {
                    match child.kind() {
                        "variable_name" => {
                            let name = self.text(child);
                            self.note_global_name(child, name);
                        }
                        _ => self.note_dynamic_global_write(child),
                    }
                    self.visit(child, true);
                }
            }
            // A backward jump breaks the source-order reasoning of the
            // `global` rule (AF3).
            "goto_statement" => {
                let scope_key = self.scope_key_for_node(node);
                if is_global_scope_key(&scope_key) {
                    self.scope_facts.entry(scope_key).or_default().goto_present = true;
                }
            }
            // `clone` runs `__clone`, user code that may rebind a global.
            "clone_expression" => {
                self.record_call_site(node);
                for child in named_children(node) {
                    self.visit(child, false);
                }
            }
            "static_variable_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.visit(name, true);
                }
                if let Some(value) = node.child_by_field_name("value") {
                    self.visit(value, false);
                }
            }
            // An include executes the included file in the current scope, so it
            // can define or overwrite any local; the scope is unanalysable
            // (T21b).
            "include_expression"
            | "include_once_expression"
            | "require_expression"
            | "require_once_expression" => {
                self.mark_unanalysable(node);
                // Included code inside a function may declare any name
                // `global` and rebind it (AF3).
                self.note_dynamic_global_write(node);
                for child in named_children(node) {
                    self.visit(child, false);
                }
            }
            // A subscript target (`$a[$i] = ...`) writes its base container;
            // the indices are reads. A write to `$GLOBALS[...]` can rebind any
            // global, which the walker cannot map to a local name, so the whole
            // scope becomes unanalysable (T21b).
            "subscript_expression" => {
                let mut children = named_children(node).into_iter();
                if let Some(base) = children.next() {
                    if write && base.kind() == "variable_name" && self.text(base) == "$GLOBALS" {
                        self.mark_unanalysable(node);
                        // The written key is a global another scope's code may
                        // see rebound (AF3).
                        self.note_globals_key(node, named_children(node).get(1).copied());
                    }
                    self.visit(base, write);
                }
                for index in children {
                    self.visit(index, false);
                }
            }
            // Imports are handled whole so their target names are not mistaken
            // for identifier uses.
            "namespace_use_declaration" => self.handle_use(node),
            "namespace_definition" => {
                if let Some(body) = node.child_by_field_name("body") {
                    self.visit(body, false);
                }
            }
            "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration"
            | "anonymous_class" => self.visit_class(node),
            "function_definition"
            | "method_declaration"
            | "anonymous_function"
            | "arrow_function" => self.visit_function(node),
            "simple_parameter" => {
                self.visit_parameter_type(node);
                if let Some(default) = node.child_by_field_name("default_value") {
                    self.visit(default, false);
                }
            }
            "property_promotion_parameter" => {
                self.visit_parameter_type(node);
                if let Some(default) = node.child_by_field_name("default_value") {
                    self.visit(default, false);
                }
            }
            "variadic_parameter" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    self.visit(ty, false);
                }
            }
            "property_declaration" => {
                for child in named_children(node) {
                    if child.kind() == "property_element" {
                        if let Some(default) = child.child_by_field_name("default_value") {
                            self.visit(default, false);
                        }
                    } else {
                        self.visit(child, false);
                    }
                }
            }
            "const_declaration" => {
                for child in named_children(node) {
                    if child.kind() == "const_element" {
                        for element_child in named_children(child) {
                            if element_child.kind() != "name" {
                                self.visit(element_child, false);
                            }
                        }
                    } else {
                        self.visit(child, false);
                    }
                }
            }
            // A named type is always a type position.
            "named_type" => {
                if let Some(name) = node.named_child(0) {
                    self.push_use(name, RefKind::Type, None, UseHint::Unresolved);
                }
            }
            "function_call_expression" => self.visit_function_call(node),
            "member_call_expression" | "nullsafe_member_call_expression" => {
                self.visit_member_call(node)
            }
            "scoped_call_expression" => self.visit_scoped_call(node),
            "object_creation_expression" => self.visit_object_creation(node),
            "member_access_expression" | "nullsafe_member_access_expression" => {
                self.visit_member_access(node, write)
            }
            "scoped_property_access_expression" => self.visit_scoped_property(node, write),
            "class_constant_access_expression" => self.visit_class_constant(node),
            "assignment_expression" => self.visit_assignment(node),
            // Named arguments carry a label that is not a reference. A
            // by-reference argument can let the callee rebind the variable, so
            // it is visited as a binding target.
            "argument" => {
                let label = node.child_by_field_name("name").map(|label| label.id());
                let by_ref = node.child_by_field_name("reference_modifier").is_some();
                for child in named_children(node) {
                    if Some(child.id()) != label && child.kind() != "reference_modifier" {
                        // A `$GLOBALS` element passed to a by-reference
                        // parameter rebinds that global; the call site alone
                        // cannot say which parameters are by-reference (AF3).
                        if globals_root(child, self.source) {
                            self.mark_referenced(child);
                        }
                        self.visit(child, by_ref);
                    }
                }
            }
            // An enum case's name is a declaration, never a use; its backing
            // value is code (AF4).
            "enum_case" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.visit(value, false);
                }
            }
            // The class operand of `instanceof` is a type position (AF4).
            "binary_expression" if is_instanceof(node) => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.visit(left, false);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    if matches!(right.kind(), "name" | "qualified_name" | "relative_name") {
                        self.push_use(right, RefKind::Type, None, UseHint::Unresolved);
                    } else {
                        self.visit(right, false);
                    }
                }
            }
            // A bare constant reference or namespaced constant is unclassified.
            "name" | "qualified_name" | "relative_name" => {
                self.push_use(node, RefKind::Unknown, None, UseHint::Unresolved);
            }
            _ => {
                for child in named_children(node) {
                    self.visit(child, write);
                }
            }
        }
    }

    /// Descend into a class-like declaration body, tracking the class context.
    ///
    /// The header's `extends`/`implements` names are recorded first (T36d),
    /// outside the class context: they are written in the declaring scope's
    /// terms, like any other class name there.
    fn visit_class(&mut self, node: Node<'_>) {
        self.visit_supertype_clauses(node);
        self.class_stack
            .push((node.id(), node.kind() == "anonymous_class"));
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, false);
        } else {
            for child in named_children(node) {
                if matches!(child.kind(), "declaration_list" | "enum_declaration_list") {
                    self.visit(child, false);
                }
            }
        }
        self.class_stack.pop();
    }

    /// Records each name in a class-like header's `extends` (`base_clause`) and
    /// `implements` (`class_interface_clause`) clauses as a type use (T36d).
    ///
    /// Each name is pushed exactly as [`record_scope_class`](Self::record_scope_class)
    /// pushes an explicit class scope, so it resolves through the same import
    /// and namespace rules. A named class, interface, or enum also records one
    /// [`DeclaredSupertype`] per name. An anonymous class is not a symbol, so
    /// it records one [`AnonymousSupertype`] per name instead (LR2); a trait
    /// has no supertypes.
    fn visit_supertype_clauses(&mut self, node: Node<'_>) {
        let declared = match node.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                self.declared_class_like(node)
            }
            _ => None,
        };
        for clause in named_children(node) {
            let relation = match clause.kind() {
                "base_clause" => SupertypeRelation::Extends,
                "class_interface_clause" => SupertypeRelation::Implements,
                _ => continue,
            };
            for name in named_children(clause) {
                let before = self.uses.len();
                if !self.record_scope_class(name) {
                    continue;
                }
                // An anonymous class's header names are hierarchy candidates
                // too (LR2), kept in the scope of their recorded type use.
                if node.kind() == "anonymous_class"
                    && self.uses.len() > before
                    && let Some(recorded) = self.uses.last()
                {
                    let (scope_key, span) = (recorded.scope_key.clone(), recorded.span);
                    let fact = AnonymousSupertype {
                        class_start: node.start_byte() as u32,
                        relation,
                        spelling: self.text(name),
                        span,
                    };
                    self.scope_facts
                        .entry(scope_key)
                        .or_default()
                        .anonymous_supertypes
                        .push(fact);
                    continue;
                }
                let (Some(symbol), Ok(span)) = (
                    declared,
                    Span::new(name.start_byte() as u32, name.end_byte() as u32),
                ) else {
                    continue;
                };
                let spelling = self.text(name);
                self.supertypes
                    .entry(symbol)
                    .or_default()
                    .push(DeclaredSupertype {
                        symbol,
                        relation,
                        spelling,
                        span,
                    });
            }
        }
    }

    /// The index of the class, interface, or enum symbol declared by `node`,
    /// whose span is exactly the declaration node's.
    fn declared_class_like(&self, node: Node<'_>) -> Option<usize> {
        self.symbols.iter().position(|symbol| {
            matches!(
                symbol.kind,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
            ) && symbol.span.start_byte() == node.start_byte() as u32
                && symbol.span.end_byte() == node.end_byte() as u32
        })
    }

    /// Enter a function body, then visit its parameters, return type, and body.
    fn visit_function(&mut self, node: Node<'_>) {
        // A closure's capture list is evaluated in the enclosing scope, so it
        // must be visited before the closure's own variable scope is pushed.
        // A by-reference capture lets the closure rebind the outer variable.
        for child in named_children(node) {
            if child.kind() == "anonymous_function_use_clause" {
                self.visit(child, false);
            }
        }
        let ordinal = self.next_body_ordinal;
        self.next_body_ordinal += 1;
        self.body_stack.push(Some(ordinal));
        self.scopes.push(HashMap::new());

        // A named function or method records which parameters are declared by
        // reference, read from the tree so no attribute, default value, or
        // comment text can mislead it (AF3). A list with an unrecognized
        // child records nothing, so the resolver treats it as unknown.
        if let (Some(parameters), Some(symbol)) = (
            node.child_by_field_name("parameters"),
            self.declared_symbol(node),
        ) && let Some(by_ref) = parameter_by_ref_flags(parameters)
        {
            self.parameter_lists.insert(symbol, by_ref);
        }

        if let Some(parameters) = node.child_by_field_name("parameters") {
            for parameter in named_children(parameters) {
                self.visit(parameter, false);
            }
        }
        if let Some(return_type) = node.child_by_field_name("return_type") {
            self.visit(return_type, false);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, false);
        }

        self.body_stack.pop();
        self.scopes.pop();
    }

    /// The index of the function or method symbol declared by `node`, whose
    /// span is exactly the declaration node's.
    fn declared_symbol(&self, node: Node<'_>) -> Option<usize> {
        self.symbols.iter().position(|symbol| {
            matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                && symbol.span.start_byte() == node.start_byte() as u32
                && symbol.span.end_byte() == node.end_byte() as u32
        })
    }

    /// Visit a parameter's type and record a typed binding for its variable.
    ///
    /// Only a single class type binds (`A`, or `?A`, since a call on null is an
    /// error rather than another target); a union, intersection, or DNF type
    /// names no one class (AF3). A by-reference parameter records no typed
    /// binding: it aliases the caller's variable, which other code can rebind
    /// while the body runs, and the declared type is checked only on entry.
    fn visit_parameter_type(&mut self, node: Node<'_>) {
        let Some(ty) = node.child_by_field_name("type") else {
            return;
        };
        self.visit(ty, false);
        if node.child_by_field_name("reference_modifier").is_some() {
            return;
        }
        let Some(type_node) = sole_class_type(ty) else {
            return;
        };
        let type_spelling = self.text(type_node);
        if let Some(name) = node.child_by_field_name("name") {
            self.bind_variable(name, Binding::Typed(type_spelling));
        }
    }

    /// Visit a `foreach` header: the iterated expression is read, the target is
    /// a binding.
    ///
    /// The `as` token separates the iterated expression from the target without
    /// guessing from node kinds, because a bare variable target and a bare
    /// variable iterable share the same kind.
    fn visit_foreach(&mut self, node: Node<'_>) {
        let mut cursor = node.walk();
        let as_byte = node
            .children(&mut cursor)
            .find(|child| child.kind() == "as")
            .map(|child| child.start_byte());
        let body_id = node.child_by_field_name("body").map(|body| body.id());
        let Some(as_byte) = as_byte else {
            // An unrecognized `foreach` shape: treat every variable mention as a
            // binding so an unknown form cannot leave a stale receiver intact.
            for child in named_children(node) {
                self.visit(child, true);
            }
            return;
        };
        // A by-reference target (`as &$v`, `as $k => &$v`) makes each element
        // of the iterated variable an alias, so the iterated variable is
        // treated as referenced too (AF3).
        let by_ref_target = named_children(node).into_iter().any(|child| {
            Some(child.id()) != body_id && child.start_byte() >= as_byte && contains_by_ref(child)
        });
        for child in named_children(node) {
            if Some(child.id()) == body_id || child.start_byte() < as_byte {
                self.visit(child, false);
                if by_ref_target && Some(child.id()) != body_id {
                    self.mark_referenced(child);
                }
            } else {
                self.visit(child, true);
            }
        }
    }

    /// Visit a `catch` clause: the caught exception binds its variable.
    fn visit_catch(&mut self, node: Node<'_>) {
        if let Some(ty) = node.child_by_field_name("type") {
            self.visit(ty, false);
        }
        if let Some(name) = node.child_by_field_name("name") {
            self.visit(name, true);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, false);
        }
    }

    /// Visit a closure's `use (...)` list.
    ///
    /// A by-reference capture lets the closure rebind the outer variable, so it
    /// is recorded as a binding. A by-value capture reads a copy and cannot
    /// rebind the outer variable, so it is left alone.
    fn visit_closure_uses(&mut self, node: Node<'_>) {
        for child in named_children(node) {
            if child.kind() == "by_ref" {
                self.visit(child, true);
            }
        }
    }

    fn visit_function_call(&mut self, node: Node<'_>) {
        self.record_call_site(node);
        let function = node.child_by_field_name("function");
        let mut callee: Option<String> = None;
        let mut dynamic = false;
        if let Some(function) = function {
            match function.kind() {
                "name" | "qualified_name" | "relative_name" => {
                    callee = Some(self.text(function));
                    self.push_use(function, RefKind::Call, None, UseHint::Unresolved);
                }
                _ => {
                    // A dynamic callee (`$fn(...)`, `($expr)(...)`) can name any
                    // function, including `extract`; with an argument it can
                    // rebind any local, so the scope is unanalysable (T21b).
                    callee = Some(self.text(function));
                    dynamic = true;
                    self.visit(function, false);
                }
            }
        }
        if let Some(callee) = callee.as_deref()
            && is_unbounded_builtin(callee)
        {
            self.mark_unanalysable(node);
            // `eval`'d code inside a function may declare any name `global`
            // (AF3). `extract` writes only the caller's locals.
            if callee.trim_start_matches('\\').eq_ignore_ascii_case("eval") {
                self.note_dynamic_global_write(node);
            }
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            if dynamic && has_argument(arguments) {
                self.mark_unanalysable(node);
            }
            if let Some(callee) = callee.as_deref() {
                self.record_call_args(arguments, callee, CallArgKind::Function, None);
            }
            self.visit(arguments, false);
        }
    }

    fn visit_member_call(&mut self, node: Node<'_>) {
        self.record_call_site(node);
        let object = node.child_by_field_name("object");
        let mut callee: Option<String> = None;
        let mut receiver: Option<CallReceiver> = None;
        if let Some(name) = node.child_by_field_name("name") {
            callee = Some(self.text(name));
            if name.kind() == "name" {
                let receiver_text = object.map(|object| self.text(object));
                let hint = object
                    .map(|object| self.receiver_hint(object))
                    .unwrap_or(UseHint::Unresolved);
                receiver = Some(call_receiver_from_hint(&hint, receiver_text.as_deref()));
                self.push_use(name, RefKind::Call, receiver_text, hint);
            } else {
                // A dynamic member name cannot be resolved to one declaration.
                receiver = Some(CallReceiver::Unknown);
            }
        }
        if let Some(object) = object {
            self.visit(object, false);
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            if let (Some(callee), Some(receiver)) = (callee.as_deref(), receiver) {
                self.record_call_args(arguments, callee, CallArgKind::Method, Some(receiver));
            }
            self.visit(arguments, false);
        }
    }

    fn visit_scoped_call(&mut self, node: Node<'_>) {
        self.record_call_site(node);
        let scope = node.child_by_field_name("scope");
        let mut callee: Option<String> = None;
        let mut receiver: Option<CallReceiver> = None;
        if let Some(name) = node.child_by_field_name("name") {
            callee = Some(self.text(name));
            if name.kind() == "name" {
                let receiver_text = scope.map(|scope| self.text(scope));
                let hint = scope
                    .map(|scope| self.scope_hint(scope))
                    .unwrap_or(UseHint::Unresolved);
                receiver = Some(self.call_receiver_from_scope(scope, &hint));
                self.push_use(name, RefKind::Call, receiver_text, hint);
            } else {
                receiver = Some(CallReceiver::Unknown);
            }
        }
        if let Some(scope) = scope {
            self.visit_scope(scope);
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            if let (Some(callee), Some(receiver)) = (callee.as_deref(), receiver) {
                self.record_call_args(arguments, callee, CallArgKind::StaticMethod, Some(receiver));
            }
            self.visit(arguments, false);
        }
    }

    /// The class a static-call scope names, for call-argument adjudication.
    fn call_receiver_from_scope(&self, scope: Option<Node<'_>>, hint: &UseHint) -> CallReceiver {
        let Some(scope) = scope else {
            return CallReceiver::Unknown;
        };
        let text = self.text(scope);
        let relative = scope.kind() == "relative_scope"
            || (scope.kind() == "name" && is_relative_keyword(&text));
        if relative {
            // Inside an anonymous class, `self` and `static` name that
            // anonymous class, which is not an indexed symbol (AF4).
            if self.in_anonymous_class() {
                return CallReceiver::Unknown;
            }
            return match text.to_ascii_lowercase().as_str() {
                "self" | "static" => CallReceiver::SelfClass,
                // `parent` names an ancestor v0.1 does not traverse.
                _ => CallReceiver::Unknown,
            };
        }
        match scope.kind() {
            "name" | "qualified_name" | "relative_name" => CallReceiver::Class { spelling: text },
            _ => call_receiver_from_hint(hint, Some(&text)),
        }
    }

    /// Records one [`CallArg`] per positional argument that passes a bare
    /// variable to `callee`, so the resolver can decide whether the callee's
    /// parameter rebinds it (T21b).
    ///
    /// An argument written with an explicit `&` is already recorded as an
    /// ordinary rebinding (T21a) and is skipped. A named argument cannot be
    /// mapped to a parameter position without the declaration order, so it is
    /// recorded with no position and the resolver suppresses.
    fn record_call_args(
        &mut self,
        arguments: Node<'_>,
        callee: &str,
        kind: CallArgKind,
        receiver: Option<CallReceiver>,
    ) {
        let scope_key = self.scope_key_for_node(arguments);
        let mut position: u32 = 0;
        for argument in named_children(arguments) {
            if argument.kind() != "argument" {
                continue;
            }
            let named = argument.child_by_field_name("name").is_some();
            let explicit_ref = argument.child_by_field_name("reference_modifier").is_some();
            let this_position = position;
            position += 1;
            if explicit_ref {
                continue;
            }
            let label = argument.child_by_field_name("name").map(|node| node.id());
            let value = named_children(argument)
                .into_iter()
                .find(|child| Some(child.id()) != label && child.kind() != "reference_modifier");
            let Some(value) = value.filter(|value| value.kind() == "variable_name") else {
                continue;
            };
            let Ok(span) = Span::new(value.start_byte() as u32, value.end_byte() as u32) else {
                continue;
            };
            let fact = CallArg {
                variable: self.text(value),
                callee: callee.to_string(),
                kind,
                position: (!named).then_some(this_position),
                receiver: receiver.clone(),
                span,
            };
            self.scope_facts
                .entry(scope_key.clone())
                .or_default()
                .call_args
                .push(fact);
        }
    }

    /// Records that `node` has a reference taken to it (AF3).
    ///
    /// A bare variable becomes an alias, so any later write through the other
    /// name rebinds it; it is recorded as a rebinding. A `$GLOBALS` element or
    /// `$GLOBALS` itself lets this scope rebind that global. Any other
    /// expression (a property, an element of an ordinary array) does not
    /// rebind a local variable and records nothing.
    fn mark_referenced(&mut self, node: Node<'_>) {
        let mut root = node;
        let mut key: Option<Node<'_>> = None;
        while root.kind() == "subscript_expression" {
            let children = named_children(root);
            let Some(base) = children.first().copied() else {
                return;
            };
            key = children.get(1).copied();
            root = base;
        }
        if root.kind() != "variable_name" {
            return;
        }
        if self.text(root) == "$GLOBALS" {
            if root.id() == node.id() {
                self.note_dynamic_global_write(node);
            } else {
                self.note_globals_key(node, key);
            }
        } else if root.id() == node.id() {
            self.bind_variable(root, Binding::Other);
        }
    }

    /// The key of the scope that owns `node` when it is a function-like body
    /// rather than a global scope (AF3).
    ///
    /// Code at a file's or namespace block's top level runs only when that
    /// file runs, never because file-scope code elsewhere called a function,
    /// so only a function-like body's `global` facts can rebind another file's
    /// globals across a call. (A file included from inside a function runs in
    /// that function's scope; the include itself is recorded as a dynamic
    /// global write.)
    fn function_scope_key(&self, node: Node<'_>) -> Option<String> {
        let scope_key = self.scope_key_for_node(node);
        (!is_global_scope_key(&scope_key)).then_some(scope_key)
    }

    /// Records that the function-like scope owning `node` can rebind the
    /// global variable `name` (with its `$`) (AF3).
    fn note_global_name(&mut self, node: Node<'_>, name: String) {
        if let Some(scope_key) = self.function_scope_key(node) {
            self.scope_facts
                .entry(scope_key)
                .or_default()
                .global_names
                .push(name);
        }
    }

    /// Records that the function-like scope owning `node` can rebind a global
    /// variable it does not name (AF3).
    fn note_dynamic_global_write(&mut self, node: Node<'_>) {
        if let Some(scope_key) = self.function_scope_key(node) {
            self.scope_facts
                .entry(scope_key)
                .or_default()
                .dynamic_global_write = true;
        }
    }

    /// Records a write or reference through `$GLOBALS[key]` (AF3): a literal
    /// string key names one global, anything else is dynamic.
    fn note_globals_key(&mut self, node: Node<'_>, key: Option<Node<'_>>) {
        match key.and_then(|key| literal_string(key, self.source)) {
            Some(name) => self.note_global_name(node, format!("${name}")),
            None => self.note_dynamic_global_write(node),
        }
    }

    /// Records one explicit call in a global scope (AF3). A call in a
    /// function-like body cannot rebind that body's locals through `global`,
    /// so only global scopes keep call sites.
    fn record_call_site(&mut self, node: Node<'_>) {
        let scope_key = self.scope_key_for_node(node);
        if !is_global_scope_key(&scope_key) {
            return;
        }
        let Ok(span) = Span::new(node.start_byte() as u32, node.end_byte() as u32) else {
            return;
        };
        self.scope_facts
            .entry(scope_key)
            .or_default()
            .call_sites
            .push(span);
    }

    /// Marks the scope that owns `node` as unanalysable (T21b).
    fn mark_unanalysable(&mut self, node: Node<'_>) {
        let scope_key = self.scope_key_for_node(node);
        self.scope_facts.entry(scope_key).or_default().unanalysable = true;
    }

    /// Visit `new Class(...)`: the class is a type use, and the arguments are
    /// code and call arguments of `Class::__construct` (AF3).
    ///
    /// The pinned grammar gives `object_creation_expression` no `arguments`
    /// field, so the arguments are found by kind; before AF3 they were never
    /// walked. An anonymous class carries its arguments inside the
    /// `anonymous_class` node; its constructor is not an indexed symbol, so
    /// its arguments are recorded with an unknown receiver.
    ///
    /// AF4: the anonymous class body is walked after the arguments, which
    /// PHP evaluates in the enclosing scope. Its uses keep the nearest named
    /// container (spec §10.1), but `$this`, `self`, and `static` inside it name
    /// the anonymous class, so they record no receiver evidence.
    fn visit_object_creation(&mut self, node: Node<'_>) {
        self.record_call_site(node);
        let class = object_creation_class(node);
        if let Some(class) = class
            && matches!(class.kind(), "name" | "qualified_name" | "relative_name")
        {
            self.push_use(class, RefKind::Type, None, UseHint::Unresolved);
        }
        let anonymous = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "anonymous_class");
        let arguments = named_children(anonymous.unwrap_or(node))
            .into_iter()
            .find(|child| child.kind() == "arguments");
        if let Some(arguments) = arguments {
            let receiver = match (anonymous, class) {
                (None, Some(class)) => self.constructor_receiver(class),
                _ => CallReceiver::Unknown,
            };
            self.record_call_args(
                arguments,
                "__construct",
                CallArgKind::Constructor,
                Some(receiver),
            );
            self.visit(arguments, false);
        }
        if let Some(anonymous) = anonymous {
            self.visit_class(anonymous);
        }
    }

    /// The class whose constructor `new <class>(...)` runs, for call-argument
    /// adjudication (AF3).
    ///
    /// `new self` runs the enclosing class's constructor. `new static` may
    /// run a subclass's and `new parent` an ancestor's, which v0.1 does not
    /// resolve, and a dynamic class expression names nothing.
    fn constructor_receiver(&self, class: Node<'_>) -> CallReceiver {
        if !matches!(class.kind(), "name" | "qualified_name" | "relative_name") {
            return CallReceiver::Unknown;
        }
        let spelling = self.text(class);
        match spelling.to_ascii_lowercase().as_str() {
            // Inside an anonymous class `new self` runs the anonymous class's
            // constructor, which is not an indexed symbol (AF4).
            "self" if self.in_anonymous_class() => CallReceiver::Unknown,
            "self" => CallReceiver::SelfClass,
            "static" | "parent" => CallReceiver::Unknown,
            _ => CallReceiver::Class { spelling },
        }
    }

    fn visit_member_access(&mut self, node: Node<'_>, write: bool) {
        let object = node.child_by_field_name("object");
        if let Some(name) = node.child_by_field_name("name")
            && name.kind() == "name"
        {
            let receiver = object.map(|object| self.text(object));
            let hint = object
                .map(|object| self.receiver_hint(object))
                .unwrap_or(UseHint::Unresolved);
            let kind = if write { RefKind::Write } else { RefKind::Read };
            self.push_use(name, kind, receiver, hint);
        }
        if let Some(object) = object {
            self.visit(object, false);
        }
    }

    fn visit_scoped_property(&mut self, node: Node<'_>, write: bool) {
        let scope = node.child_by_field_name("scope");
        if let Some(scope) = scope {
            self.visit_scope(scope);
        }
        if let Some(name) = node.child_by_field_name("name")
            && name.kind() == "variable_name"
        {
            let receiver = scope.map(|scope| self.text(scope));
            let hint = scope
                .map(|scope| self.scope_hint(scope))
                .unwrap_or(UseHint::Unresolved);
            let kind = if write { RefKind::Write } else { RefKind::Read };
            self.push_use(name, kind, receiver, hint);
        }
    }

    fn visit_class_constant(&mut self, node: Node<'_>) {
        let children = named_children(node);
        if children.len() < 2 {
            return;
        }
        let scope = children[0];
        let name = children[children.len() - 1];
        // `Foo::BAR` and `Foo::class` are type uses of `Foo` (AF4); an
        // expression scope is walked as code (T36d).
        self.visit_scope(scope);
        // `::class` is the class-name literal, not a member.
        if name.kind() == "name" && self.text(name).eq_ignore_ascii_case("class") {
            return;
        }
        if name.kind() == "name" || name.kind() == "variable_name" {
            let receiver = Some(self.text(scope));
            let hint = self.scope_hint(scope);
            let before = self.uses.len();
            self.push_use(name, RefKind::Read, receiver, hint);
            // A class-constant read and an instance property read are otherwise
            // recorded identically, so the use's scope records which uses name
            // a constant (AF2).
            if self.uses.len() > before
                && let Some(recorded) = self.uses.last()
            {
                let (scope_key, span) = (recorded.scope_key.clone(), recorded.span);
                self.scope_facts
                    .entry(scope_key)
                    .or_default()
                    .class_constant_accesses
                    .push(span);
            }
        }
    }

    fn visit_assignment(&mut self, node: Node<'_>) {
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        // A non-variable left is a binding target (a destructuring list, a
        // subscript, or a member). Visiting it as a write records any variable
        // it rebinds. A plain `$x` left is handled below so its own name is not
        // recorded twice.
        if let Some(left) = left
            && left.kind() != "variable_name"
        {
            self.visit(left, true);
        }
        if let Some(right) = right {
            self.visit(right, false);
        }
        let (Some(left), Some(right)) = (left, right) else {
            return;
        };
        if left.kind() != "variable_name" {
            return;
        }
        let binding = if right.kind() == "object_creation_expression" {
            object_creation_class(right)
                .filter(|class| matches!(class.kind(), "name" | "qualified_name" | "relative_name"))
                // Inside an anonymous class, `new self` and `new static` name
                // the anonymous class, never the enclosing named one (AF4).
                .filter(|class| {
                    !(self.in_anonymous_class() && is_relative_keyword(&self.text(*class)))
                })
                .map(|class| Binding::New(self.text(class), right.end_byte() as u32))
        } else {
            None
        };
        self.bind_variable(left, binding.unwrap_or(Binding::Other));
    }

    /// Record one use, resolving its container and lexical scope.
    fn push_use(
        &mut self,
        node: Node<'_>,
        ref_kind: RefKind,
        receiver: Option<String>,
        hint: UseHint,
    ) {
        if self.use_limit_exceeded {
            return;
        }
        let Ok(span) = Span::new(node.start_byte() as u32, node.end_byte() as u32) else {
            return;
        };
        if self.uses.len() as u64 >= self.max_uses {
            self.use_limit_exceeded = true;
            return;
        }
        let containing = containing_symbol(self.symbols, span.start_byte(), span.end_byte());
        let scope_key = self.scope_key(containing, span.start_byte());
        self.ensure_scope(&scope_key);
        self.uses.push(ExtractedUse {
            spelling: self.text(node),
            ref_kind,
            span,
            containing_symbol_index: containing,
            receiver,
            scope_key,
            hint,
        });
    }

    /// The scope key for a position at `byte` whose innermost named container
    /// is `containing`.
    ///
    /// A position with no named container belongs to the namespace block that
    /// owns `byte`, so a top-level closure's key carries that block's prefix
    /// and chains to its imports and namespace (AF1).
    fn scope_key(&self, containing: Option<usize>, byte: u32) -> String {
        let container = match containing {
            Some(index) => index.to_string(),
            None => block_prefix(self.layout, byte),
        };
        match self.body_stack.last().copied().flatten() {
            Some(ordinal) => format!("{container}:{ordinal}"),
            None => format!("{container}:file"),
        }
    }

    /// Derive the receiver hint for a member/static receiver expression.
    fn receiver_hint(&self, node: Node<'_>) -> UseHint {
        match node.kind() {
            "relative_scope" => self.relative_scope_hint(),
            "variable_name" => {
                let text = self.text(node);
                if text == "$this" {
                    // `$this` inside an anonymous class is the anonymous
                    // instance, never the enclosing named class (AF4).
                    if self.in_anonymous_class() {
                        return UseHint::Unresolved;
                    }
                    return UseHint::This;
                }
                match self.scopes.last().and_then(|scope| scope.get(&text)) {
                    Some(Binding::New(class, _)) => UseHint::NewExpr {
                        class_spelling: class.clone(),
                        use_block: enclosing_block_start(node),
                    },
                    Some(Binding::Typed(ty)) => UseHint::Typed {
                        type_spelling: ty.clone(),
                        origin: TypedOrigin::Parameter,
                    },
                    _ => UseHint::Unresolved,
                }
            }
            "member_access_expression" | "nullsafe_member_access_expression" => {
                let Some(object) = node.child_by_field_name("object") else {
                    return UseHint::Unresolved;
                };
                if self.text(object) != "$this" {
                    return UseHint::Unresolved;
                }
                let Some(name) = node.child_by_field_name("name") else {
                    return UseHint::Unresolved;
                };
                if name.kind() != "name" {
                    return UseHint::Unresolved;
                }
                let property = self.text(name);
                let Some((class_id, _)) = self.class_stack.last() else {
                    return UseHint::Unresolved;
                };
                match self
                    .property_types
                    .get(class_id)
                    .and_then(|properties| properties.get(&property))
                {
                    Some(ty) => UseHint::Typed {
                        type_spelling: ty.clone(),
                        origin: TypedOrigin::Property,
                    },
                    None => UseHint::Unresolved,
                }
            }
            _ => UseHint::Unresolved,
        }
    }

    /// Whether the innermost enclosing class-like is an anonymous class (AF4).
    fn in_anonymous_class(&self) -> bool {
        self.class_stack
            .last()
            .is_some_and(|(_, anonymous)| *anonymous)
    }

    /// The hint for `self`, `static`, or `parent` before `::` (AF4).
    ///
    /// Inside an anonymous class each names the anonymous class (or its
    /// ancestor), not the enclosing named class, so no evidence is recorded.
    fn relative_scope_hint(&self) -> UseHint {
        if self.in_anonymous_class() {
            UseHint::Unresolved
        } else {
            UseHint::SelfOrStatic
        }
    }

    /// The receiver hint for the scope of `Scope::member` (AF4).
    ///
    /// A class named explicitly is [`UseHint::NamedClass`]; `self`, `static`,
    /// and `parent` keep their relative-scope hint; any other expression is
    /// hinted like a member receiver.
    fn scope_hint(&self, scope: Node<'_>) -> UseHint {
        match scope.kind() {
            "name" | "qualified_name" | "relative_name" => {
                let text = self.text(scope);
                if scope.kind() == "name" && is_relative_keyword(&text) {
                    self.relative_scope_hint()
                } else {
                    UseHint::NamedClass {
                        class_spelling: text,
                    }
                }
            }
            _ => self.receiver_hint(scope),
        }
    }

    /// Records the class named explicitly before `::` as a type use (AF4).
    ///
    /// Returns whether `scope` was such a class name. `self`, `static`,
    /// `parent`, and any expression scope record nothing here.
    fn record_scope_class(&mut self, scope: Node<'_>) -> bool {
        if !matches!(scope.kind(), "name" | "qualified_name" | "relative_name") {
            return false;
        }
        if scope.kind() == "name" && is_relative_keyword(&self.text(scope)) {
            return false;
        }
        self.push_use(scope, RefKind::Type, None, UseHint::Unresolved);
        true
    }

    /// Handles the scope before `::` in a static call, a class-constant
    /// access, or a static property access.
    ///
    /// A class named explicitly is a type use of that class (AF4); `self`,
    /// `static`, and `parent` name no class by themselves. Any other scope is
    /// an expression (`$obj`, `$this->f()`, `(expr)`) whose own uses are
    /// walked as code (T36d); the expression itself is never a type use.
    fn visit_scope(&mut self, scope: Node<'_>) {
        if self.record_scope_class(scope) {
            return;
        }
        if !matches!(
            scope.kind(),
            "name" | "qualified_name" | "relative_name" | "relative_scope"
        ) {
            self.visit(scope, false);
        }
    }

    fn bind_variable(&mut self, name: Node<'_>, binding: Binding) {
        if name.kind() != "variable_name" {
            return;
        }
        let text = self.text(name);
        let scope_key = self.scope_key_for_node(name);
        if let Ok(span) = Span::new(name.start_byte() as u32, name.end_byte() as u32) {
            match &binding {
                Binding::Typed(type_spelling) => {
                    self.scope_facts
                        .entry(scope_key)
                        .or_default()
                        .typed_bindings
                        .push(TypedBinding {
                            variable: text.clone(),
                            type_spelling: type_spelling.clone(),
                            span,
                        });
                }
                Binding::New(class_spelling, value_end) => {
                    self.scope_facts
                        .entry(scope_key)
                        .or_default()
                        .new_bindings
                        .push(NewBinding {
                            variable: text.clone(),
                            class_spelling: class_spelling.clone(),
                            span,
                            direct_new: true,
                            block: enclosing_block_start(name),
                            value_end: Some(*value_end),
                        });
                }
                Binding::Other => {
                    self.scope_facts
                        .entry(scope_key)
                        .or_default()
                        .new_bindings
                        .push(NewBinding {
                            variable: text.clone(),
                            class_spelling: String::new(),
                            span,
                            direct_new: false,
                            block: enclosing_block_start(name),
                            value_end: None,
                        });
                }
            }
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(text, binding);
        }
    }

    /// The current lexical scope key for a use or declaration node.
    fn scope_key_for_node(&self, node: Node<'_>) -> String {
        let containing = containing_symbol(
            self.symbols,
            node.start_byte() as u32,
            node.end_byte() as u32,
        );
        self.scope_key(containing, node.start_byte() as u32)
    }

    /// Ensures a scope entry exists so every scope that owns a use is persisted.
    fn ensure_scope(&mut self, scope_key: &str) {
        self.scope_facts.entry(scope_key.to_string()).or_default();
    }

    /// Adds one import fact to the scope that owns the `use` binding.
    fn add_import_fact(&mut self, scope_key: &str, import: ScopeImport) {
        self.scope_facts
            .entry(scope_key.to_string())
            .or_default()
            .imports
            .push(import);
    }

    /// Freezes the walker's scope map into owned, parent-linked scopes.
    fn finish_scopes(&mut self) -> Vec<ExtractedScope> {
        let mut scope_facts = std::mem::take(&mut self.scope_facts);
        let mut parameter_lists = std::mem::take(&mut self.parameter_lists);
        let mut supertypes = std::mem::take(&mut self.supertypes);
        // Every top-level scope always exists so imports and top-level
        // declarations have a home even in a file with no uses: the file scope
        // for a file with no namespace, otherwise one scope per namespace
        // block. A namespaced file in which no position can be attributed has
        // only the orphan scope.
        if !self.layout.is_namespaced() {
            scope_facts.entry(FILE_SCOPE_KEY.to_string()).or_default();
        } else if self.layout.block_count() == 0 {
            scope_facts
                .entry(format!("{ORPHAN_PREFIX}:file"))
                .or_default();
        } else {
            for block in 0..self.layout.block_count() {
                scope_facts.entry(format!("ns{block}:file")).or_default();
            }
        }
        for (index, symbol) in self.symbols.iter().enumerate() {
            let scope_key = match symbol.parent_index {
                Some(parent) => format!("{parent}:file"),
                None => format!(
                    "{}:file",
                    block_prefix(self.layout, symbol.span.start_byte())
                ),
            };
            let facts = scope_facts.entry(scope_key).or_default();
            facts.declares.push(index);
            // A declaration's parameter list lives beside its `declares` entry
            // so persistence can rewrite the index to the canonical ID (AF3).
            if let Some(by_ref) = parameter_lists.remove(&index) {
                facts.parameter_lists.push(ParameterList {
                    symbol: index,
                    by_ref,
                });
            }
            // Likewise a class-like's declared supertypes (T36d).
            if let Some(list) = supertypes.remove(&index) {
                facts.supertypes.extend(list);
            }
        }
        share_top_level_variables(&mut scope_facts);
        // Every lookup depends on the namespace, so a scope no block owns is
        // flagged and the resolver records no binding under it.
        for (scope_key, facts) in scope_facts.iter_mut() {
            if key_prefix(scope_key) == ORPHAN_PREFIX {
                facts.namespace_unattributed = true;
            }
            facts.global_scope = is_global_scope_key(scope_key);
            facts.class_constant_accesses.sort();
            facts.class_constant_accesses.dedup();
            facts.call_sites.sort();
            facts.call_sites.dedup();
            facts.global_names.sort();
            facts.global_names.dedup();
        }
        scope_facts
            .into_iter()
            .map(|(scope_key, facts)| {
                let parent_scope_key = parent_scope_key(&scope_key, self.symbols, self.layout);
                ExtractedScope {
                    scope_key,
                    parent_scope_key,
                    facts,
                }
            })
            .collect()
    }

    /// Record the import bindings of one `use` declaration.
    fn handle_use(&mut self, node: Node<'_>) {
        let declaration_kind = import_kind(node.child_by_field_name("type"));
        let prefix = node
            .named_child(0)
            .filter(|child| child.kind() == "namespace_name")
            .map(|child| self.text(child));

        let mut clauses: Vec<Node> = Vec::new();
        for child in named_children(node) {
            match child.kind() {
                "namespace_use_clause" => clauses.push(child),
                "namespace_use_group" => {
                    for clause in named_children(child) {
                        if clause.kind() == "namespace_use_clause" {
                            clauses.push(clause);
                        }
                    }
                }
                _ => {}
            }
        }
        for clause in clauses {
            self.handle_use_clause(clause, prefix.as_deref(), declaration_kind);
        }
    }

    fn handle_use_clause(
        &mut self,
        clause: Node<'_>,
        prefix: Option<&str>,
        declaration_kind: Option<ImportKind>,
    ) {
        let kind = import_kind(clause.child_by_field_name("type"))
            .or(declaration_kind)
            .unwrap_or(ImportKind::Class);
        let Some(target) = clause
            .named_child(0)
            .filter(|child| matches!(child.kind(), "name" | "qualified_name"))
        else {
            return;
        };
        let target_text = self.text(target);
        let target_qualified = match prefix {
            Some(prefix) => format!("{prefix}\\{target_text}"),
            None => target_text,
        };
        let alias = clause.child_by_field_name("alias");
        let binding = alias.or_else(|| last_name_node(target));
        let Some(binding) = binding else {
            return;
        };
        let spelling_alias = self.text(binding);
        let Ok(span) = Span::new(binding.start_byte() as u32, binding.end_byte() as u32) else {
            return;
        };
        self.imports.push(ExtractedImport {
            spelling_alias: spelling_alias.clone(),
            target_qualified: target_qualified.clone(),
            kind,
            span,
        });
        let scope_key = self.scope_key_for_node(binding);
        self.add_import_fact(
            &scope_key,
            ScopeImport {
                alias: spelling_alias.clone(),
                target_qualified,
                kind,
                span,
            },
        );
        self.push_use(binding, RefKind::Import, None, UseHint::Unresolved);
    }

    fn text(&self, node: Node<'_>) -> String {
        String::from_utf8_lossy(&self.source[node.start_byte()..node.end_byte()]).into_owned()
    }
}

/// The innermost named container whose span contains `[start, end)`.
fn containing_symbol(symbols: &[ExtractedSymbol], start: u32, end: u32) -> Option<usize> {
    let mut best: Option<(usize, u32)> = None;
    for (index, symbol) in symbols.iter().enumerate() {
        if !is_named_container(symbol.kind) {
            continue;
        }
        let symbol_start = symbol.span.start_byte();
        let symbol_end = symbol.span.end_byte();
        if symbol_start <= start && end <= symbol_end {
            let length = symbol_end - symbol_start;
            let replace = match best {
                None => true,
                Some((_, best_length)) => length < best_length,
            };
            if replace {
                best = Some((index, length));
            }
        }
    }
    best.map(|(index, _)| index)
}

fn is_named_container(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Class
            | SymbolKind::Interface
            | SymbolKind::Enum
    )
}

/// The scope-key prefix for a position with no named container (AF1).
///
/// `top` in a file with no namespace, `ns{block}` inside a namespace block,
/// and `orphan` where no single block owns the position.
fn block_prefix(layout: &NamespaceLayout, byte: u32) -> String {
    match layout.attribution(byte) {
        Attribution::NoNamespace => "top".to_string(),
        Attribution::Block(block) => format!("ns{block}"),
        Attribution::Unattributed => ORPHAN_PREFIX.to_string(),
    }
}

/// The container part of a scope key, before the first `:`.
fn key_prefix(scope_key: &str) -> &str {
    scope_key.split(':').next().unwrap_or(scope_key)
}

/// Whether a scope-key container names a top-level (block) prefix rather than
/// a symbol index.
fn is_block_prefix(container: &str) -> bool {
    container == "top"
        || container == ORPHAN_PREFIX
        || container
            .strip_prefix("ns")
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Makes top-level variable facts suppress across namespace blocks (AF1).
///
/// PHP namespaces do not scope variables: top-level code in every block of a
/// file shares one set of globals. Each block's top-level scope keeps its own
/// facts, so a `new` receiver and its class spelling resolve in the block they
/// appear in, and gains every other block's top-level assignments and
/// call arguments as non-direct rebinding entries plus its `unanalysable`
/// flag. A variable assigned in more than one block therefore never binds, as
/// it did not before blocks had separate scopes.
fn share_top_level_variables(scope_facts: &mut BTreeMap<String, ScopeFacts>) {
    let roots: Vec<String> = scope_facts
        .keys()
        .filter(|key| key.ends_with(":file") && is_block_prefix(key_prefix(key)))
        .cloned()
        .collect();
    if roots.len() < 2 {
        return;
    }
    // A `goto` in any block can jump into another (AF3), so each block also
    // gains every other block's call sites and `goto` flag.
    #[allow(clippy::type_complexity)]
    let mut shared: BTreeMap<String, (Vec<NewBinding>, bool, Vec<Span>, bool)> = BTreeMap::new();
    for root in &roots {
        let mut rebindings: Vec<NewBinding> = Vec::new();
        let mut unanalysable = false;
        let mut call_sites: Vec<Span> = Vec::new();
        let mut goto_present = false;
        for other in roots.iter().filter(|other| *other != root) {
            let facts = &scope_facts[other];
            unanalysable |= facts.unanalysable;
            call_sites.extend(facts.call_sites.iter().copied());
            goto_present |= facts.goto_present;
            for binding in &facts.new_bindings {
                rebindings.push(NewBinding {
                    variable: binding.variable.clone(),
                    class_spelling: String::new(),
                    span: binding.span,
                    direct_new: false,
                    block: binding.block,
                    value_end: None,
                });
            }
            for arg in &facts.call_args {
                rebindings.push(NewBinding {
                    variable: arg.variable.clone(),
                    class_spelling: String::new(),
                    span: arg.span,
                    direct_new: false,
                    block: None,
                    value_end: None,
                });
            }
        }
        shared.insert(
            root.clone(),
            (rebindings, unanalysable, call_sites, goto_present),
        );
    }
    for (root, (rebindings, unanalysable, call_sites, goto_present)) in shared {
        let facts = scope_facts.entry(root).or_default();
        facts.new_bindings.extend(rebindings);
        facts.unanalysable |= unanalysable;
        facts.call_sites.extend(call_sites);
        facts.goto_present |= goto_present;
    }
}

/// The enclosing scope key of `scope_key`.
///
/// A scope key is `{prefix}:file` (a top-level scope), `{prefix}:{ordinal}` (a
/// top-level closure or arrow function body), `{symbol_index}:file` (a
/// class-like body or an otherwise unscoped position), or
/// `{symbol_index}:{body_ordinal}` (a function body). `prefix` is `top`,
/// `ns{block}`, or `orphan` (see [`block_prefix`]).
///
/// The parent of a top-level closure is its block's top-level scope. The
/// parent of a symbol-owned scope is the scope that declares that symbol: the
/// top-level scope of its namespace block for a top-level symbol, or the
/// parent's `:file` scope for a member. Top-level scopes have no parent.
fn parent_scope_key(
    scope_key: &str,
    symbols: &[ExtractedSymbol],
    layout: &NamespaceLayout,
) -> Option<String> {
    let (container, rest) = scope_key.split_once(':')?;
    if is_block_prefix(container) {
        return (rest != "file").then(|| format!("{container}:file"));
    }
    let index: usize = container.parse().ok()?;
    let symbol = symbols.get(index)?;
    Some(match symbol.parent_index {
        Some(parent) => format!("{parent}:file"),
        None => format!("{}:file", block_prefix(layout, symbol.span.start_byte())),
    })
}

/// The class-like node named by an `object_creation_expression`.
fn object_creation_class(node: Node<'_>) -> Option<Node<'_>> {
    named_children(node)
        .into_iter()
        .find(|child| !matches!(child.kind(), "arguments" | "anonymous_class"))
}

/// Maps a member receiver's evidence hint to the class lookup a call argument
/// needs (T21b).
///
/// AF3: a hint that rests on a local variable (a `new` assignment or a typed
/// parameter) keeps the variable, so the resolver can refuse the hint when the
/// variable is not trustworthy, exactly as it would refuse the receiver's own
/// use. A typed property holds its type on every assignment and needs no
/// variable.
fn call_receiver_from_hint(hint: &UseHint, receiver: Option<&str>) -> CallReceiver {
    match hint {
        UseHint::NewExpr {
            class_spelling,
            use_block,
        } => match receiver {
            Some(variable) => CallReceiver::Variable {
                variable: variable.to_string(),
                evidence: ReceiverEvidence::New {
                    class_spelling: class_spelling.clone(),
                    use_block: *use_block,
                },
            },
            None => CallReceiver::Unknown,
        },
        UseHint::Typed {
            type_spelling,
            origin: TypedOrigin::Parameter,
        } => match receiver {
            Some(variable) => CallReceiver::Variable {
                variable: variable.to_string(),
                evidence: ReceiverEvidence::TypedParameter {
                    type_spelling: type_spelling.clone(),
                },
            },
            None => CallReceiver::Unknown,
        },
        UseHint::Typed {
            type_spelling,
            origin: TypedOrigin::Property,
        } => CallReceiver::Class {
            spelling: type_spelling.clone(),
        },
        UseHint::This | UseHint::SelfOrStatic => CallReceiver::SelfClass,
        UseHint::NamedClass { class_spelling } => CallReceiver::Class {
            spelling: class_spelling.clone(),
        },
        // A variable annotation is TypeScript's (T43); PHP never records one.
        UseHint::Typed {
            origin: TypedOrigin::Variable,
            ..
        }
        | UseHint::Imported { .. }
        | UseHint::Unresolved => CallReceiver::Unknown,
    }
}

/// Whether `callee` names the unbounded global builtins `extract` or `eval`.
///
/// A namespaced function of the same short name is a different function and is
/// not this builtin, so only an unqualified or fully qualified global spelling
/// matches.
fn is_unbounded_builtin(callee: &str) -> bool {
    let name = callee.trim_start_matches('\\');
    !name.contains('\\') && matches!(name.to_ascii_lowercase().as_str(), "extract" | "eval")
}

/// Whether a scope spelling is `self`, `static`, or `parent`, which PHP
/// compares case-insensitively (AF4).
fn is_relative_keyword(text: &str) -> bool {
    matches!(
        text.to_ascii_lowercase().as_str(),
        "self" | "static" | "parent"
    )
}

/// Whether a `binary_expression` is an `instanceof` test (AF4).
fn is_instanceof(node: Node<'_>) -> bool {
    node.child_by_field_name("operator")
        .is_some_and(|operator| operator.kind().eq_ignore_ascii_case("instanceof"))
}

/// Whether an `arguments` node carries at least one argument.
fn has_argument(arguments: Node<'_>) -> bool {
    named_children(arguments)
        .iter()
        .any(|child| child.kind() == "argument")
}

/// The `function`/`const` token that selects a non-class import kind.
fn import_kind(node: Option<Node<'_>>) -> Option<ImportKind> {
    match node.map(|node| node.kind()) {
        Some("function") => Some(ImportKind::Function),
        Some("const") => Some(ImportKind::Const),
        _ => None,
    }
}

/// The last `name` descendant of an import target.
fn last_name_node(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "name" {
        return Some(node);
    }
    let mut found = None;
    for child in named_children(node) {
        if let Some(name) = last_name_node(child) {
            found = Some(name);
        }
    }
    found
}

/// The one class a declared type names, or `None` (AF3).
///
/// A plain `A` names `A`, and so does a nullable `?A`: calling a method on
/// null is an error, not a call to another target. A union, intersection, or
/// DNF type (`A|B`, `A&B`, `(A&B)|null`, and also `A|null`) names more than
/// one type or none uniquely, and a primitive type names no class, so each
/// yields `None`. The pre-AF3 helper walked a stack and returned the last
/// member of a union.
fn sole_class_type(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "named_type" => Some(node),
        "optional_type" => {
            let children = named_children(node);
            match children.as_slice() {
                [only] if only.kind() == "named_type" => Some(*only),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The by-reference flag of each parameter in a `formal_parameters` node, in
/// declaration order (AF3).
///
/// Read from the tree, so an attribute, a default value, or a comment cannot
/// shift or split the list: comments are skipped, and a promoted parameter is
/// by-reference when its name is a `by_ref` node. Any other unrecognized child
/// yields `None`, which the resolver treats as an unknown parameter list.
fn parameter_by_ref_flags(parameters: Node<'_>) -> Option<Vec<bool>> {
    let mut flags = Vec::new();
    for child in named_children(parameters) {
        match child.kind() {
            "comment" => {}
            "simple_parameter" | "variadic_parameter" => {
                flags.push(child.child_by_field_name("reference_modifier").is_some());
            }
            "property_promotion_parameter" => flags.push(
                child
                    .child_by_field_name("name")
                    .is_some_and(|name| name.kind() == "by_ref"),
            ),
            _ => return None,
        }
    }
    Some(flags)
}

/// Whether a scope key names a global scope: the top level of a file or of a
/// namespace block, whose variables are PHP globals (AF3).
fn is_global_scope_key(scope_key: &str) -> bool {
    scope_key
        .split_once(':')
        .is_some_and(|(container, rest)| rest == "file" && is_block_prefix(container))
}

/// Whether `node` is `$GLOBALS` or an element chain rooted at it.
fn globals_root(node: Node<'_>, source: &[u8]) -> bool {
    let mut root = node;
    while root.kind() == "subscript_expression" {
        let Some(base) = root.named_child(0) else {
            return false;
        };
        root = base;
    }
    root.kind() == "variable_name" && text_of(root, source) == "$GLOBALS"
}

/// Whether `node` is or contains a `by_ref` node.
fn contains_by_ref(node: Node<'_>) -> bool {
    node.kind() == "by_ref" || named_children(node).into_iter().any(contains_by_ref)
}

/// The value of a string literal with no interpolation or escape sequence,
/// or `None` for anything else.
fn literal_string(node: Node<'_>, source: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let children = named_children(node);
    if children
        .iter()
        .any(|child| child.kind() != "string_content")
    {
        return None;
    }
    Some(
        children
            .iter()
            .map(|child| text_of(*child, source))
            .collect(),
    )
}

/// Class-like node id -> property name (without `$`) -> written type spelling.
fn collect_property_types(
    root: Node<'_>,
    source: &[u8],
) -> HashMap<usize, HashMap<String, String>> {
    let mut types: HashMap<usize, HashMap<String, String>> = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "property_declaration" => {
                if let (Some(class_id), Some(type_field)) =
                    (enclosing_class_like(node), node.child_by_field_name("type"))
                    && let Some(type_node) = sole_class_type(type_field)
                {
                    let spelling = text_of(type_node, source);
                    for element in named_children(node) {
                        if element.kind() != "property_element" {
                            continue;
                        }
                        let Some(name) = element.child_by_field_name("name") else {
                            continue;
                        };
                        let property = text_of(name, source).trim_start_matches('$').to_string();
                        types
                            .entry(class_id)
                            .or_default()
                            .insert(property, spelling.clone());
                    }
                }
            }
            "property_promotion_parameter" => {
                if let (Some(class_id), Some(type_field)) =
                    (enclosing_class_like(node), node.child_by_field_name("type"))
                    && let (Some(type_node), Some(name)) = (
                        sole_class_type(type_field),
                        node.child_by_field_name("name"),
                    )
                {
                    let property = text_of(name, source).trim_start_matches('$').to_string();
                    types
                        .entry(class_id)
                        .or_default()
                        .insert(property, text_of(type_node, source));
                }
            }
            _ => {}
        }
        for child in named_children(node) {
            stack.push(child);
        }
    }
    types
}

/// The nearest enclosing class-like node id, if any.
fn enclosing_class_like(node: Node<'_>) -> Option<usize> {
    let mut current = node.parent();
    while let Some(candidate) = current {
        if matches!(
            candidate.kind(),
            "class_declaration"
                | "interface_declaration"
                | "trait_declaration"
                | "enum_declaration"
                | "anonymous_class"
        ) {
            return Some(candidate.id());
        }
        current = candidate.parent();
    }
    None
}

/// The start byte of the nearest enclosing control-flow block, or `None` at
/// the top level of the node's function body.
///
/// A braced block is identified by its `compound_statement`; a braceless body
/// falls back to the control-flow node itself. The function body's own
/// `compound_statement` is the top level and reports `None`, so a use outside a
/// conditional and an assignment inside it never share a block.
fn enclosing_block_start(node: Node<'_>) -> Option<u32> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if is_function_like(parent) {
            return None;
        }
        if parent.kind() == "compound_statement" {
            if parent.parent().is_some_and(is_function_like) {
                return None;
            }
            return Some(parent.start_byte() as u32);
        }
        if is_control_flow(parent) {
            return Some(parent.start_byte() as u32);
        }
        current = parent.parent();
    }
    None
}

/// Whether `node` introduces a conditional or loop body.
fn is_control_flow(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "if_statement"
            | "else_clause"
            | "else_if_clause"
            | "for_statement"
            | "foreach_statement"
            | "while_statement"
            | "do_statement"
            | "switch_statement"
            | "case_statement"
            | "default_statement"
            | "try_statement"
            | "catch_clause"
            | "finally_clause"
            | "match_expression"
            | "match_conditional_expression"
            | "match_default_expression"
            | "conditional_expression"
    )
}

/// Whether `node` opens a lexical function body.
fn is_function_like(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_definition" | "method_declaration" | "anonymous_function" | "arrow_function"
    )
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn text_of(node: Node<'_>, source: &[u8]) -> String {
    String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::FILE_SCOPE_KEY;
    use crate::{LanguageId, grammar};
    use rivet_core::ExtractedFile;
    use rivet_core::extract::{
        CallArgKind, CallReceiver, ExtractedScope, NewBinding, ReceiverEvidence, ScopeFacts,
        TypedOrigin, UseHint,
    };
    use tree_sitter::Parser;

    fn extract(source: &str) -> ExtractedFile {
        let mut parser = Parser::new();
        parser
            .set_language(&grammar(LanguageId::Php))
            .expect("pinned PHP grammar must load");
        let tree = parser
            .parse(source.as_bytes(), None)
            .expect("parser must return a tree");
        crate::php::extract(source.as_bytes(), &tree)
    }

    /// The file scope's facts, where a top-level snippet records its bindings.
    fn file_scope(file: &ExtractedFile) -> &ScopeFacts {
        &file
            .scopes
            .iter()
            .find(|scope| scope.scope_key == FILE_SCOPE_KEY)
            .expect("the file scope always exists")
            .facts
    }

    /// The number of binding facts recorded for `variable` in the file scope.
    fn binding_count(file: &ExtractedFile, variable: &str) -> usize {
        file.scopes
            .iter()
            .find(|scope| scope.scope_key == FILE_SCOPE_KEY)
            .map(|scope| {
                scope
                    .facts
                    .new_bindings
                    .iter()
                    .filter(|binding| binding.variable == variable)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Whether the persisted facts would let the `new`-receiver rule select a
    /// single direct-`new` assignment for `variable` in the file scope. This
    /// mirrors the resolver's `sole_assignment` check, so a `false` result is
    /// exactly "the rule records no binding".
    fn binds_direct_new(file: &ExtractedFile, variable: &str) -> bool {
        let Some(scope) = file
            .scopes
            .iter()
            .find(|scope| scope.scope_key == FILE_SCOPE_KEY)
        else {
            return false;
        };
        let mut found: Option<&NewBinding> = None;
        for binding in &scope.facts.new_bindings {
            if binding.variable != variable {
                continue;
            }
            if found.is_some() {
                return false;
            }
            found = Some(binding);
        }
        found.is_some_and(|binding| binding.direct_new)
    }

    /// A snippet that assigns `new Alpha()`, rebinds `$s`, then calls
    /// `$s->go()` must record the rebinding and never bind.
    fn assert_rebound(source: &str) {
        let file = extract(source);
        assert!(file.diagnostics.is_empty(), "snippet must parse: {source}");
        assert_eq!(
            binding_count(&file, "$s"),
            2,
            "the rebinding must be recorded: {source}"
        );
        assert!(
            !binds_direct_new(&file, "$s"),
            "a rebound receiver must record no binding: {source}"
        );
    }

    #[test]
    fn foreach_target_rebinding_records_nothing() {
        assert_rebound("<?php\n$s = new Alpha();\nforeach ($xs as $s) {}\n$s->go();\n");
    }

    #[test]
    fn destructuring_rebinding_records_nothing() {
        assert_rebound("<?php\n$s = new Alpha();\n[$a, $s] = $pair;\n$s->go();\n");
        assert_rebound("<?php\n$s = new Alpha();\nlist($a, $s) = $pair;\n$s->go();\n");
    }

    #[test]
    fn reference_assignment_rebinding_records_nothing() {
        assert_rebound("<?php\n$s = new Alpha();\n$s = &$o;\n$s->go();\n");
    }

    #[test]
    fn catch_parameter_rebinding_records_nothing() {
        assert_rebound("<?php\n$s = new Alpha();\ntry {} catch (\\Throwable $s) {}\n$s->go();\n");
    }

    #[test]
    fn by_reference_closure_capture_records_nothing() {
        assert_rebound(
            "<?php\n$s = new Alpha();\n$f = function () use (&$s) { $s = mk(); };\n$f();\n$s->go();\n",
        );
    }

    #[test]
    fn compound_assignment_rebinding_records_nothing() {
        assert_rebound("<?php\n$s = new Alpha();\n$s ??= mk();\n$s->go();\n");
        assert_rebound("<?php\n$s = new Alpha();\n$s .= 'x';\n$s->go();\n");
        assert_rebound("<?php\n$s = new Alpha();\n$s += 1;\n$s->go();\n");
    }

    #[test]
    fn update_expression_rebinding_records_nothing() {
        assert_rebound("<?php\n$s = new Alpha();\n$s++;\n$s->go();\n");
    }

    #[test]
    fn unset_rebinding_records_nothing() {
        assert_rebound("<?php\n$s = new Alpha();\nunset($s);\n$s->go();\n");
    }

    #[test]
    fn plain_reassignment_records_nothing() {
        // Control: the one form the walker already recorded stays unbound.
        assert_rebound("<?php\n$s = new Alpha();\n$s = mk();\n$s->go();\n");
    }

    #[test]
    fn safe_single_assignment_still_binds_direct_new() {
        let file = extract("<?php\n$s = new Alpha();\n$s->go();\n");
        assert!(file.diagnostics.is_empty());
        assert_eq!(binding_count(&file, "$s"), 1);
        assert!(binds_direct_new(&file, "$s"));
    }

    /// T21b cases 3-6: a construct whose effect on locals cannot be bounded
    /// marks the whole scope unanalysable.
    #[test]
    fn unanalysable_constructs_mark_the_scope() {
        for source in [
            "<?php\n$s = new Alpha();\n$$name = 1;\n$s->go();\n",
            "<?php\n$s = new Alpha();\n${$name} = 1;\n$s->go();\n",
            "<?php\n$s = new Alpha();\n$GLOBALS['s'] = 1;\n$s->go();\n",
            "<?php\n$s = new Alpha();\nextract($arr);\n$s->go();\n",
            "<?php\n$s = new Alpha();\neval('$x = 1;');\n$s->go();\n",
            "<?php\n$s = new Alpha();\ninclude 'other.php';\n$s->go();\n",
            "<?php\n$s = new Alpha();\nrequire_once 'other.php';\n$s->go();\n",
        ] {
            let file = extract(source);
            assert!(file.diagnostics.is_empty(), "snippet must parse: {source}");
            assert!(
                file_scope(&file).unanalysable,
                "the scope must be unanalysable: {source}"
            );
        }
    }

    /// A read of a dynamic variable or a read of `$GLOBALS` does not rebind
    /// anything, so it must not make the scope unanalysable.
    #[test]
    fn reads_do_not_mark_the_scope_unanalysable() {
        let file =
            extract("<?php\n$s = new Alpha();\n$v = $$name;\n$w = $GLOBALS['x'];\n$s->go();\n");
        assert!(file.diagnostics.is_empty());
        assert!(!file_scope(&file).unanalysable);
    }

    /// A namespaced function of the same short name is not the builtin.
    #[test]
    fn a_namespaced_extract_is_not_the_builtin() {
        let file = extract("<?php\n$s = new Alpha();\n\\Foo\\extract($arr);\n$s->go();\n");
        assert!(file.diagnostics.is_empty());
        assert!(!file_scope(&file).unanalysable);
    }

    /// T21b cases 1-2: a bare-variable positional argument is recorded with its
    /// callee and position for the resolver to adjudicate.
    #[test]
    fn call_arguments_record_the_variable_and_position() {
        let file = extract("<?php\n$s = new Alpha();\ntakesRef($s, $t);\n");
        assert!(file.diagnostics.is_empty());
        let args = &file_scope(&file).call_args;
        assert_eq!(args.len(), 2, "both arguments are recorded: {args:?}");
        assert_eq!(args[0].variable, "$s");
        assert_eq!(args[0].callee, "takesRef");
        assert_eq!(args[0].kind, CallArgKind::Function);
        assert_eq!(args[0].position, Some(0));
        assert_eq!(args[0].receiver, None);
        assert_eq!(args[1].variable, "$t");
        assert_eq!(args[1].position, Some(1));
    }

    /// A method call records the receiver evidence the resolver needs.
    #[test]
    fn method_call_arguments_record_the_receiver_hint() {
        let file = extract("<?php\n$svc = new Alpha();\n$svc->takesRef($s);\n");
        assert!(file.diagnostics.is_empty());
        let args = &file_scope(&file).call_args;
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].kind, CallArgKind::Method);
        assert_eq!(args[0].callee, "takesRef");
        // AF3: the class comes from `$svc`'s `new` hint, so the variable is
        // kept for the resolver to re-check.
        assert_eq!(
            args[0].receiver,
            Some(CallReceiver::Variable {
                variable: "$svc".to_string(),
                evidence: ReceiverEvidence::New {
                    class_spelling: "Alpha".to_string(),
                    use_block: None,
                },
            })
        );
    }

    /// A named argument has no mappable position and is recorded as unknown.
    #[test]
    fn named_arguments_record_no_position() {
        let file = extract("<?php\n$s = new Alpha();\ntakesRef(x: $s);\n");
        assert!(file.diagnostics.is_empty());
        let args = &file_scope(&file).call_args;
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].variable, "$s");
        assert_eq!(args[0].position, None);
    }

    /// An explicit `&$s` at the call site is already a T21a rebinding, so it is
    /// not duplicated as a call-argument candidate.
    #[test]
    fn explicit_by_reference_arguments_are_not_duplicated() {
        let file = extract("<?php\n$s = new Alpha();\ntakesRef(&$s);\n");
        assert!(file.diagnostics.is_empty());
        assert!(file_scope(&file).call_args.is_empty());
        assert_eq!(binding_count(&file, "$s"), 2, "T21a records the rebinding");
    }

    /// The scope with `key`, which must exist.
    fn scope<'a>(file: &'a ExtractedFile, key: &str) -> &'a ExtractedScope {
        file.scopes
            .iter()
            .find(|scope| scope.scope_key == key)
            .unwrap_or_else(|| panic!("missing scope {key}: {:?}", file.scopes))
    }

    /// The scope key of the `nth` use spelled `spelling`, in source order.
    fn use_scope(file: &ExtractedFile, spelling: &str, nth: usize) -> String {
        file.uses
            .iter()
            .filter(|use_| use_.spelling == spelling)
            .nth(nth)
            .unwrap_or_else(|| panic!("missing use #{nth} of {spelling}"))
            .scope_key
            .clone()
    }

    /// The import aliases recorded in the scope with `key`.
    fn aliases(file: &ExtractedFile, key: &str) -> Vec<String> {
        scope(file, key)
            .facts
            .imports
            .iter()
            .map(|import| import.alias.clone())
            .collect()
    }

    /// AF1 finding 1: a top-level closure chains to its namespace block, which
    /// holds the block's imports and its `module` declaration.
    #[test]
    fn top_level_closure_chains_to_its_namespace_block() {
        let file = extract(
            "<?php\nnamespace App;\nuse App\\Lib\\Tool;\n$f = function () { new Tool(); };\n$g = fn () => new Tool();\n",
        );
        assert!(file.diagnostics.is_empty());
        assert_eq!(use_scope(&file, "Tool", 1), "ns0:0");
        assert_eq!(use_scope(&file, "Tool", 2), "ns0:1");
        assert_eq!(
            scope(&file, "ns0:0").parent_scope_key.as_deref(),
            Some("ns0:file")
        );
        assert_eq!(
            scope(&file, "ns0:1").parent_scope_key.as_deref(),
            Some("ns0:file")
        );
        let block = scope(&file, "ns0:file");
        assert_eq!(block.parent_scope_key, None);
        assert_eq!(aliases(&file, "ns0:file"), ["Tool"]);
        let module = block.facts.declares[0];
        assert_eq!(file.symbols[module].qualified_name, "App");
        assert!(
            !file
                .scopes
                .iter()
                .any(|scope| scope.scope_key == FILE_SCOPE_KEY),
            "a namespaced file has no `top:file` scope"
        );
    }

    /// A file with no namespace keeps `top:file`, and a top-level closure now
    /// chains to it.
    #[test]
    fn closure_in_a_file_without_namespace_chains_to_the_file_scope() {
        let file = extract("<?php\nuse Lib\\Tool;\n$f = function () { new Tool(); };\n");
        assert_eq!(use_scope(&file, "Tool", 1), "top:0");
        assert_eq!(
            scope(&file, "top:0").parent_scope_key.as_deref(),
            Some(FILE_SCOPE_KEY)
        );
        assert_eq!(aliases(&file, FILE_SCOPE_KEY), ["Tool"]);
    }

    /// AF1 finding 2: each block, unbraced or braced, owns its own imports and
    /// declarations, and a member's scope chains to its own block.
    #[test]
    fn each_namespace_block_owns_its_imports_and_declarations() {
        for source in [
            "<?php\nnamespace One;\nuse Lib\\A;\nclass C {}\nnamespace Two;\nuse Lib\\B;\nclass D { public function m() { $f = function () { new B(); }; } }\n",
            "<?php\nnamespace One {\nuse Lib\\A;\nclass C {}\n}\nnamespace Two {\nuse Lib\\B;\nclass D { public function m() { $f = function () { new B(); }; } }\n}\n",
        ] {
            let file = extract(source);
            assert!(file.diagnostics.is_empty(), "{source}");
            assert_eq!(aliases(&file, "ns0:file"), ["A"], "{source}");
            assert_eq!(aliases(&file, "ns1:file"), ["B"], "{source}");
            let declared = |key: &str| -> Vec<String> {
                scope(&file, key)
                    .facts
                    .declares
                    .iter()
                    .map(|index| file.symbols[*index].qualified_name.clone())
                    .collect()
            };
            assert_eq!(declared("ns0:file"), ["One", "One\\C"], "{source}");
            assert_eq!(declared("ns1:file"), ["Two", "Two\\D"], "{source}");
            // The closure in `D::m` chains `m` -> `D` -> block `Two`.
            let closure = use_scope(&file, "B", 1);
            let method_scope = scope(&file, &closure)
                .parent_scope_key
                .clone()
                .expect("closure scope has a parent");
            let class_parent = scope(&file, &method_scope)
                .parent_scope_key
                .clone()
                .expect("class scope has a parent");
            assert_eq!(class_parent, "ns1:file", "{source}");
        }
    }

    /// Positions no namespace block owns are flagged, and nothing else is.
    #[test]
    fn unattributed_positions_are_flagged() {
        let mixed = extract("<?php\nnamespace One;\nnew A();\nnamespace Two { new B(); }\n");
        assert!(mixed.diagnostics.is_empty());
        assert_eq!(use_scope(&mixed, "A", 0), "orphan:file");
        assert_eq!(use_scope(&mixed, "B", 0), "orphan:file");
        assert_eq!(mixed.scopes.len(), 1, "{:?}", mixed.scopes);
        assert!(scope(&mixed, "orphan:file").facts.namespace_unattributed);

        let between = extract(
            "<?php\nnamespace One { new A(); }\n$f = function () { new C(); };\nnamespace Two { new B(); }\n",
        );
        assert!(between.diagnostics.is_empty());
        assert_eq!(use_scope(&between, "A", 0), "ns0:file");
        assert_eq!(use_scope(&between, "C", 0), "orphan:0");
        assert_eq!(use_scope(&between, "B", 0), "ns1:file");
        assert!(scope(&between, "orphan:0").facts.namespace_unattributed);
        assert!(!scope(&between, "ns0:file").facts.namespace_unattributed);
        assert!(!scope(&between, "ns1:file").facts.namespace_unattributed);
    }

    /// A top-level assignment in one block is a rebinding in every other
    /// block's top-level scope, because PHP namespaces do not scope variables.
    #[test]
    fn top_level_assignments_rebind_across_namespace_blocks() {
        let file = extract("<?php\nnamespace One;\n$s = new A();\nnamespace Two;\n$t = new B();\n");
        let one = &scope(&file, "ns0:file").facts.new_bindings;
        let two = &scope(&file, "ns1:file").facts.new_bindings;
        assert!(one.iter().any(|b| b.variable == "$s" && b.direct_new));
        assert!(one.iter().any(|b| b.variable == "$t" && !b.direct_new));
        assert!(two.iter().any(|b| b.variable == "$t" && b.direct_new));
        assert!(two.iter().any(|b| b.variable == "$s" && !b.direct_new));
    }

    #[test]
    fn class_constant_accesses_are_recorded_and_property_reads_are_not() {
        // AF2: `$x::NAME` and `$x->NAME` are both `read` uses with receiver
        // `$x`; only the constant access is listed in its scope.
        let source = "<?php\n$x = 1;\necho Foo::A, self::B, $x::C, $x->D, Foo::$e, $x->f();\n";
        let file = extract(source);
        let facts = file_scope(&file);
        let listed: Vec<&str> = facts
            .class_constant_accesses
            .iter()
            .map(|span| &source[span.start_byte() as usize..span.end_byte() as usize])
            .collect();
        assert_eq!(listed, vec!["A", "B", "C"]);
        let reads: Vec<(&str, Option<&str>)> = file
            .uses
            .iter()
            .filter(|use_| use_.ref_kind == rivet_core::RefKind::Read)
            .map(|use_| (use_.spelling.as_str(), use_.receiver.as_deref()))
            .collect();
        assert_eq!(
            reads,
            vec![
                ("A", Some("Foo")),
                ("B", Some("self")),
                ("C", Some("$x")),
                ("D", Some("$x")),
                ("$e", Some("Foo")),
            ]
        );
    }

    /// The hint of the `nth` call use spelled `spelling`, in source order.
    fn call_hint(file: &ExtractedFile, spelling: &str, nth: usize) -> UseHint {
        file.uses
            .iter()
            .filter(|use_| use_.spelling == spelling && use_.ref_kind == rivet_core::RefKind::Call)
            .nth(nth)
            .unwrap_or_else(|| panic!("missing call #{nth} of {spelling}"))
            .hint
            .clone()
    }

    /// AF3 finding 7: only a single class type or a nullable one records a
    /// typed receiver, for a parameter and a property alike.
    #[test]
    fn only_a_single_class_type_records_a_typed_receiver() {
        let file = extract(
            "<?php\nfunction f(A|B $u, A&B $i, (A&B)|null $d, A|null $n, ?A $o, A $a, int $p) {\n$u->m(); $i->m(); $d->m(); $n->m(); $o->m(); $a->m(); $p->m();\n}\n",
        );
        assert!(file.diagnostics.is_empty());
        for nth in 0..4 {
            assert_eq!(call_hint(&file, "m", nth), UseHint::Unresolved, "#{nth}");
        }
        for nth in [4, 5] {
            assert_eq!(
                call_hint(&file, "m", nth),
                UseHint::Typed {
                    type_spelling: "A".to_string(),
                    origin: TypedOrigin::Parameter,
                },
                "#{nth}"
            );
        }
        assert_eq!(call_hint(&file, "m", 6), UseHint::Unresolved);

        let class = extract(
            "<?php\nclass C {\nprivate A|B $u;\nprivate ?A $n;\npublic function __construct(private A&B $i, private ?A $q) {}\npublic function run() { $this->u->m(); $this->n->m(); $this->i->m(); $this->q->m(); }\n}\n",
        );
        assert!(class.diagnostics.is_empty());
        let property = |type_spelling: &str| UseHint::Typed {
            type_spelling: type_spelling.to_string(),
            origin: TypedOrigin::Property,
        };
        assert_eq!(call_hint(&class, "m", 0), UseHint::Unresolved);
        assert_eq!(call_hint(&class, "m", 1), property("A"));
        assert_eq!(call_hint(&class, "m", 2), UseHint::Unresolved);
        assert_eq!(call_hint(&class, "m", 3), property("A"));
    }

    /// AF3: a by-reference parameter aliases the caller's variable, so its
    /// declared type is not a receiver hint.
    #[test]
    fn a_by_reference_parameter_records_no_typed_receiver() {
        let file = extract("<?php\nfunction f(A &$x, A $y) { $x->m(); $y->m(); }\n");
        assert!(file.diagnostics.is_empty());
        assert_eq!(call_hint(&file, "m", 0), UseHint::Unresolved);
        assert!(matches!(call_hint(&file, "m", 1), UseHint::Typed { .. }));
    }

    /// AF3 requirement 3: a reference taken to a variable records a rebinding
    /// of it: `$y = &$x`, a by-reference `foreach` over it, and a
    /// by-reference array element.
    #[test]
    fn a_reference_taken_to_a_variable_records_a_rebinding() {
        assert_rebound("<?php\n$s = new Alpha();\n$y = &$s;\n$s->go();\n");
        assert_rebound("<?php\n$s = new Alpha();\nforeach ($s as &$v) {}\n$s->go();\n");
        assert_rebound("<?php\n$s = new Alpha();\nforeach ($s as $k => &$v) {}\n$s->go();\n");
        assert_rebound("<?php\n$s = new Alpha();\n$a = [&$s];\n$s->go();\n");
        // A by-value `foreach` and a reference to an element of another array
        // do not rebind the iterated or indexed variable.
        let file =
            extract("<?php\n$s = new Alpha();\nforeach ($s as $v) {}\n$y = &$t['k'];\n$s->go();\n");
        assert_eq!(binding_count(&file, "$s"), 1);
        assert_eq!(binding_count(&file, "$t"), 0);
    }

    /// AF3 requirement 4: constructor arguments are walked as code and
    /// recorded as `__construct` call arguments with the class to look it up
    /// on.
    #[test]
    fn constructor_arguments_are_walked_and_recorded() {
        let file = extract(
            "<?php\nnew Holder($a, inner($b));\nnew self($c);\nnew static($d);\nnew parent($e);\nnew $cls($f);\nnew class($g) {};\n",
        );
        assert!(file.diagnostics.is_empty());
        assert!(
            file.uses.iter().any(|use_| use_.spelling == "inner"),
            "a call inside constructor arguments is a use"
        );
        let args = &file_scope(&file).call_args;
        let constructor: Vec<(&str, Option<u32>, &CallReceiver)> = args
            .iter()
            .filter(|arg| arg.kind == CallArgKind::Constructor)
            .map(|arg| {
                assert_eq!(arg.callee, "__construct");
                (
                    arg.variable.as_str(),
                    arg.position,
                    arg.receiver.as_ref().expect("a constructor has a receiver"),
                )
            })
            .collect();
        let holder = CallReceiver::Class {
            spelling: "Holder".to_string(),
        };
        assert_eq!(
            constructor,
            vec![
                ("$a", Some(0), &holder),
                ("$c", Some(0), &CallReceiver::SelfClass),
                ("$d", Some(0), &CallReceiver::Unknown),
                ("$e", Some(0), &CallReceiver::Unknown),
                ("$f", Some(0), &CallReceiver::Unknown),
                ("$g", Some(0), &CallReceiver::Unknown),
            ]
        );
        // `inner($b)` is an ordinary function argument.
        assert!(
            args.iter()
                .any(|arg| arg.variable == "$b" && arg.kind == CallArgKind::Function)
        );
    }

    /// A reassignment inside constructor arguments is a rebinding the walker
    /// must see, now that the arguments are walked.
    #[test]
    fn a_reassignment_inside_constructor_arguments_is_recorded() {
        assert_rebound("<?php\n$s = new Alpha();\nnew Holder($s = mk());\n$s->go();\n");
    }

    /// The by-reference flags recorded for the one function in `source`.
    fn parameter_flags(source: &str) -> Option<Vec<bool>> {
        let file = extract(source);
        assert!(file.diagnostics.is_empty(), "snippet must parse: {source}");
        let lists: Vec<&rivet_core::extract::ParameterList> = file
            .scopes
            .iter()
            .flat_map(|scope| scope.facts.parameter_lists.iter())
            .collect();
        assert!(lists.len() <= 1, "{lists:?}");
        lists.first().map(|list| {
            assert!(matches!(
                file.symbols[list.symbol].kind,
                rivet_core::SymbolKind::Function | rivet_core::SymbolKind::Method
            ));
            list.by_ref.clone()
        })
    }

    /// AF3 finding 9 and requirement 6: by-reference positions come from the
    /// tree, so attribute text, default values, and comments cannot mislead.
    #[test]
    fn parameter_by_reference_flags_come_from_the_tree() {
        assert_eq!(
            parameter_flags(
                "<?php\n#[Deprecated(\"use function other\")] #[Pure(1)] function rebind(&$v) {}\n"
            ),
            Some(vec![true])
        );
        assert_eq!(
            parameter_flags("<?php\nfunction f($a = \"function (\", &$v = null, $w = [1, 2]) {}\n"),
            Some(vec![false, true, false])
        );
        assert_eq!(
            parameter_flags("<?php\nfunction f(/* ) , */ $a, /* & */ &$v, // &$z\n $w) {}\n"),
            Some(vec![false, true, false])
        );
        assert_eq!(
            parameter_flags("<?php\nfunction f(#[A('&$v')] $a, $b = '&$x', A&B $c) {}\n"),
            Some(vec![false, false, false])
        );
        assert_eq!(
            parameter_flags("<?php\nfunction f(int $a, &...$rest) {}\n"),
            Some(vec![false, true])
        );
        assert_eq!(
            parameter_flags("<?php\nfunction f() {}\n"),
            Some(Vec::new())
        );
        assert_eq!(
            parameter_flags(
                "<?php\nclass C { public function __construct(private A &$x, public ?B $y) {} }\n"
            ),
            Some(vec![true, false])
        );
        // A closure is not an indexed callee and records nothing.
        assert_eq!(parameter_flags("<?php\n$f = function (&$x) {};\n"), None);
    }

    /// AF3 requirement 5: the facts behind the `global` rule.
    #[test]
    fn global_rebinding_facts_are_recorded_per_scope() {
        let file = extract(
            "<?php\nfunction a() { global $x, $y; }\nfunction b() { $GLOBALS['z'] = 1; unset($GLOBALS[\"u\"]); }\nfunction c() { $r = &$GLOBALS['w']; f($GLOBALS['v']); }\nfunction d() { global $$n; }\nfunction e($k) { $GLOBALS[$k] = 1; }\nfunction g() { eval('1;'); }\nfunction h() { include 'x.php'; }\nfunction i() { foreach ($GLOBALS as &$v) {} }\nfunction j() { $GLOBALS['a' . 'b'] = 1; }\nfunction k() { $x = 1; }\n",
        );
        assert!(file.diagnostics.is_empty());
        let facts_of = |function: &str| -> &ScopeFacts {
            let index = file
                .symbols
                .iter()
                .position(|symbol| symbol.name == function)
                .expect("function symbol");
            &file
                .scopes
                .iter()
                .find(|scope| scope.scope_key.starts_with(&format!("{index}:")))
                .unwrap_or_else(|| panic!("scope of {function}"))
                .facts
        };
        assert_eq!(facts_of("a").global_names, ["$x", "$y"]);
        assert!(!facts_of("a").dynamic_global_write);
        assert_eq!(facts_of("b").global_names, ["$u", "$z"]);
        assert_eq!(facts_of("c").global_names, ["$v", "$w"]);
        for dynamic in ["d", "e", "g", "h", "i", "j"] {
            assert!(facts_of(dynamic).dynamic_global_write, "{dynamic}");
        }
        assert!(facts_of("k").global_names.is_empty());
        assert!(!facts_of("k").dynamic_global_write);
        for scope in &file.scopes {
            assert!(!scope.facts.global_scope || scope.scope_key == FILE_SCOPE_KEY);
        }
    }

    /// File-scope `global`, `$GLOBALS`, `eval`, and includes run only when the
    /// file itself runs, so they are not cross-scope global writes.
    #[test]
    fn file_scope_global_writes_are_not_cross_scope_facts() {
        let file =
            extract("<?php\nglobal $x;\n$GLOBALS['y'] = 1;\neval('1;');\nrequire 'boot.php';\n");
        let facts = file_scope(&file);
        assert!(facts.global_scope);
        assert!(facts.global_names.is_empty());
        assert!(!facts.dynamic_global_write);
        assert!(facts.unanalysable, "T21b still marks the scope itself");
    }

    /// Call sites and `goto` are recorded only for a global scope, and a
    /// direct `new` records where its expression ends.
    #[test]
    fn global_scope_records_call_sites_goto_and_new_extent() {
        let source = "<?php\n$s = new Alpha(mk());\nf();\n$o->m();\nK::s();\nclone $o;\nL:\ngoto L;\nfunction inner() { g(); goto M; M: }\n";
        let file = extract(source);
        assert!(file.diagnostics.is_empty());
        let facts = file_scope(&file);
        let calls: Vec<&str> = facts
            .call_sites
            .iter()
            .map(|span| &source[span.start_byte() as usize..span.end_byte() as usize])
            .collect();
        assert_eq!(
            calls,
            [
                "new Alpha(mk())",
                "mk()",
                "f()",
                "$o->m()",
                "K::s()",
                "clone $o"
            ]
        );
        assert!(facts.goto_present);
        let binding = facts
            .new_bindings
            .iter()
            .find(|binding| binding.variable == "$s")
            .expect("the assignment");
        assert_eq!(
            binding.value_end,
            Some((source.find("mk())").unwrap() + "mk())".len()) as u32)
        );
        for scope in file
            .scopes
            .iter()
            .filter(|scope| scope.scope_key != FILE_SCOPE_KEY)
        {
            assert!(!scope.facts.global_scope, "{}", scope.scope_key);
            assert!(scope.facts.call_sites.is_empty(), "{}", scope.scope_key);
            assert!(!scope.facts.goto_present, "{}", scope.scope_key);
        }
    }

    /// AF3: a call argument's receiver keeps the variable its class came from
    /// unless the class comes from a typed property.
    #[test]
    fn call_receivers_keep_the_hinted_variable() {
        let file = extract(
            "<?php\nclass C {\nprivate Svc $p;\npublic function run(Svc $t) { $t->take($a); $this->p->take($b); }\n}\n",
        );
        assert!(file.diagnostics.is_empty());
        let receivers: Vec<(&str, &CallReceiver)> = file
            .scopes
            .iter()
            .flat_map(|scope| scope.facts.call_args.iter())
            .map(|arg| {
                (
                    arg.variable.as_str(),
                    arg.receiver.as_ref().expect("receiver"),
                )
            })
            .collect();
        assert_eq!(
            receivers,
            vec![
                (
                    "$a",
                    &CallReceiver::Variable {
                        variable: "$t".to_string(),
                        evidence: ReceiverEvidence::TypedParameter {
                            type_spelling: "Svc".to_string()
                        },
                    }
                ),
                (
                    "$b",
                    &CallReceiver::Class {
                        spelling: "Svc".to_string()
                    }
                ),
            ]
        );
    }

    /// Every use as `(spelling, ref_kind, start_byte, hint)`, in span order.
    fn use_summary(file: &ExtractedFile) -> Vec<(String, &'static str, u32, UseHint)> {
        file.uses
            .iter()
            .map(|use_| {
                (
                    use_.spelling.clone(),
                    use_.ref_kind.as_str(),
                    use_.span.start_byte(),
                    use_.hint.clone(),
                )
            })
            .collect()
    }

    /// The byte offset of the `nth` occurrence of `needle` in `source`.
    fn offset(source: &str, needle: &str, nth: usize) -> u32 {
        source
            .match_indices(needle)
            .nth(nth)
            .unwrap_or_else(|| panic!("missing occurrence #{nth} of {needle}"))
            .0 as u32
    }

    /// AF4 finding 11: an anonymous class body is walked. Its uses keep the
    /// nearest named container, and `$this`, `self`, and `static` inside it
    /// record no receiver evidence, because they name the anonymous class.
    #[test]
    fn anonymous_class_bodies_are_walked_without_self_evidence() {
        let source = "<?php\nclass Outer {\n    public function run() {}\n    public function host() {\n        \
                      $a = new class {\n            public function run(Dep $d) {\n                \
                      launch(); new Dep(); $this->run(); self::run(); static::run(); \
                      $n = new self(); $n->run(); $f = fn () => $this->run();\n            }\n        };\n        \
                      $this->run(); self::run();\n    }\n}\n";
        let file = extract(source);
        let host = file
            .symbols
            .iter()
            .position(|symbol| symbol.qualified_name == "Outer::host")
            .expect("host is a symbol");
        // The anonymous class's `run` is not an addressable symbol.
        assert!(!file.symbols.iter().any(|symbol| symbol.span.start_byte()
            > offset(source, "new class", 0)
            && symbol.name == "run"));
        let body_start = offset(source, "new class", 0);
        let body_end = offset(source, "};", 0);
        let inside: Vec<_> = file
            .uses
            .iter()
            .filter(|use_| use_.span.start_byte() > body_start && use_.span.end_byte() < body_end)
            .collect();
        let spellings: Vec<(&str, &str)> = inside
            .iter()
            .map(|use_| (use_.spelling.as_str(), use_.ref_kind.as_str()))
            .collect();
        assert_eq!(
            spellings,
            vec![
                ("Dep", "type"),
                ("launch", "call"),
                ("Dep", "type"),
                ("run", "call"),
                ("run", "call"),
                ("run", "call"),
                ("self", "type"),
                ("run", "call"),
                ("run", "call"),
            ]
        );
        for use_ in &inside {
            assert_eq!(use_.containing_symbol_index, Some(host), "{use_:?}");
            if use_.ref_kind == rivet_core::RefKind::Call {
                assert_eq!(use_.hint, UseHint::Unresolved, "{use_:?}");
            }
        }
        // `new self` inside the anonymous class runs its own constructor.
        let ctor = file
            .scopes
            .iter()
            .flat_map(|scope| scope.facts.call_args.iter())
            .find(|arg| arg.kind == CallArgKind::Constructor);
        assert!(ctor.is_none(), "no argument was passed: {ctor:?}");
        // Outside the anonymous class `$this` and `self` keep their hints.
        let after: Vec<UseHint> = file
            .uses
            .iter()
            .filter(|use_| use_.span.start_byte() > body_end)
            .map(|use_| use_.hint.clone())
            .collect();
        assert_eq!(after, vec![UseHint::This, UseHint::SelfOrStatic]);
    }

    /// AF4: `$this->prop` inside an anonymous class reads the anonymous
    /// class's own typed property, never the enclosing class's.
    #[test]
    fn anonymous_class_typed_properties_are_its_own() {
        let source = "<?php\nclass Outer {\n    private Svc $svc;\n    public function host() {\n        \
                      new class { private Other $svc; function a() { $this->svc->go(); } };\n        \
                      new class { function b() { $this->svc->go(); } };\n    }\n}\n";
        let file = extract(source);
        assert_eq!(
            call_hint(&file, "go", 0),
            UseHint::Typed {
                type_spelling: "Other".to_string(),
                origin: TypedOrigin::Property,
            }
        );
        assert_eq!(call_hint(&file, "go", 1), UseHint::Unresolved);
    }

    /// AF4: a constructor argument of an anonymous class is still evaluated
    /// in the enclosing scope and recorded with an unknown receiver.
    #[test]
    fn anonymous_class_arguments_stay_in_the_enclosing_scope() {
        let source = "<?php\n$v = 1;\n$a = new class($v) { function f() { $v = 2; } };\n";
        let file = extract(source);
        let args: Vec<_> = file_scope(&file)
            .call_args
            .iter()
            .map(|arg| (arg.variable.as_str(), arg.kind, arg.receiver.clone()))
            .collect();
        assert_eq!(
            args,
            vec![("$v", CallArgKind::Constructor, Some(CallReceiver::Unknown))]
        );
        // The body's `$v = 2` is the anonymous method's own local.
        assert_eq!(binding_count(&file, "$v"), 1, "only `$v = 1`");
    }

    /// AF4 finding 13: the class named before `::` is a type use, and the
    /// member carries a `NamedClass` hint. `::class` is not a member, and
    /// `self`, `static`, and `parent` record no type use.
    #[test]
    fn explicit_class_scopes_record_a_type_use() {
        let source = "<?php\nFoo::make(); Foo::BAR; Foo::$prop; Foo::class; \\App\\Foo::make(); \
                      static::make(); parent::make(); self::X; $x::class;\n";
        let file = extract(source);
        let named = |spelling: &str| UseHint::NamedClass {
            class_spelling: spelling.to_string(),
        };
        assert_eq!(
            use_summary(&file),
            vec![
                (
                    "Foo".to_string(),
                    "type",
                    offset(source, "Foo::make", 0),
                    UseHint::Unresolved
                ),
                (
                    "make".to_string(),
                    "call",
                    offset(source, "make", 0),
                    named("Foo")
                ),
                (
                    "Foo".to_string(),
                    "type",
                    offset(source, "Foo::BAR", 0),
                    UseHint::Unresolved
                ),
                (
                    "BAR".to_string(),
                    "read",
                    offset(source, "BAR", 0),
                    named("Foo")
                ),
                (
                    "Foo".to_string(),
                    "type",
                    offset(source, "Foo::$prop", 0),
                    UseHint::Unresolved
                ),
                (
                    "$prop".to_string(),
                    "read",
                    offset(source, "$prop", 0),
                    named("Foo")
                ),
                (
                    "Foo".to_string(),
                    "type",
                    offset(source, "Foo::class", 0),
                    UseHint::Unresolved
                ),
                (
                    "\\App\\Foo".to_string(),
                    "type",
                    offset(source, "\\App", 0),
                    UseHint::Unresolved
                ),
                (
                    "make".to_string(),
                    "call",
                    offset(source, "make", 1),
                    named("\\App\\Foo")
                ),
                (
                    "make".to_string(),
                    "call",
                    offset(source, "make", 2),
                    UseHint::SelfOrStatic
                ),
                (
                    "make".to_string(),
                    "call",
                    offset(source, "make", 3),
                    UseHint::SelfOrStatic
                ),
                (
                    "X".to_string(),
                    "read",
                    offset(source, "X;", 0),
                    UseHint::SelfOrStatic
                ),
            ]
        );
        // The class-constant read is still told apart from a property read.
        let constants = &file_scope(&file).class_constant_accesses;
        let bar = offset(source, "BAR", 0);
        assert!(constants.iter().any(|span| span.start_byte() == bar));
    }

    /// AF4: an `instanceof` class operand is a type use; a variable operand
    /// records nothing.
    #[test]
    fn instanceof_operands_are_type_uses() {
        let source = "<?php\n$x instanceof \\App\\I; $x instanceof J; $x instanceof $y;\n";
        let file = extract(source);
        let summary: Vec<(String, &str)> = use_summary(&file)
            .into_iter()
            .map(|(spelling, kind, _, _)| (spelling, kind))
            .collect();
        assert_eq!(
            summary,
            vec![("\\App\\I".to_string(), "type"), ("J".to_string(), "type")]
        );
    }

    /// AF4 item 6: a `catch` type was already a type use before AF4.
    #[test]
    fn catch_types_are_type_uses() {
        let source = "<?php\ntry {} catch (Foo | \\App\\Bar $e) {}\n";
        let file = extract(source);
        let summary: Vec<(String, &str)> = use_summary(&file)
            .into_iter()
            .map(|(spelling, kind, _, _)| (spelling, kind))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("Foo".to_string(), "type"),
                ("\\App\\Bar".to_string(), "type")
            ]
        );
    }

    /// AF4: an enum case's own name is a declaration, not a use; its backing
    /// value is still code.
    #[test]
    fn enum_case_names_are_not_uses() {
        let source = "<?php\nenum S: int { case A = 1; case B = Foo::C; }\n";
        let file = extract(source);
        let summary: Vec<(String, &str)> = use_summary(&file)
            .into_iter()
            .map(|(spelling, kind, _, _)| (spelling, kind))
            .collect();
        assert_eq!(
            summary,
            vec![("Foo".to_string(), "type"), ("C".to_string(), "read")]
        );
    }

    /// `(spelling, kind, container qualified name)` for every use.
    fn use_containers(file: &ExtractedFile) -> Vec<(String, &'static str, Option<String>)> {
        file.uses
            .iter()
            .map(|use_| {
                (
                    use_.spelling.clone(),
                    use_.ref_kind.as_str(),
                    use_.containing_symbol_index
                        .map(|index| file.symbols[index].qualified_name.clone()),
                )
            })
            .collect()
    }

    /// `(declaring qualified name, relation, spelling)` for every recorded
    /// supertype fact, with the scope key that holds it.
    fn supertype_facts(file: &ExtractedFile) -> Vec<(String, String, &'static str, String)> {
        file.scopes
            .iter()
            .flat_map(|scope| {
                scope.facts.supertypes.iter().map(|fact| {
                    (
                        scope.scope_key.clone(),
                        file.symbols[fact.symbol].qualified_name.clone(),
                        fact.relation.as_str(),
                        fact.spelling.clone(),
                    )
                })
            })
            .collect()
    }

    /// T36d: every name in `extends` and `implements` is a type use contained
    /// by the declared class, and a supertype fact in the declaring scope
    /// whose span is exactly the use's.
    #[test]
    fn class_header_names_are_type_uses_and_supertype_facts() {
        let source = "<?php\nnamespace App;\nuse Lib\\Base;\n\
                      final class Sub extends Base implements \\Lib\\A, Sub\\B {}\n";
        let file = extract(source);
        let sub = Some("App\\Sub".to_string());
        assert_eq!(
            use_containers(&file)
                .into_iter()
                .filter(|(_, kind, _)| *kind == "type")
                .collect::<Vec<_>>(),
            vec![
                ("Base".to_string(), "type", sub.clone()),
                ("\\Lib\\A".to_string(), "type", sub.clone()),
                ("Sub\\B".to_string(), "type", sub.clone()),
            ]
        );
        assert_eq!(
            supertype_facts(&file),
            vec![
                (
                    "ns0:file".to_string(),
                    "App\\Sub".to_string(),
                    "extends",
                    "Base".to_string()
                ),
                (
                    "ns0:file".to_string(),
                    "App\\Sub".to_string(),
                    "implements",
                    "\\Lib\\A".to_string()
                ),
                (
                    "ns0:file".to_string(),
                    "App\\Sub".to_string(),
                    "implements",
                    "Sub\\B".to_string()
                ),
            ]
        );
        for fact in file.scopes.iter().flat_map(|scope| &scope.facts.supertypes) {
            assert!(
                file.uses
                    .iter()
                    .any(|use_| use_.span == fact.span && use_.spelling == fact.spelling),
                "{fact:?} has no use with the same span"
            );
        }
        // The header uses live in the class's own scope, which chains to the
        // namespace block that holds the `use` import.
        let base = file
            .uses
            .iter()
            .find(|use_| use_.spelling == "Base" && use_.ref_kind.as_str() == "type")
            .expect("Base use");
        let scope = file
            .scopes
            .iter()
            .find(|scope| scope.scope_key == base.scope_key)
            .expect("use scope");
        assert_eq!(scope.parent_scope_key.as_deref(), Some("ns0:file"));
    }

    /// T36d: `implements A, B` and an interface's `extends I1, I2` each record
    /// two uses and two facts; an enum's `implements` records one.
    #[test]
    fn interface_lists_and_enum_implements_are_recorded() {
        let source = "<?php\ninterface I extends I1, I2 {}\n\
                      class C implements A, B {}\n\
                      enum E: string implements I { case X = 'x'; }\n";
        let file = extract(source);
        let summary: Vec<(String, &str)> = use_summary(&file)
            .into_iter()
            .map(|(spelling, kind, _, _)| (spelling, kind))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("I1".to_string(), "type"),
                ("I2".to_string(), "type"),
                ("A".to_string(), "type"),
                ("B".to_string(), "type"),
                ("I".to_string(), "type"),
            ]
        );
        let facts: Vec<(String, &str, String)> = supertype_facts(&file)
            .into_iter()
            .map(|(_, owner, relation, spelling)| (owner, relation, spelling))
            .collect();
        assert_eq!(
            facts,
            vec![
                ("I".to_string(), "extends", "I1".to_string()),
                ("I".to_string(), "extends", "I2".to_string()),
                ("C".to_string(), "implements", "A".to_string()),
                ("C".to_string(), "implements", "B".to_string()),
                ("E".to_string(), "implements", "I".to_string()),
            ]
        );
    }

    /// T36d: an anonymous class's `extends` and `implements` names are type
    /// uses contained by the nearest named container; the anonymous class is
    /// still not a symbol and records no supertype fact. A trait records
    /// neither (it has no header clauses), and its `use` of another trait is
    /// not a supertype.
    #[test]
    fn anonymous_class_headers_record_uses_but_no_symbol_or_fact() {
        let source = "<?php\nclass Host {\n    public function make() {\n        \
                      return new class(1) extends Base implements I { public function m() {} };\n    \
                      }\n}\ntrait T { use U; }\n";
        let file = extract(source);
        let make = Some("Host::make".to_string());
        let types: Vec<_> = use_containers(&file)
            .into_iter()
            .filter(|(_, kind, _)| *kind == "type")
            .collect();
        assert_eq!(
            types,
            vec![
                ("Base".to_string(), "type", make.clone()),
                ("I".to_string(), "type", make.clone()),
            ]
        );
        let names: Vec<&str> = file
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert_eq!(names, vec!["Host", "Host::make", "T"]);
        assert!(supertype_facts(&file).is_empty());
    }

    /// T36d: the expression before `::` is walked as code in every scoped
    /// form, so the uses inside it are recorded; the expression itself is
    /// never a type use. A plain class name keeps its AF4 type use.
    #[test]
    fn expression_scopes_are_walked_without_a_type_use() {
        let source = "<?php\nclass K {\n    public function f() {}\n    public function g($obj) {\n        \
                      $obj::$prop; $obj::CONST; $obj::method(); $this->f()::$p; \
                      (make())::$q; $this->f()::C; Foo::$r;\n    }\n}\n";
        let file = extract(source);
        let summary: Vec<(String, &str)> = use_summary(&file)
            .into_iter()
            .map(|(spelling, kind, _, _)| (spelling, kind))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("$prop".to_string(), "read"),
                ("CONST".to_string(), "read"),
                ("method".to_string(), "call"),
                ("f".to_string(), "call"),
                ("$p".to_string(), "read"),
                ("make".to_string(), "call"),
                ("$q".to_string(), "read"),
                ("f".to_string(), "call"),
                ("C".to_string(), "read"),
                ("Foo".to_string(), "type"),
                ("$r".to_string(), "read"),
            ]
        );
        // `$this->f()` before `::` is recorded as the call it is, with its
        // `$this` receiver evidence.
        let calls: Vec<UseHint> = file
            .uses
            .iter()
            .filter(|use_| use_.spelling == "f")
            .map(|use_| use_.hint.clone())
            .collect();
        assert_eq!(calls, vec![UseHint::This, UseHint::This]);
    }
}
