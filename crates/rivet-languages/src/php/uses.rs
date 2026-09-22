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

use std::collections::{BTreeMap, HashMap};

use rivet_core::extract::{
    CallArg, CallArgKind, CallReceiver, ExtractedImport, ExtractedScope, ExtractedUse, ImportKind,
    NewBinding, ScopeFacts, ScopeImport, TypedBinding, UseHint,
};
use rivet_core::{ExtractedSymbol, RefKind, Span, SymbolKind};
use tree_sitter::Node;

/// The deterministic key of a file's top-level scope.
pub const FILE_SCOPE_KEY: &str = "top:file";

/// The most recent binding of one variable within a function body.
enum Binding {
    /// The most recent assignment was `new X(...)`.
    New(String),
    /// The variable is a typed parameter or promoted property.
    Typed(String),
    /// Known but unsupported for hints.
    Other,
}

/// The per-file use walker.
struct Walker<'a> {
    source: &'a [u8],
    symbols: &'a [ExtractedSymbol],
    /// Class-like node id -> property name (no `$`) -> written type spelling.
    property_types: HashMap<usize, HashMap<String, String>>,
    uses: Vec<ExtractedUse>,
    imports: Vec<ExtractedImport>,
    /// Owned lexical facts per scope key, filled while walking.
    scope_facts: BTreeMap<String, ScopeFacts>,
    /// Stack of lexical function-body variable scopes; the last is current.
    scopes: Vec<HashMap<String, Binding>>,
    /// Stack of enclosing class-like node ids.
    class_stack: Vec<usize>,
    /// Stack of enclosing function-body ordinals; `None` at file scope.
    body_stack: Vec<Option<u32>>,
    next_body_ordinal: u32,
}

/// Extract uses, imports, and lexical scope facts from a parsed, error-free
/// PHP file.
///
/// `symbols` must be the already-built symbol list, because uses record the
/// index of their innermost named container and scope facts record
/// declarations by symbol index.
pub fn extract_uses(
    source: &[u8],
    root: Node<'_>,
    symbols: &[ExtractedSymbol],
) -> (Vec<ExtractedUse>, Vec<ExtractedImport>, Vec<ExtractedScope>) {
    let mut walker = Walker {
        source,
        symbols,
        property_types: collect_property_types(root, source),
        uses: Vec::new(),
        imports: Vec::new(),
        scope_facts: BTreeMap::new(),
        scopes: vec![HashMap::new()],
        class_stack: Vec::new(),
        body_stack: vec![None],
        next_body_ordinal: 0,
    };
    walker.visit(root, false);
    walker.uses.sort_by(|a, b| {
        a.span
            .start_byte()
            .cmp(&b.span.start_byte())
            .then(a.span.end_byte().cmp(&b.span.end_byte()))
    });
    let scopes = walker.finish_scopes();
    (walker.uses, walker.imports, scopes)
}

impl Walker<'_> {
    /// Visit one node, classifying it or descending generically.
    ///
    /// `write` is true while walking a binding target: the left side of an
    /// assignment, a `foreach` target, a `catch` parameter, and so on. A
    /// property access there becomes [`RefKind::Write`], and a bare variable
    /// mention there is recorded as a rebinding (T21a).
    fn visit(&mut self, node: Node<'_>, write: bool) {
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
            "augmented_assignment_expression" | "reference_assignment_expression" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.visit(left, true);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.visit(right, false);
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
            "unset_statement" | "global_declaration" => {
                for child in named_children(node) {
                    self.visit(child, true);
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
                        self.visit(child, by_ref);
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
    fn visit_class(&mut self, node: Node<'_>) {
        self.class_stack.push(node.id());
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

    /// Visit a parameter's type and record a typed binding for its variable.
    fn visit_parameter_type(&mut self, node: Node<'_>) {
        let Some(ty) = node.child_by_field_name("type") else {
            return;
        };
        self.visit(ty, false);
        let Some(type_node) = first_named_type(ty) else {
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
        for child in named_children(node) {
            if Some(child.id()) == body_id || child.start_byte() < as_byte {
                self.visit(child, false);
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
                receiver = Some(call_receiver_from_hint(&hint));
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
        let scope = node.child_by_field_name("scope");
        let mut callee: Option<String> = None;
        let mut receiver: Option<CallReceiver> = None;
        if let Some(name) = node.child_by_field_name("name") {
            callee = Some(self.text(name));
            if name.kind() == "name" {
                let receiver_text = scope.map(|scope| self.text(scope));
                let hint = scope
                    .map(|scope| self.receiver_hint(scope))
                    .unwrap_or(UseHint::Unresolved);
                receiver = Some(self.call_receiver_from_scope(scope, &hint));
                self.push_use(name, RefKind::Call, receiver_text, hint);
            } else {
                receiver = Some(CallReceiver::Unknown);
            }
        }
        if let Some(scope) = scope {
            // A class-name scope is carried as the receiver, not a second use.
            if !matches!(
                scope.kind(),
                "name" | "qualified_name" | "relative_name" | "relative_scope"
            ) {
                self.visit(scope, false);
            }
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
        match scope.kind() {
            "name" | "qualified_name" | "relative_name" => CallReceiver::Class {
                spelling: self.text(scope),
            },
            "relative_scope" => match self.text(scope).to_ascii_lowercase().as_str() {
                "self" | "static" => CallReceiver::SelfClass,
                // `parent` names an ancestor v0.1 does not traverse.
                _ => CallReceiver::Unknown,
            },
            _ => call_receiver_from_hint(hint),
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

    /// Marks the scope that owns `node` as unanalysable (T21b).
    fn mark_unanalysable(&mut self, node: Node<'_>) {
        let scope_key = self.scope_key_for_node(node);
        self.scope_facts.entry(scope_key).or_default().unanalysable = true;
    }

    fn visit_object_creation(&mut self, node: Node<'_>) {
        if let Some(class) = object_creation_class(node)
            && matches!(class.kind(), "name" | "qualified_name" | "relative_name")
        {
            self.push_use(class, RefKind::Type, None, UseHint::Unresolved);
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            self.visit(arguments, false);
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
        if let Some(name) = node.child_by_field_name("name")
            && name.kind() == "variable_name"
        {
            let receiver = scope.map(|scope| self.text(scope));
            let hint = scope
                .map(|scope| self.receiver_hint(scope))
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
        if name.kind() == "name" || name.kind() == "variable_name" {
            let receiver = Some(self.text(scope));
            let hint = self.receiver_hint(scope);
            self.push_use(name, RefKind::Read, receiver, hint);
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
                .map(|class| Binding::New(self.text(class)))
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
        let Ok(span) = Span::new(node.start_byte() as u32, node.end_byte() as u32) else {
            return;
        };
        let containing = containing_symbol(self.symbols, span.start_byte(), span.end_byte());
        let scope_key = self.scope_key(containing);
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

    fn scope_key(&self, containing: Option<usize>) -> String {
        let container = match containing {
            Some(index) => index.to_string(),
            None => "top".to_string(),
        };
        match self.body_stack.last().copied().flatten() {
            Some(ordinal) => format!("{container}:{ordinal}"),
            None => format!("{container}:file"),
        }
    }

    /// Derive the receiver hint for a member/static receiver expression.
    fn receiver_hint(&self, node: Node<'_>) -> UseHint {
        match node.kind() {
            "relative_scope" => UseHint::SelfOrStatic,
            "variable_name" => {
                let text = self.text(node);
                if text == "$this" {
                    return UseHint::This;
                }
                match self.scopes.last().and_then(|scope| scope.get(&text)) {
                    Some(Binding::New(class)) => UseHint::NewExpr {
                        class_spelling: class.clone(),
                        use_block: enclosing_block_start(node),
                    },
                    Some(Binding::Typed(ty)) => UseHint::Typed {
                        type_spelling: ty.clone(),
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
                let Some(class_id) = self.class_stack.last() else {
                    return UseHint::Unresolved;
                };
                match self
                    .property_types
                    .get(class_id)
                    .and_then(|properties| properties.get(&property))
                {
                    Some(ty) => UseHint::Typed {
                        type_spelling: ty.clone(),
                    },
                    None => UseHint::Unresolved,
                }
            }
            _ => UseHint::Unresolved,
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
                Binding::New(class_spelling) => {
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
        self.scope_key(containing)
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
        // The file scope always exists so imports and top-level declarations
        // have a home even in a file with no uses.
        scope_facts.entry(FILE_SCOPE_KEY.to_string()).or_default();
        for (index, symbol) in self.symbols.iter().enumerate() {
            let scope_key = match symbol.parent_index {
                Some(parent) => format!("{parent}:file"),
                None => FILE_SCOPE_KEY.to_string(),
            };
            scope_facts
                .entry(scope_key)
                .or_default()
                .declares
                .push(index);
        }
        scope_facts
            .into_iter()
            .map(|(scope_key, facts)| {
                let parent_scope_key = parent_scope_key(&scope_key, self.symbols);
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

/// The enclosing scope key of `scope_key`.
///
/// A scope key is `top:file`, `{symbol_index}:file` (a class-like body or an
/// otherwise unscoped position), or `{symbol_index}:{body_ordinal}` (a function
/// body). The parent of a symbol-owned scope is the scope that declares that
/// symbol: the file scope for a top-level symbol, or the parent's `:file` scope
/// for a member. The file scope has no parent.
fn parent_scope_key(scope_key: &str, symbols: &[ExtractedSymbol]) -> Option<String> {
    if scope_key == FILE_SCOPE_KEY {
        return None;
    }
    let container = scope_key.split(':').next()?;
    if container == "top" {
        return None;
    }
    let index: usize = container.parse().ok()?;
    let symbol = symbols.get(index)?;
    Some(match symbol.parent_index {
        Some(parent) => format!("{parent}:file"),
        None => FILE_SCOPE_KEY.to_string(),
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
fn call_receiver_from_hint(hint: &UseHint) -> CallReceiver {
    match hint {
        UseHint::NewExpr { class_spelling, .. } => CallReceiver::Class {
            spelling: class_spelling.clone(),
        },
        UseHint::Typed { type_spelling } => CallReceiver::Class {
            spelling: type_spelling.clone(),
        },
        UseHint::This | UseHint::SelfOrStatic => CallReceiver::SelfClass,
        UseHint::Imported { .. } | UseHint::Unresolved => CallReceiver::Unknown,
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

/// The first `named_type` within a type node (unwrapping optional/union types).
fn first_named_type(node: Node<'_>) -> Option<Node<'_>> {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "named_type" {
            return Some(current);
        }
        for child in named_children(current) {
            stack.push(child);
        }
    }
    None
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
                    && let Some(type_node) = first_named_type(type_field)
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
                        first_named_type(type_field),
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
    use rivet_core::extract::{CallArgKind, CallReceiver, NewBinding, ScopeFacts};
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
        assert_eq!(
            args[0].receiver,
            Some(CallReceiver::Class {
                spelling: "Alpha".to_string()
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
}
