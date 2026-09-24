//! T43 TypeScript/TSX uses, scopes, and imports: adversarial cases.
//!
//! The authored fixture is compared with its gold in `typescript_gold.rs`.
//! These tests pin what the fixture does not reach on its own: shadowing
//! facts, anonymous containers, literal text, template interpolation, optional
//! chaining, type positions, JSX shapes, every import and re-export form,
//! receiver hints, writes, declaration names, value and type spaces, block and
//! function scoping, and the use bound.

#![cfg(feature = "lang-typescript")]

use rivet_core::extract::{
    BindingSpace, ExtractedScope, ExtractedUse, ModuleImport, ModuleImportKind, TypedOrigin,
    UseHint,
};
use rivet_core::{ExtractedFile, RefKind, Span};
use rivet_languages::{LanguageId, ResourceLimits, grammar, typescript};
use tree_sitter::{Parser, Tree};

fn parse(language: LanguageId, source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&grammar(language))
        .expect("pinned grammar must load");
    parser
        .parse(source.as_bytes(), None)
        .expect("parser must return a tree")
}

/// Extracts `source` with `language`'s grammar and requires a clean parse.
fn extract_with(language: LanguageId, source: &str) -> ExtractedFile {
    let extracted = typescript::extract(source.as_bytes(), &parse(language, source));
    assert!(
        extracted.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        extracted.diagnostics
    );
    extracted
}

fn ts(source: &str) -> ExtractedFile {
    extract_with(LanguageId::Typescript, source)
}

fn tsx(source: &str) -> ExtractedFile {
    extract_with(LanguageId::Tsx, source)
}

/// The byte offset of the `nth` (0-based) occurrence of `needle`.
fn at(source: &str, needle: &str, nth: usize) -> usize {
    source
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("{needle:?} #{nth} not in source"))
        .0
}

/// The use whose span starts at `start`, if any.
fn use_at(extracted: &ExtractedFile, start: usize) -> Option<&ExtractedUse> {
    extracted
        .uses
        .iter()
        .find(|use_| use_.span.start_byte() as usize == start)
}

/// The use at the `nth` occurrence of `needle`, which must exist.
fn the_use<'a>(
    extracted: &'a ExtractedFile,
    source: &str,
    needle: &str,
    nth: usize,
) -> &'a ExtractedUse {
    let start = at(source, needle, nth);
    use_at(extracted, start).unwrap_or_else(|| {
        panic!(
            "no use at {needle:?} #{nth} (byte {start}); uses: {:?}",
            extracted.uses
        )
    })
}

/// Every use's `(spelling, ref_kind)` in order.
fn listing(extracted: &ExtractedFile) -> Vec<(&str, &'static str)> {
    extracted
        .uses
        .iter()
        .map(|use_| (use_.spelling.as_str(), use_.ref_kind.as_str()))
        .collect()
}

fn scope<'a>(extracted: &'a ExtractedFile, key: &str) -> &'a ExtractedScope {
    extracted
        .scopes
        .iter()
        .find(|scope| scope.scope_key == key)
        .unwrap_or_else(|| panic!("no scope {key}: {:?}", extracted.scopes))
}

/// The scope chain of `use_`, nearest first.
fn chain<'a>(extracted: &'a ExtractedFile, use_: &ExtractedUse) -> Vec<&'a ExtractedScope> {
    let mut out = Vec::new();
    let mut key = Some(use_.scope_key.clone());
    while let Some(current) = key {
        let found = scope(extracted, &current);
        key = found.parent_scope_key.clone();
        out.push(found);
    }
    out
}

/// The nearest scope on `use_`'s chain that binds `name` as a value (a
/// local), or `"import"` when the module scope's import binds it first.
fn binds(extracted: &ExtractedFile, use_: &ExtractedUse, name: &str) -> Option<String> {
    for scope in chain(extracted, use_) {
        if scope
            .facts
            .locals
            .iter()
            .any(|local| local.name == name && local.space != BindingSpace::Type)
        {
            return Some(scope.scope_key.clone());
        }
        if scope
            .facts
            .module_imports
            .iter()
            .any(|import| import.local.as_deref() == Some(name))
        {
            return Some("import".to_string());
        }
    }
    None
}

/// `(kind, local, imported, exported, specifier, type_only)` of one import.
type ImportRow<'a> = (
    ModuleImportKind,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    &'a str,
    bool,
);

fn imports(extracted: &ExtractedFile) -> &[ModuleImport] {
    &scope(extracted, typescript::MODULE_SCOPE_KEY)
        .facts
        .module_imports
}

// ---------------------------------------------------------------------------
// Shadowing and scopes.
// ---------------------------------------------------------------------------

/// A local `double` shadowing the imported `double` is in its function's
/// scope, so the local call's nearest binding is the local and the top-level
/// call's is the import.
#[test]
fn a_local_shadowing_an_import_has_distinct_scope_facts() {
    let source = "\
import { double } from \"./util\";
double(1);
export function shadowed(): number {
  const double = (n: number): number => n + n;
  return double(2);
}
export function outer(): number {
  return double(3);
}
";
    let extracted = ts(source);
    let top = the_use(&extracted, source, "double(1)", 0);
    let local = the_use(&extracted, source, "double(2)", 0);
    let other = the_use(&extracted, source, "double(3)", 0);
    for use_ in [top, local, other] {
        assert_eq!(use_.ref_kind, RefKind::Call, "{use_:?}");
    }
    assert_eq!(top.scope_key, typescript::MODULE_SCOPE_KEY);
    assert_ne!(local.scope_key, typescript::MODULE_SCOPE_KEY);
    assert_eq!(binds(&extracted, top, "double").as_deref(), Some("import"));
    assert_eq!(
        binds(&extracted, other, "double").as_deref(),
        Some("import")
    );
    assert_eq!(
        binds(&extracted, local, "double").as_deref(),
        Some(local.scope_key.as_str())
    );
    // The local declaration is a local of that scope, not a use.
    let locals = &scope(&extracted, &local.scope_key).facts.locals;
    assert!(locals.iter().any(|binding| binding.name == "double"
        && binding.space == BindingSpace::Value
        && binding.span.start_byte() as usize == at(source, "double = ", 0)));
    assert!(use_at(&extracted, at(source, "double = ", 0)).is_none());
    // The import binding sits on the local name, with an `import` use there.
    let import = &imports(&extracted)[0];
    assert_eq!(import.kind, ModuleImportKind::Named);
    assert_eq!(import.local.as_deref(), Some("double"));
    assert_eq!(import.span.start_byte() as usize, at(source, "double", 0));
    assert_eq!(
        the_use(&extracted, source, "double", 0).ref_kind,
        RefKind::Import
    );
}

/// `let`/`const`/class declarations bind in their block; `var` and function
/// declarations in the nearest function scope. A name binds for its whole
/// scope, even where it is declared after a use.
#[test]
fn block_and_function_scoping() {
    let source = "\
import { a, b, c } from \"./m\";
export function f(flag: boolean): void {
  if (flag) {
    const a = 1;
    var b = 2;
    a; b;
  }
  a; b; c;
  c;
  function c() {}
}
";
    let extracted = ts(source);
    let inner_a = the_use(&extracted, source, "a; b;", 0);
    let inner_b = the_use(&extracted, source, "b;", 0);
    let outer_a = the_use(&extracted, source, "a; b; c;", 0);
    let outer_b = the_use(&extracted, source, "b; c;", 0);
    let outer_c = the_use(&extracted, source, "c;\n  c;", 0);
    let function_scope = outer_a.scope_key.clone();
    assert_ne!(inner_a.scope_key, function_scope, "the block has a scope");
    assert_eq!(
        binds(&extracted, inner_a, "a").as_deref(),
        Some(inner_a.scope_key.as_str())
    );
    assert_eq!(binds(&extracted, outer_a, "a").as_deref(), Some("import"));
    // `var b` hoists to the function: both uses see the local.
    assert_eq!(
        binds(&extracted, inner_b, "b"),
        Some(function_scope.clone())
    );
    assert_eq!(
        binds(&extracted, outer_b, "b"),
        Some(function_scope.clone())
    );
    // `function c` is declared after the use, and still binds.
    assert_eq!(binds(&extracted, outer_c, "c"), Some(function_scope));
    // A block that binds nothing opens no scope.
    let plain = ts("export function g(): void {\n  {\n    h();\n  }\n}\n");
    let call = &plain.uses[0];
    assert_eq!(call.spelling, "h");
    assert_eq!(chain(&plain, call).len(), 2, "{:?}", plain.scopes);
}

/// A value local does not hide a type use, and a type parameter does not hide
/// a value use: TypeScript keeps the two spaces apart.
#[test]
fn values_and_types_bind_in_separate_spaces() {
    let source = "\
import { Config } from \"./config\";
export function load<T>(Config: string, raw: T): Config {
  return parse(Config, raw as T);
}
";
    let extracted = ts(source);
    let function_scope = &the_use(&extracted, source, "Config, raw", 0).scope_key;
    let locals = &scope(&extracted, function_scope).facts.locals;
    let space = |name: &str| {
        locals
            .iter()
            .find(|local| local.name == name)
            .map(|local| local.space)
    };
    assert_eq!(space("Config"), Some(BindingSpace::Value));
    assert_eq!(space("T"), Some(BindingSpace::Type));
    assert_eq!(space("raw"), Some(BindingSpace::Value));
    // The return type is a `type` use, not the parameter.
    let returned = the_use(&extracted, source, "Config {", 0);
    assert_eq!(returned.ref_kind, RefKind::Type);
    assert_eq!(
        the_use(&extracted, source, "Config, raw", 0).ref_kind,
        RefKind::Unknown
    );
    assert_eq!(the_use(&extracted, source, "T)", 1).ref_kind, RefKind::Type);
}

/// Scope keys and parents are deterministic, every scope a use names exists,
/// and the chain always ends at the module scope.
#[test]
fn every_scope_chain_reaches_the_module_scope() {
    let source = "\
export namespace N {
  export class C<T> {
    static { init(); }
    m(): void {
      for (const x of xs) { try { use(x); } catch (e) { report(e); } }
      switch (k) { case 1: { const y = 1; y; } }
    }
  }
}
type Keys<T> = { [K in keyof T]: T[K] };
type Elem<T> = T extends Array<infer U> ? U : never;
";
    let extracted = ts(source);
    assert!(!extracted.uses.is_empty());
    for use_ in &extracted.uses {
        let found = chain(&extracted, use_);
        assert_eq!(
            found.last().map(|scope| scope.scope_key.as_str()),
            Some(typescript::MODULE_SCOPE_KEY),
            "{use_:?}"
        );
    }
    // Mapped and inferred names bind a type locally.
    let k = the_use(&extracted, source, "K]", 0);
    assert_eq!(k.ref_kind, RefKind::Type);
    assert!(
        scope(&extracted, &k.scope_key)
            .facts
            .locals
            .iter()
            .any(|local| local.name == "K" && local.space == BindingSpace::Type)
    );
    let u = the_use(&extracted, source, "U :", 0);
    assert!(chain(&extracted, u).iter().any(|scope| {
        scope
            .facts
            .locals
            .iter()
            .any(|local| local.name == "U" && local.space == BindingSpace::Type)
    }));
    // Same bytes, same scopes.
    assert_eq!(format!("{extracted:?}"), format!("{:?}", ts(source)));
}

// ---------------------------------------------------------------------------
// Containers.
// ---------------------------------------------------------------------------

/// Anonymous functions and classes are transparent: a use inside an IIFE or a
/// callback in an anonymous default export has no container, and one inside a
/// callback in a named function belongs to that function.
#[test]
fn anonymous_containers_are_transparent() {
    let source = "\
export default function () {
  [1].map((value) => helper(value));
  (function () { helper(2); })();
}
export function named(): void {
  [1].forEach(() => { helper(3); });
}
(() => helper(4))();
";
    let extracted = ts(source);
    for (needle, container) in [
        ("helper(value)", None),
        ("helper(2)", None),
        ("helper(3)", Some("named")),
        ("helper(4)", None),
    ] {
        let use_ = the_use(&extracted, source, needle, 0);
        let found = use_
            .containing_symbol_index
            .map(|index| extracted.symbols[index].qualified_name.as_str());
        assert_eq!(found, container, "{needle}");
    }
    let tsx_source = "export default () => <Panel onOpen={() => open(1)} />;\n";
    let extracted = tsx(tsx_source);
    for use_ in &extracted.uses {
        assert_eq!(use_.containing_symbol_index, None, "{use_:?}");
    }
    assert_eq!(listing(&extracted), [("Panel", "call"), ("open", "call")]);
}

/// A use in a class member's initializer or a namespace belongs to the
/// nearest function, method, class, interface, or enum symbol; a namespace,
/// a `const`, and a property are not containers.
#[test]
fn containers_follow_the_php_rule() {
    let source = "\
export namespace N {
  export const x = make();
  export class K {
    field = build();
    method(): void { run(); }
  }
}
";
    let extracted = ts(source);
    let container = |needle: &str| {
        the_use(&extracted, source, needle, 0)
            .containing_symbol_index
            .map(|index| extracted.symbols[index].qualified_name.clone())
    };
    assert_eq!(container("make"), None);
    assert_eq!(container("build").as_deref(), Some("N.K"));
    assert_eq!(container("run").as_deref(), Some("N.K.method"));
}

// ---------------------------------------------------------------------------
// Literal text, templates, and JSX.
// ---------------------------------------------------------------------------

/// JSX text, attribute names, string attribute values, lowercase intrinsic
/// elements, and closing tag names are not uses; `{expr}` and capitalized or
/// dotted component names are.
#[test]
fn jsx_text_and_attributes_are_not_uses() {
    let source = "\
export function View() {
  return (
    <section title=\"Button\" data-x=\"launch\">
      Button launch text
      <Button label=\"Launch\" onPress={() => launch(count)} {...rest} />
      <ui.Card>{count}</ui.Card>
      <my-element />
    </section>
  );
}
";
    let extracted = tsx(source);
    assert_eq!(
        listing(&extracted),
        [
            ("Button", "call"),
            ("launch", "call"),
            ("count", "unknown"),
            ("rest", "unknown"),
            ("ui", "unknown"),
            ("Card", "call"),
            ("count", "unknown"),
        ]
    );
    let card = the_use(&extracted, source, "Card>", 0);
    assert_eq!(card.receiver.as_deref(), Some("ui"));
    // Nothing inside the text, the strings, or the closing tag.
    for needle in [
        "Button launch text",
        "\"Button\"",
        "\"launch\"",
        "\"Launch\"",
        "</ui.Card>",
    ] {
        let start = at(source, needle, 0);
        let end = start + needle.len();
        assert!(
            extracted.uses.iter().all(|use_| {
                (use_.span.end_byte() as usize) <= start || (use_.span.start_byte() as usize) >= end
            }),
            "{needle}"
        );
    }
}

/// A template interpolation is code; the text around it is not.
#[test]
fn template_interpolation_is_code() {
    let source = "export const s = `launch ${a} and ${b.c(d)} launch`;\nconst t = tag`x ${e}`;\n";
    let extracted = ts(source);
    assert_eq!(
        listing(&extracted),
        [
            ("a", "unknown"),
            ("b", "unknown"),
            ("c", "call"),
            ("d", "unknown"),
            ("tag", "call"),
            ("e", "unknown"),
        ]
    );
    assert!(extracted.uses.iter().all(|use_| use_.spelling != "launch"));
    // Plain strings and comments hold no uses.
    let quiet = ts("// launch()\nconst x = \"launch()\" + 'launch';\n/* launch */\n");
    assert!(quiet.uses.is_empty(), "{:?}", quiet.uses);
}

/// Real-shaped TSX: props destructuring, a map callback, and conditional
/// rendering.
#[test]
fn real_shaped_tsx() {
    let source = "\
import { Row } from \"./Row\";
import type { Item } from \"./types\";

type Props = { items: Item[]; empty?: boolean };

export const List = ({ items, empty = false }: Props) => (
  <ul>
    {items.length === 0 && !empty ? <Empty /> : null}
    {items.map((item) => (
      <Row key={item.id} item={item} onSelect={() => select(item.id)} />
    ))}
  </ul>
);
";
    let extracted = tsx(source);
    let list = extracted
        .symbols
        .iter()
        .position(|symbol| symbol.qualified_name == "List")
        .expect("List");
    let expected: &[(&str, &str)] = &[
        ("Row", "import"),
        ("Item", "import"),
        ("Item", "type"),
        ("Props", "type"),
        ("items", "unknown"),
        ("length", "read"),
        ("empty", "unknown"),
        ("Empty", "call"),
        ("items", "unknown"),
        ("map", "call"),
        ("Row", "call"),
        ("item", "unknown"),
        ("id", "read"),
        ("item", "unknown"),
        ("select", "call"),
        ("item", "unknown"),
        ("id", "read"),
    ];
    assert_eq!(listing(&extracted), expected);
    // Uses inside the component and its callbacks belong to `List`.
    let props = the_use(&extracted, source, "Props)", 0);
    assert_eq!(props.containing_symbol_index, Some(list));
    let select = the_use(&extracted, source, "select", 0);
    assert_eq!(select.containing_symbol_index, Some(list));
    // The destructured props are locals of the component's scope, not uses.
    let component_scope = &the_use(&extracted, source, "items.length", 0).scope_key;
    let names: Vec<&str> = scope(&extracted, component_scope)
        .facts
        .locals
        .iter()
        .map(|local| local.name.as_str())
        .collect();
    assert_eq!(names, ["items", "empty"]);
    // The map callback's parameter is a local of the callback, and the
    // attribute value `{item}` is a use that sees it.
    let value = use_at(&extracted, at(source, "item={item}", 0) + "item={".len())
        .expect("the attribute value");
    assert_eq!(value.spelling, "item");
    assert_eq!(
        binds(&extracted, value, "item").as_deref(),
        Some(value.scope_key.as_str())
    );
    // The attribute name is not a use.
    assert!(use_at(&extracted, at(source, "item={item}", 0)).is_none());
}

// ---------------------------------------------------------------------------
// Members, calls, types, and writes.
// ---------------------------------------------------------------------------

/// `a?.b()` records the receiver without `?.`, as `a.b()` does; a chain's
/// receiver is its whole object text.
#[test]
fn optional_chaining_records_the_receiver() {
    let source = "a?.b(); a.b(); a?.c?.d(); a.b().e; obj?.[k]; super_.x?.();\n";
    let extracted = ts(source);
    let receivers: Vec<(&str, &str, Option<&str>)> = extracted
        .uses
        .iter()
        .map(|use_| {
            (
                use_.spelling.as_str(),
                use_.ref_kind.as_str(),
                use_.receiver.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        receivers,
        [
            ("a", "unknown", None),
            ("b", "call", Some("a")),
            ("a", "unknown", None),
            ("b", "call", Some("a")),
            ("a", "unknown", None),
            ("c", "read", Some("a")),
            ("d", "call", Some("a?.c")),
            ("a", "unknown", None),
            ("b", "call", Some("a")),
            ("e", "read", Some("a.b()")),
            ("obj", "unknown", None),
            ("k", "unknown", None),
            ("super_", "unknown", None),
            ("x", "call", Some("super_")),
        ]
    );
}

/// Type positions: annotations, a generic argument, `typeof x`, `keyof`,
/// heritage clauses, `as`/`satisfies`, a qualified type, `new`, and
/// `instanceof`.
#[test]
fn type_position_uses() {
    let source = "\
const box: Box<Config> = make<Settings>();
let copy: typeof box;
type K = keyof Config;
class A extends Base<Config> implements Named, ns.Tagged {}
interface I extends Parent<Config> {}
const x = raw as Shape;
const y = raw satisfies Shape;
const z = new Foo<Config>();
const w = new lib.Bar();
if (z instanceof Foo) {}
let q: typeof lib.value;
";
    let extracted = ts(source);
    let kind_of = |needle: &str, nth: usize| {
        let use_ = the_use(&extracted, source, needle, nth);
        (use_.ref_kind.as_str(), use_.receiver.as_deref())
    };
    assert_eq!(kind_of("Box", 0), ("type", None));
    assert_eq!(kind_of("Config", 0), ("type", None));
    assert_eq!(kind_of("make", 0), ("call", None));
    assert_eq!(kind_of("Settings", 0), ("type", None));
    assert_eq!(kind_of("box;", 0), ("type", None));
    assert_eq!(kind_of("Config", 1), ("type", None));
    assert_eq!(kind_of("Base", 0), ("type", None));
    assert_eq!(kind_of("Config", 2), ("type", None));
    assert_eq!(kind_of("Named", 0), ("type", None));
    assert_eq!(kind_of("ns.Tagged", 0), ("unknown", None));
    assert_eq!(kind_of("Tagged", 0), ("type", Some("ns")));
    assert_eq!(kind_of("Parent", 0), ("type", None));
    assert_eq!(kind_of("Shape", 0), ("type", None));
    assert_eq!(kind_of("Shape", 1), ("type", None));
    assert_eq!(kind_of("Foo", 0), ("type", None));
    assert_eq!(kind_of("lib.Bar", 0), ("unknown", None));
    assert_eq!(kind_of("Bar", 0), ("type", Some("lib")));
    assert_eq!(kind_of("Foo", 1), ("type", None));
    assert_eq!(kind_of("z instanceof", 0), ("unknown", None));
    assert_eq!(kind_of("lib.value", 0), ("unknown", None));
    assert_eq!(kind_of("value;", 0), ("type", Some("lib")));
    // Predefined and literal types are not uses.
    assert!(
        ts("let n: number | \"a\" | undefined | null;\n")
            .uses
            .is_empty()
    );
}

/// Assignment targets are writes: a bare name, a member (plain, compound,
/// update), and every name a destructuring assignment writes.
#[test]
fn writes() {
    let source = "x = 1; a.b = 2; a.c += 3; d++; --e.f; [g, h] = pair; ({ i, j: k } = o);\n";
    let extracted = ts(source);
    assert_eq!(
        listing(&extracted),
        [
            ("x", "write"),
            ("a", "unknown"),
            ("b", "write"),
            ("a", "unknown"),
            ("c", "write"),
            ("d", "write"),
            ("e", "unknown"),
            ("f", "write"),
            ("g", "write"),
            ("h", "write"),
            ("pair", "unknown"),
            ("i", "write"),
            ("k", "write"),
            ("o", "unknown"),
        ]
    );
}

/// Declaration names are never uses: functions, classes, members,
/// parameters, variables, destructured names, type parameters, enum members,
/// object keys, labels, and an index signature's key.
#[test]
fn declaration_names_are_not_uses() {
    let source = "\
export function f<T>(a: number, { b, c: d = e }: Opts, ...rest: number[]): void {
  const [g, h] = pair;
  label: for (let i = 0; i < 1; i++) { break label; }
}
export class K { m(p: number): void {} static s = 1; #q = 2; }
export enum E { One, Two = One }
export interface I { [key: string]: V; prop: W; method(arg: X): void; }
const o = { key: value, [computed]: 1, short, method() {} };
";
    let extracted = ts(source);
    assert_eq!(
        listing(&extracted),
        [
            ("e", "unknown"),
            ("Opts", "type"),
            ("pair", "unknown"),
            ("i", "unknown"),
            ("i", "write"),
            ("One", "unknown"),
            ("V", "type"),
            ("W", "type"),
            ("X", "type"),
            ("value", "unknown"),
            ("computed", "unknown"),
            ("short", "unknown"),
        ]
    );
    // An enum member's initializer names its sibling in the enum's scope.
    let one = the_use(&extracted, source, "One }", 0);
    assert!(
        scope(&extracted, &one.scope_key)
            .facts
            .locals
            .iter()
            .any(|local| local.name == "One")
    );
}

/// A decorator is applied by being called.
#[test]
fn decorators_are_calls() {
    let source = "\
@Component({ selector: tag })
export class View {
  @Input() name = \"\";
  @observable count = 0;
  @lib.track() method(): void {}
}
";
    let extracted = ts(source);
    assert_eq!(
        listing(&extracted),
        [
            ("Component", "call"),
            ("tag", "unknown"),
            ("Input", "call"),
            ("observable", "call"),
            ("lib", "unknown"),
            ("track", "call"),
        ]
    );
}

// ---------------------------------------------------------------------------
// Imports and exports.
// ---------------------------------------------------------------------------

/// Every import form is one binding with an `import` use on its local name;
/// re-exports are marked and bind nothing; `export *` has no use;
/// specifiers are kept as written; `require(...)` is a call, not an import.
#[test]
fn import_and_re_export_forms() {
    let source = "\
import def, { named, orig as alias, default as other, type T, \"a-b\" as ab } from \"./rel\";
import * as ns from \"@/alias/path\";
import type { Only } from \"lodash\";
import type Def2 from \"pkg/sub\";
import legacy = require(\"./legacy\");
import \"./side-effect\";
export { x as y, z } from \"./re\";
export type { W } from \"./types\";
export { default as D, default } from \"./def\";
export * from \"./star\";
export * as all from \"./all\";
export { local as exported };
const cjs = require(\"./cjs\");
";
    let extracted = ts(source);
    let found: Vec<ImportRow<'_>> = imports(&extracted)
        .iter()
        .map(|import| {
            (
                import.kind,
                import.local.as_deref(),
                import.imported.as_deref(),
                import.exported.as_deref(),
                import.specifier.as_str(),
                import.type_only,
            )
        })
        .collect();
    use ModuleImportKind::*;
    assert_eq!(
        found,
        [
            (Default, Some("def"), Some("default"), None, "./rel", false),
            (Named, Some("named"), Some("named"), None, "./rel", false),
            (Named, Some("alias"), Some("orig"), None, "./rel", false),
            (
                Default,
                Some("other"),
                Some("default"),
                None,
                "./rel",
                false
            ),
            (Named, Some("T"), Some("T"), None, "./rel", true),
            (Named, Some("ab"), Some("a-b"), None, "./rel", false),
            (Namespace, Some("ns"), None, None, "@/alias/path", false),
            (Named, Some("Only"), Some("Only"), None, "lodash", true),
            (
                Default,
                Some("Def2"),
                Some("default"),
                None,
                "pkg/sub",
                true
            ),
            (Require, Some("legacy"), None, None, "./legacy", false),
            (ReExport, None, Some("x"), Some("y"), "./re", false),
            (ReExport, None, Some("z"), Some("z"), "./re", false),
            (ReExport, None, Some("W"), Some("W"), "./types", true),
            (ReExport, None, Some("default"), Some("D"), "./def", false),
            (
                ReExport,
                None,
                Some("default"),
                Some("default"),
                "./def",
                false
            ),
            (ReExportAll, None, None, None, "./star", false),
            (ReExportAll, None, None, Some("all"), "./all", false),
        ]
    );
    // Uses: one `import` use per local binding (the alias when there is one)
    // and per re-exported identifier other than `default`; a local export
    // names its local; `require` is a call.
    assert_eq!(
        listing(&extracted),
        [
            ("def", "import"),
            ("named", "import"),
            ("alias", "import"),
            ("other", "import"),
            ("T", "import"),
            ("ab", "import"),
            ("ns", "import"),
            ("Only", "import"),
            ("Def2", "import"),
            ("legacy", "import"),
            ("x", "import"),
            ("z", "import"),
            ("W", "import"),
            ("local", "unknown"),
            ("require", "call"),
        ]
    );
    // Each import use is the span of its binding.
    for use_ in extracted
        .uses
        .iter()
        .filter(|use_| use_.ref_kind == RefKind::Import)
    {
        assert!(
            imports(&extracted)
                .iter()
                .any(|import| import.span == use_.span),
            "{use_:?}"
        );
        assert_eq!(use_.containing_symbol_index, None);
    }
    // Imports are not locals, and bind nothing in PHP's list.
    assert!(extracted.imports.is_empty());
    assert!(
        scope(&extracted, typescript::MODULE_SCOPE_KEY)
            .facts
            .locals
            .iter()
            .all(|local| local.name == "cjs")
    );
}

// ---------------------------------------------------------------------------
// Receiver hints.
// ---------------------------------------------------------------------------

/// The hints T45 needs: `this` inside a named class, a `const` bound by
/// `new`, and an explicit parameter, field, parameter property, or variable
/// annotation. Nothing else carries a hint.
#[test]
fn receiver_hints() {
    let source = "\
export class Service {
  private repo: Repo;
  private maybe: Repo | null = null;
  static shared: Cache;
  constructor(private readonly log: Logger, plain: Plain) {
    this.repo.save();
    this.log.write();
    plain.use();
    log.flush();
  }
  run(arg: Arg, loose: Arg[], opt?: Opt): void {
    this.save();
    this.maybe.save();
    arg.go();
    loose.go();
    opt?.go();
    const made = new Maker();
    made.go();
    let later = new Maker();
    later.go();
    const typed: Typed = build();
    typed.go();
    const both: Declared = new Other();
    both.go();
    const inner = function () { this.go(); };
    const arrow = () => this.go();
    const object = { m() { this.go(); } };
    const Local = class { m() { this.go(); } };
  }
  static make(): void {
    this.shared.clear();
  }
}
function free(this: Window) {
  this.go();
}
";
    let extracted = ts(source);
    // The type-name span is checked separately below.
    let typed = |spelling: &str, origin: TypedOrigin| UseHint::Typed {
        type_spelling: spelling.to_string(),
        origin,
        name_span: None,
    };
    let new_expr = |spelling: &str| UseHint::NewExpr {
        class_spelling: spelling.to_string(),
        use_block: None,
        name_span: None,
    };
    let this = UseHint::This {
        is_static: Some(false),
    };
    let static_this = UseHint::This {
        is_static: Some(true),
    };
    // (line, member, receiver, hint)
    let expected: Vec<(usize, &str, &str, UseHint)> = vec![
        (6, "repo", "this", this.clone()),
        (6, "save", "this.repo", typed("Repo", TypedOrigin::Property)),
        (
            7,
            "write",
            "this.log",
            typed("Logger", TypedOrigin::Property),
        ),
        (8, "use", "plain", typed("Plain", TypedOrigin::Parameter)),
        (9, "flush", "log", typed("Logger", TypedOrigin::Parameter)),
        (12, "save", "this", this.clone()),
        // A union type records no hint.
        (13, "save", "this.maybe", UseHint::Unresolved),
        (14, "go", "arg", typed("Arg", TypedOrigin::Parameter)),
        // An array type records no hint.
        (15, "go", "loose", UseHint::Unresolved),
        (16, "go", "opt", typed("Opt", TypedOrigin::Parameter)),
        (18, "go", "made", new_expr("Maker")),
        // A `let` can be reassigned.
        (20, "go", "later", UseHint::Unresolved),
        (22, "go", "typed", typed("Typed", TypedOrigin::Variable)),
        // The annotation wins over `new`.
        (24, "go", "both", typed("Declared", TypedOrigin::Variable)),
        // A `function` has its own `this`; an arrow keeps the method's.
        (25, "go", "this", UseHint::Unresolved),
        (26, "go", "this", this.clone()),
        // An object-literal method, and an anonymous class.
        (27, "go", "this", UseHint::Unresolved),
        (28, "go", "this", UseHint::Unresolved),
        // A static method's `this` is the class: static fields.
        (31, "shared", "this", static_this),
        (
            31,
            "clear",
            "this.shared",
            typed("Cache", TypedOrigin::Property),
        ),
        // A free function's `this` names nothing.
        (35, "go", "this", UseHint::Unresolved),
    ];
    let line_of = |byte: u32| source[..byte as usize].matches('\n').count() + 1;
    for (line, member, receiver, hint) in expected {
        let found: Vec<&ExtractedUse> = extracted
            .uses
            .iter()
            .filter(|use_| {
                line_of(use_.span.start_byte()) == line
                    && use_.spelling == member
                    && use_.receiver.as_deref() == Some(receiver)
            })
            .collect();
        assert_eq!(found.len(), 1, "line {line} {receiver}.{member}: {found:?}");
        let (got, name_span) = without_name_span(&found[0].hint);
        assert_eq!(got, hint, "line {line} {receiver}.{member}");
        // T45: a typed or `new` hint points at the `type` use of its name.
        if let UseHint::Typed { type_spelling, .. }
        | UseHint::NewExpr {
            class_spelling: type_spelling,
            ..
        } = &got
        {
            let span = name_span.unwrap_or_else(|| panic!("line {line}: no name span"));
            let at: Vec<&ExtractedUse> = extracted
                .uses
                .iter()
                .filter(|use_| use_.span == span)
                .collect();
            assert_eq!(at.len(), 1, "line {line}: {at:?}");
            assert_eq!(at[0].ref_kind, RefKind::Type, "line {line}");
            assert_eq!(&at[0].spelling, type_spelling, "line {line}");
        }
    }
    // A hint rides only on a member use: a bare name gets none.
    for use_ in extracted.uses.iter().filter(|use_| use_.receiver.is_none()) {
        assert_eq!(use_.hint, UseHint::Unresolved, "{use_:?}");
    }
}

/// A receiver bound twice in its nearest scope, or bound by an import, gets no
/// hint; a nearer binding hides a farther one.
#[test]
fn receiver_hints_follow_the_nearest_binding() {
    let source = "\
import { imported } from \"./m\";
export function f(svc: Service): void {
  imported.go();
  {
    const svc = make();
    svc.go();
  }
  svc.go();
  var twice = new A();
  var twice = new B();
  twice.go();
}
";
    let extracted = ts(source);
    let hint = |needle: &str, nth: usize| the_use(&extracted, source, needle, nth).hint.clone();
    assert_eq!(hint("go", 0), UseHint::Unresolved, "an import");
    assert_eq!(
        hint("go", 1),
        UseHint::Unresolved,
        "a nearer unhinted local"
    );
    assert_eq!(
        hint("go", 2),
        UseHint::Typed {
            type_spelling: "Service".to_string(),
            origin: TypedOrigin::Parameter,
            name_span: Some(the_use(&extracted, source, "Service", 0).span),
        }
    );
    assert_eq!(hint("go", 3), UseHint::Unresolved, "bound twice");
}

/// T45: the module scope records the side of every class and interface
/// member symbol; a constructor, an anonymous class's members, and every
/// other scope record none.
#[test]
fn member_sides_are_recorded_in_the_module_scope() {
    let source = "\
export class K {
  static count = 0;
  #hidden = 1;
  name: string = \"\";
  constructor(private readonly dep: Dep, plain: Plain) {}
  static make(): K { return new K(null!, null!); }
  run(): void {}
  get label(): string { return \"\"; }
  set label(v: string) {}
  static get total(): number { return 0; }
  static(): void {}
}
export interface I {
  m(): void;
  p: number;
}
export const Anon = class {
  x(): void {}
};
export default class {
  y(): void {}
}
";
    let extracted = ts(source);
    let top = scope(&extracted, typescript::MODULE_SCOPE_KEY);
    let mut sides: Vec<(String, bool)> = top
        .facts
        .member_sides
        .iter()
        .map(|side| {
            (
                extracted.symbols[side.symbol].qualified_name.clone(),
                side.is_static,
            )
        })
        .collect();
    sides.sort();
    let want: Vec<(String, bool)> = [
        ("Anon.x", false),
        ("I.m", false),
        ("I.p", false),
        ("K.#hidden", false),
        ("K.count", true),
        ("K.dep", false),
        ("K.label", false),
        ("K.label", false),
        ("K.make", true),
        ("K.name", false),
        ("K.run", false),
        // A method named `static` is an instance method.
        ("K.static", false),
        ("K.total", true),
    ]
    .iter()
    .map(|(name, is_static)| (name.to_string(), *is_static))
    .collect();
    assert_eq!(sides, want);
    // In symbol order, once each.
    let order: Vec<usize> = top
        .facts
        .member_sides
        .iter()
        .map(|side| side.symbol)
        .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(order, sorted);
    for other in extracted
        .scopes
        .iter()
        .filter(|scope| scope.scope_key != typescript::MODULE_SCOPE_KEY)
    {
        assert!(other.facts.member_sides.is_empty(), "{other:?}");
    }
}

/// `hint` with its type-name span taken out, and that span (T45).
fn without_name_span(hint: &UseHint) -> (UseHint, Option<Span>) {
    let mut hint = hint.clone();
    let span = match &mut hint {
        UseHint::Typed { name_span, .. } | UseHint::NewExpr { name_span, .. } => name_span.take(),
        _ => None,
    };
    (hint, span)
}

// ---------------------------------------------------------------------------
// Grammar variants, limits, and determinism.
// ---------------------------------------------------------------------------

/// Code valid under both grammars extracts identically with either.
#[test]
fn both_grammar_variants_extract_the_same_facts() {
    let source = "\
import { a } from \"./a\";
export function f(x: X): Y {
  return a(x).b<Z>(`${x}`);
}
";
    assert_eq!(format!("{:?}", ts(source)), format!("{:?}", tsx(source)));
}

/// The use bound bites only when exceeded and yields one `resource_limit`
/// diagnostic and no facts.
#[test]
fn use_limit_bites_only_when_exceeded() {
    let source = "f(); g(); h();\n";
    let tree = parse(LanguageId::Typescript, source);
    let at_bound = typescript::extract_with_limits(
        source.as_bytes(),
        &tree,
        ResourceLimits {
            max_extracted_uses: 3,
            ..ResourceLimits::DEFAULT
        },
    );
    assert!(at_bound.diagnostics.is_empty());
    assert_eq!(at_bound.uses.len(), 3);

    let over = typescript::extract_with_limits(
        source.as_bytes(),
        &tree,
        ResourceLimits {
            max_extracted_uses: 2,
            ..ResourceLimits::DEFAULT
        },
    );
    assert_eq!(over.diagnostics.len(), 1);
    assert_eq!(over.diagnostics[0].code, "resource_limit");
    assert_eq!(over.diagnostics[0].detail, "extracted more than 2 uses");
    assert!(over.symbols.is_empty() && over.uses.is_empty() && over.scopes.is_empty());
}
