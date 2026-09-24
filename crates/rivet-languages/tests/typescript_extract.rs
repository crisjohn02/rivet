//! T42 TypeScript/TSX named-definition extraction: adversarial cases. T43's
//! use, scope, and import cases are in `typescript_uses.rs`.
//!
//! The authored fixture is compared with its gold in `typescript_gold.rs`.
//! These tests pin the rules the fixture does not reach on its own: overload
//! folding with and without an implementation, accessor pairs, declaration
//! merging, nested and dotted namespaces, a class expression bound to a
//! `const`, named and anonymous default exports, `declare global`, TSX
//! components, Unicode names, the constructs that are never symbols, doc
//! comments, and the parse and resource policies.

#![cfg(feature = "lang-typescript")]

use rivet_core::{ExtractedFile, ExtractedSymbol, SymbolId, SymbolKind, assign_ordinals};
use rivet_languages::{LanguageId, ResourceLimits, grammar, language_for_path, typescript};
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

/// Extracts `source` as the file `path`, with the grammar its extension
/// dispatches to.
fn extract_as(path: &str, source: &str) -> ExtractedFile {
    let language = language_for_path(path).expect("a TypeScript-family path");
    typescript::extract(source.as_bytes(), &parse(language, source))
}

/// Extracts `source` as a `.ts` file and requires it to parse cleanly.
fn extract(source: &str) -> ExtractedFile {
    clean(extract_as("a.ts", source))
}

fn clean(extracted: ExtractedFile) -> ExtractedFile {
    assert!(
        extracted.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        extracted.diagnostics
    );
    extracted
}

/// Every symbol's canonical ID in file `path`, computed as refresh computes
/// them: spec §10.1 escaping and duplicate ordinals from `rivet_core`.
fn ids(path: &str, symbols: &[ExtractedSymbol]) -> Vec<String> {
    let items: Vec<_> = symbols
        .iter()
        .map(|symbol| (symbol.qualified_name.as_str(), symbol.span, symbol.kind))
        .collect();
    symbols
        .iter()
        .zip(assign_ordinals(&items))
        .map(|(symbol, ordinal)| {
            SymbolId::new(path, &symbol.qualified_name, ordinal)
                .expect("valid id")
                .as_canonical()
        })
        .collect()
}

/// `(id without the path, kind)` for every symbol, in extraction order.
fn listing(extracted: &ExtractedFile) -> Vec<(String, &'static str)> {
    ids("a.ts", &extracted.symbols)
        .into_iter()
        .zip(&extracted.symbols)
        .map(|(id, symbol)| {
            (
                id.strip_prefix("a.ts#").expect("path prefix").to_string(),
                symbol.kind.as_str(),
            )
        })
        .collect()
}

fn owned(expected: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    expected
        .iter()
        .map(|(id, kind)| (id.to_string(), *kind))
        .collect()
}

/// The one symbol with this qualified name.
fn symbol<'a>(extracted: &'a ExtractedFile, qualified_name: &str) -> &'a ExtractedSymbol {
    let found: Vec<_> = extracted
        .symbols
        .iter()
        .filter(|symbol| symbol.qualified_name == qualified_name)
        .collect();
    assert_eq!(found.len(), 1, "{qualified_name}: {found:?}");
    found[0]
}

fn slice(source: &str, span: rivet_core::Span) -> &str {
    &source[span.start_byte() as usize..span.end_byte() as usize]
}

/// Checks the invariants every extracted symbol keeps: the name span is the
/// name's own bytes inside the declaration span, a parent precedes its member
/// and prefixes its qualified name, and symbols are in start order.
fn check_invariants(source: &str, extracted: &ExtractedFile) {
    let symbols = &extracted.symbols;
    for (index, symbol) in symbols.iter().enumerate() {
        let name_span = symbol.name_span.expect("TypeScript records a name span");
        assert_eq!(slice(source, name_span), symbol.name, "{symbol:?}");
        assert!(
            symbol.span.start_byte() <= name_span.start_byte()
                && name_span.end_byte() <= symbol.span.end_byte(),
            "{symbol:?}"
        );
        assert!(
            symbol.qualified_name == symbol.name
                || symbol
                    .qualified_name
                    .ends_with(&format!(".{}", symbol.name)),
            "{symbol:?}"
        );
        if let Some(parent) = symbol.parent_index {
            assert!(parent < index, "{symbol:?}");
            let parent = &symbols[parent];
            assert_eq!(
                symbol.qualified_name,
                format!("{}.{}", parent.qualified_name, symbol.name)
            );
            assert!(
                parent.span.start_byte() <= symbol.span.start_byte()
                    && symbol.span.end_byte() <= parent.span.end_byte()
            );
        }
        if index > 0 {
            let previous = &symbols[index - 1];
            assert!(
                (previous.span.start_byte(), previous.span.end_byte())
                    <= (symbol.span.start_byte(), symbol.span.end_byte())
            );
        }
    }
    // T43: every use is its spelling's bytes, in a recorded scope whose
    // parent chain reaches the module scope; PHP's import list stays empty.
    assert!(extracted.imports.is_empty());
    let keys: std::collections::BTreeMap<&str, Option<&str>> = extracted
        .scopes
        .iter()
        .map(|scope| (scope.scope_key.as_str(), scope.parent_scope_key.as_deref()))
        .collect();
    for use_ in &extracted.uses {
        assert_eq!(slice(source, use_.span), use_.spelling, "{use_:?}");
        let mut key = Some(use_.scope_key.as_str());
        let mut depth = 0;
        while let Some(current) = key {
            assert!(keys.contains_key(current), "{use_:?}: no scope {current}");
            key = keys[current];
            depth += 1;
            assert!(depth <= keys.len(), "{use_:?}: scope cycle");
        }
        if let Some(container) = use_.containing_symbol_index {
            let container = &symbols[container];
            assert!(
                container.span.start_byte() <= use_.span.start_byte()
                    && use_.span.end_byte() <= container.span.end_byte(),
                "{use_:?}"
            );
        }
    }
}

/// An overload set with an implementation is one symbol: the
/// implementation's span and signature. Functions, methods, static methods,
/// and constructors fold alike.
#[test]
fn overloads_fold_into_the_implementation() {
    let source = "\
export function f(a: string): string;
export function f(a: number): number;
export function f(a: any): any {
  return a;
}
class K {
  m(): void;
  m(a?: number): void {}
  static s(): void;
  static s() {}
  constructor(a: string);
  constructor(private a: any) {}
}
";
    let extracted = extract(source);
    check_invariants(source, &extracted);
    assert_eq!(
        listing(&extracted),
        owned(&[
            ("f", "function"),
            ("K", "class"),
            ("K.m", "method"),
            ("K.s", "method"),
            ("K.constructor", "method"),
            ("K.a", "property"),
        ])
    );
    let f = symbol(&extracted, "f");
    assert_eq!(
        slice(source, f.span),
        "export function f(a: any): any {\n  return a;\n}"
    );
    assert_eq!(
        f.signature.as_deref(),
        Some("export function f(a: any): any")
    );
    assert_eq!(
        symbol(&extracted, "K.m").signature.as_deref(),
        Some("m(a?: number): void")
    );
    assert_eq!(
        symbol(&extracted, "K.s").signature.as_deref(),
        Some("static s()")
    );
    assert_eq!(
        symbol(&extracted, "K.constructor").signature.as_deref(),
        Some("constructor(private a: any)")
    );
}

/// Without an implementation, as in a `.d.ts` file, an ambient declaration,
/// an interface, or abstract methods, the first signature is the symbol and
/// the rest are not symbols. Overloads fold only within one body: two merged
/// interfaces keep a same-name member each, with ordinals.
#[test]
fn overloads_without_an_implementation_keep_the_first_signature() {
    let source = "\
declare function g(a: string): void;
declare function g(a: number): void;
interface I {
  m(): void;
  m(a: string): void;
}
declare class W {
  r(): void;
  r(a: number): void;
}
abstract class A {
  abstract x(): void;
  abstract x(a: number): void;
}
declare namespace N {
  function h(): void;
  function h(a: number): void;
}
interface J {
  m(): void;
}
interface J {
  m(a: string): void;
}
";
    let extracted = clean(extract_as("types.d.ts", source));
    check_invariants(source, &extracted);
    assert_eq!(
        listing_for("types.d.ts", &extracted),
        owned(&[
            ("g", "function"),
            ("I", "interface"),
            ("I.m", "method"),
            ("W", "class"),
            ("W.r", "method"),
            ("A", "class"),
            ("A.x", "method"),
            ("N", "module"),
            ("N.h", "function"),
            ("J#1", "interface"),
            ("J.m#1", "method"),
            ("J#2", "interface"),
            ("J.m#2", "method"),
        ])
    );
    assert_eq!(
        symbol(&extracted, "g").signature.as_deref(),
        Some("declare function g(a: string): void")
    );
    assert_eq!(
        symbol(&extracted, "I.m").signature.as_deref(),
        Some("m(): void")
    );
    assert_eq!(
        symbol(&extracted, "A.x").signature.as_deref(),
        Some("abstract x(): void")
    );
    assert_eq!(
        slice(source, symbol(&extracted, "N.h").span),
        "function h(): void;"
    );
}

fn listing_for(path: &str, extracted: &ExtractedFile) -> Vec<(String, &'static str)> {
    let prefix = format!("{path}#");
    ids(path, &extracted.symbols)
        .into_iter()
        .zip(&extracted.symbols)
        .map(|(id, symbol)| {
            (
                id.strip_prefix(&prefix).expect("path prefix").to_string(),
                symbol.kind.as_str(),
            )
        })
        .collect()
}

/// A getter and a setter are each a `method`; a shared name gets ordinals in
/// source order, and the signature keeps `get`/`set`. A method *named* `get`
/// is an ordinary method, and interface accessor signatures never fold.
#[test]
fn accessor_pair_is_two_methods_with_ordinals() {
    let source = "\
export class P {
  get v(): number {
    return 1;
  }
  set v(x: number) {}
  static get w(): number {
    return 2;
  }
  get(): void {}
}
interface Q {
  get u(): number;
  set u(x: number);
}
";
    let extracted = extract(source);
    check_invariants(source, &extracted);
    assert_eq!(
        listing(&extracted),
        owned(&[
            ("P", "class"),
            ("P.v#1", "method"),
            ("P.v#2", "method"),
            ("P.w", "method"),
            ("P.get", "method"),
            ("Q", "interface"),
            ("Q.u#1", "method"),
            ("Q.u#2", "method"),
        ])
    );
    let signatures: Vec<_> = extracted
        .symbols
        .iter()
        .map(|symbol| symbol.signature.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(
        signatures,
        vec![
            "export class P",
            "get v(): number",
            "set v(x: number)",
            "static get w(): number",
            "get(): void",
            "interface Q",
            "get u(): number",
            "set u(x: number)",
        ]
    );
}

/// Declaration merging: two interfaces or two namespaces with one name get
/// ordinals, and each member's parent is its own block.
#[test]
fn declaration_merging_gets_ordinals() {
    let source = "\
export interface S {
  a: string;
}
export interface S {
  b: number;
}
export namespace N {
  export const x = 1;
}
export namespace N {
  export const y = 2;
}
";
    let extracted = extract(source);
    check_invariants(source, &extracted);
    assert_eq!(
        listing(&extracted),
        owned(&[
            ("S#1", "interface"),
            ("S.a", "property"),
            ("S#2", "interface"),
            ("S.b", "property"),
            ("N#1", "module"),
            ("N.x", "const"),
            ("N#2", "module"),
            ("N.y", "const"),
        ])
    );
    assert_eq!(symbol(&extracted, "S.b").parent_index, Some(2));
    assert_eq!(symbol(&extracted, "N.y").parent_index, Some(6));
}

/// Namespaces nest lexically, exported or not. A dotted namespace name is one
/// `module` whose short name is its last segment.
#[test]
fn nested_namespaces_nest_their_qualified_names() {
    let source = "\
export namespace Outer {
  export namespace Inner {
    export namespace Deepest {
      export function f(): void {}
    }
  }
  namespace Hidden {
    export const k = 1;
  }
}
namespace A.B.C {
  export const z = 1;
}
";
    let extracted = extract(source);
    check_invariants(source, &extracted);
    assert_eq!(
        listing(&extracted),
        owned(&[
            ("Outer", "module"),
            ("Outer.Inner", "module"),
            ("Outer.Inner.Deepest", "module"),
            ("Outer.Inner.Deepest.f", "function"),
            ("Outer.Hidden", "module"),
            ("Outer.Hidden.k", "const"),
            ("A.B.C", "module"),
            ("A.B.C.z", "const"),
        ])
    );
    let parents: Vec<_> = extracted
        .symbols
        .iter()
        .map(|symbol| symbol.parent_index)
        .collect();
    assert_eq!(
        parents,
        vec![
            None,
            Some(0),
            Some(1),
            Some(2),
            Some(0),
            Some(4),
            None,
            Some(6)
        ]
    );
    // A doc comment above a bare (expression-statement) namespace attaches.
    let documented = extract("/** Bare. */\nnamespace X {\n  /** Inner. */\n  namespace Y {}\n}\n");
    let docs: Vec<_> = documented
        .symbols
        .iter()
        .map(|symbol| symbol.doc_comment.as_deref())
        .collect();
    assert_eq!(docs, vec![Some("/** Bare. */"), Some("/** Inner. */")]);
    let dotted = symbol(&extracted, "A.B.C");
    assert_eq!(dotted.name, "C");
    assert_eq!(
        slice(source, dotted.span),
        "namespace A.B.C {\n  export const z = 1;\n}"
    );
    assert_eq!(dotted.signature.as_deref(), Some("namespace A.B.C"));
}

/// `const X = class {}` is one `class` symbol named `X`, by the arrow-function
/// rule, and its members are `X.member`. A class expression not bound to a
/// `const` is anonymous: neither it nor its members are symbols.
#[test]
fn class_expression_bound_to_a_const_is_a_class() {
    let source = "\
export const X = class Y extends Base {
  m(): void {}
  f = 1;
};
const Z = class {};
foo(class Hidden {
  h() {}
});
";
    let extracted = extract(source);
    check_invariants(source, &extracted);
    assert_eq!(
        listing(&extracted),
        owned(&[
            ("X", "class"),
            ("X.m", "method"),
            ("X.f", "property"),
            ("Z", "class"),
        ])
    );
    let x = symbol(&extracted, "X");
    assert_eq!(
        x.signature.as_deref(),
        Some("export const X = class Y extends Base")
    );
    assert!(slice(source, x.span).starts_with("export const X"));
    assert!(slice(source, x.span).ends_with("};"));
}

/// A default export with a name is an ordinary symbol whose span includes
/// `export default`; an anonymous one creates no symbol, and neither do its
/// members (the PHP anonymous-class rule), even under the grammar quirk that
/// reads a following parenthesized statement as a call of the class.
#[test]
fn default_exports_named_and_anonymous() {
    let named = "\
/** Makes a label. */
export default function helper(): string {
  return \"\";
}
";
    let extracted = extract(named);
    check_invariants(named, &extracted);
    assert_eq!(listing(&extracted), owned(&[("helper", "function")]));
    let helper = symbol(&extracted, "helper");
    assert_eq!(slice(named, helper.span), named[22..].trim_end());
    assert_eq!(
        helper.signature.as_deref(),
        Some("export default function helper(): string")
    );
    assert_eq!(helper.doc_comment.as_deref(), Some("/** Makes a label. */"));

    let class = "export default class Named {\n  m() {}\n}\n";
    assert_eq!(
        listing(&extract(class)),
        owned(&[("Named", "class"), ("Named.m", "method")])
    );

    for anonymous in [
        "export default class {\n  run(): number {\n    return 1;\n  }\n  field = 2;\n}\n",
        // The pinned grammar reads this pair as one call expression.
        "export default class {\n  run() {}\n}\n(function () {})();\n",
        "export default function () {\n  return 1;\n}\n",
        "export default { method() {}, arrow: () => 1 };\n",
        "export default (class Inner {\n  m() {}\n});\n",
    ] {
        let extracted = extract(anonymous);
        assert!(
            extracted.symbols.is_empty(),
            "{anonymous:?}: {:?}",
            extracted.symbols
        );
    }
}

/// In a `.d.ts` file, `declare global` is no symbol and its declarations are
/// named as top-level ones. A string-named ambient module and its members are
/// not symbols; an identifier-named one is a `module`.
#[test]
fn declaration_file_with_declare_global() {
    let source = "\
export {};
declare global {
  interface Window {
    rivet: string;
  }
  function g(): void;
  namespace NodeJS {
    interface Global {
      x: number;
    }
  }
}
declare module \"pkg\" {
  export function start(): void;
}
declare module \"shorthand\";
declare module Legacy {
  function old(): void;
}
";
    let extracted = clean(extract_as("src/globals.d.ts", source));
    check_invariants(source, &extracted);
    assert_eq!(
        listing_for("src/globals.d.ts", &extracted),
        owned(&[
            ("Window", "interface"),
            ("Window.rivet", "property"),
            ("g", "function"),
            ("NodeJS", "module"),
            ("NodeJS.Global", "interface"),
            ("NodeJS.Global.x", "property"),
            ("Legacy", "module"),
            ("Legacy.old", "function"),
        ])
    );
    assert_eq!(symbol(&extracted, "Window").parent_index, None);
    assert_eq!(
        symbol(&extracted, "Legacy").signature.as_deref(),
        Some("declare module Legacy")
    );
}

/// A `.tsx` file uses the TSX grammar: a function component and a `const`
/// arrow component are `function` symbols; a component declared inside a
/// function and an anonymous default-exported component are not symbols.
#[test]
fn tsx_components_are_functions() {
    let source = "\
export function Greeting({ name }: { name: string }) {
  const Inner = () => <b>{name}</b>;
  return (
    <div>
      <Inner />
    </div>
  );
}
export const Badge = (props: { label: string }) => <span>{props.label}</span>;
export default function () {
  return <Greeting name=\"x\" />;
}
";
    assert!(
        parse(LanguageId::Typescript, source)
            .root_node()
            .has_error(),
        "the sample must need the TSX grammar"
    );
    let extracted = clean(extract_as("src/Greeting.tsx", source));
    check_invariants(source, &extracted);
    assert_eq!(
        listing_for("src/Greeting.tsx", &extracted),
        owned(&[("Greeting", "function"), ("Badge", "function")])
    );
    assert_eq!(
        symbol(&extracted, "Badge").signature.as_deref(),
        Some("export const Badge = (props: { label: string }) =>")
    );
}

/// Unicode identifiers keep their spelling, and spans are UTF-8 byte offsets.
#[test]
fn unicode_identifiers_keep_bytes_and_spelling() {
    let source = "\
export class Café {
  naïve = \"é\";
  grüßen(): string {
    return \"ß\";
  }
}
export const π = 3.14;
";
    let extracted = extract(source);
    check_invariants(source, &extracted);
    assert_eq!(
        listing(&extracted),
        owned(&[
            ("Café", "class"),
            ("Café.naïve", "property"),
            ("Café.grüßen", "method"),
            ("π", "const"),
        ])
    );
    let naive = symbol(&extracted, "Café.naïve");
    let start = source.find("naïve").expect("naïve") as u32;
    assert_eq!(naive.span.start_byte(), start);
    assert_eq!(slice(source, naive.span), "naïve = \"é\"");
    let pi = symbol(&extracted, "π");
    assert_eq!(slice(source, pi.name_span.expect("name span")), "π");
    assert_eq!(pi.signature.as_deref(), Some("export const π = 3.14"));
}

/// A file with an ERROR or MISSING node yields no facts and one
/// `parse_error`, as PHP does.
#[test]
fn broken_files_yield_no_facts() {
    let missing = "export function broken(value: number {\n  return value;\n}\n";
    let extracted = extract_as("src/broken.ts", missing);
    assert!(extracted.symbols.is_empty(), "{:?}", extracted.symbols);
    assert_eq!(
        extracted.diagnostics.len(),
        1,
        "{:?}",
        extracted.diagnostics
    );
    assert_eq!(extracted.diagnostics[0].code, "parse_error");
    assert_eq!(extracted.diagnostics[0].detail, ") at byte 36");
    assert_eq!(extracted.diagnostics[0].start_byte, Some(36));

    // `export default abstract class {}` is an ERROR node in the pinned
    // grammar; the valid declarations around it are not published either.
    let error = "export class Fine {}\nexport default abstract class {}\n";
    let extracted = extract_as("src/error.ts", error);
    assert!(extracted.symbols.is_empty());
    assert_eq!(extracted.diagnostics.len(), 1);
    assert_eq!(extracted.diagnostics[0].code, "parse_error");
    assert!(
        extracted.diagnostics[0]
            .detail
            .starts_with("ERROR at byte ")
    );

    // JSX under the plain TypeScript grammar is a parse failure.
    let jsx = "export function A() {\n  return <div />;\n}\n";
    let extracted = extract_as("src/A.ts", jsx);
    assert!(extracted.symbols.is_empty());
    assert_eq!(extracted.diagnostics[0].code, "parse_error");
}

/// The node bound bites only when exceeded, and it is checked before the parse
/// policy, as for PHP.
#[test]
fn node_limit_is_checked_first_and_bites_only_when_exceeded() {
    let source = "export function f(): void {}\n";
    let tree = parse(LanguageId::Typescript, source);
    let mut cursor = tree.walk();
    let mut nodes = 0_u64;
    'walk: loop {
        nodes += 1;
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    let at = ResourceLimits {
        max_visited_nodes: nodes,
        ..ResourceLimits::DEFAULT
    };
    let ok = typescript::extract_with_limits(source.as_bytes(), &tree, at);
    assert!(ok.diagnostics.is_empty());
    assert_eq!(ok.symbols.len(), 1);

    let below = ResourceLimits {
        max_visited_nodes: nodes - 1,
        ..ResourceLimits::DEFAULT
    };
    let limited = typescript::extract_with_limits(source.as_bytes(), &tree, below);
    assert!(limited.symbols.is_empty());
    assert_eq!(limited.diagnostics.len(), 1);
    assert_eq!(limited.diagnostics[0].code, "resource_limit");
    assert_eq!(
        limited.diagnostics[0].detail,
        format!("visited more than {} Tree-sitter nodes", nodes - 1)
    );

    let broken = "export function broken(value: number {\n}\n";
    let broken_tree = parse(LanguageId::Typescript, broken);
    let limited = typescript::extract_with_limits(
        broken.as_bytes(),
        &broken_tree,
        ResourceLimits {
            max_visited_nodes: 2,
            ..ResourceLimits::DEFAULT
        },
    );
    assert_eq!(limited.diagnostics[0].code, "resource_limit");
}

/// Declarations inside a function body or a top-level block, object-literal
/// methods, `let`/`var` bindings, destructuring `const`s, and generic type
/// parameters are never symbols.
#[test]
fn locals_blocks_objects_and_generics_are_not_symbols() {
    let source = "\
export function outer<T>(value: T): T {
  const inner = () => 1;
  function nested() {}
  class Local {
    m() {}
  }
  interface LocalI {
    x: number;
  }
  type LocalT = number;
  enum LocalE {
    A,
  }
  return value;
}
if (flag) {
  function inBlock() {}
}
export const obj = { method() {}, arrow: () => 1 };
let l = () => 1;
var v = 1;
export const { a, b } = source;
export type Pair<A, B> = [A, B];
class G<T extends object> {}
";
    let extracted = extract(source);
    check_invariants(source, &extracted);
    assert_eq!(
        listing(&extracted),
        owned(&[
            ("outer", "function"),
            ("obj", "const"),
            ("Pair", "type_alias"),
            ("G", "class"),
        ])
    );
    assert_eq!(
        symbol(&extracted, "Pair").signature.as_deref(),
        Some("export type Pair<A, B> = [A, B]")
    );
}

/// ES private names keep `#` and escape it in the ID (spec §10.1). String,
/// number, and computed member names, index signatures, and static blocks are
/// not symbols. Parameter properties are the parameters with an accessibility
/// modifier, `readonly`, or `override`. Enum members are `const`s.
#[test]
fn member_forms() {
    let source = "\
class M extends Base {
  #secret = 1;
  #hidden(): void {}
  \"quoted\" = 2;
  42 = 3;
  [key] = 4;
  static {
    init();
  }
  [index: string]: unknown;
  constructor(private a: string, readonly b: number, public override c = 1, d: number, protected e?: string) {
    super();
  }
}
enum E {
  A,
  B = 2,
  \"c-d\" = 3,
}
";
    let extracted = extract(source);
    check_invariants(source, &extracted);
    assert_eq!(
        listing(&extracted),
        owned(&[
            ("M", "class"),
            ("M.%23secret", "property"),
            ("M.%23hidden", "method"),
            ("M.constructor", "method"),
            ("M.a", "property"),
            ("M.b", "property"),
            ("M.c", "property"),
            ("M.e", "property"),
            ("E", "enum"),
            ("E.A", "const"),
            ("E.B", "const"),
        ])
    );
    assert_eq!(symbol(&extracted, "M.#secret").name, "#secret");
    assert_eq!(
        symbol(&extracted, "M.c").signature.as_deref(),
        Some("public override c = 1")
    );
    assert_eq!(
        symbol(&extracted, "E.B").signature.as_deref(),
        Some("B = 2")
    );
}

/// Only a `/** ... */` comment directly above attaches: not a `//` comment,
/// not `/**/`, not across a blank line. A method's decorators sit between its
/// doc comment and the method and are skipped; a field's decorators are inside
/// the field node, so they are part of its span.
#[test]
fn doc_comments_attach_by_the_php_rule() {
    let source = "\
/**
 * Documented.
 */
export class D {
  /** Field doc. */
  f = 1;

  /** Separated. */

  g = 2;
  // plain
  h = 3;
  /** Decorated. */
  @dec()
  m(): void {}
  /**/
  n = 4;
  /** Input. */
  @Input() name: string;
}
";
    let extracted = extract(source);
    check_invariants(source, &extracted);
    let docs: Vec<_> = extracted
        .symbols
        .iter()
        .map(|symbol| (symbol.name.as_str(), symbol.doc_comment.as_deref()))
        .collect();
    assert_eq!(
        docs,
        vec![
            ("D", Some("/**\n * Documented.\n */")),
            ("f", Some("/** Field doc. */")),
            ("g", None),
            ("h", None),
            ("m", Some("/** Decorated. */")),
            ("n", None),
            ("name", Some("/** Input. */")),
        ]
    );
    // Member spans follow the grammar: a method's excludes its decorator, a
    // field's includes it.
    assert_eq!(
        slice(source, symbol(&extracted, "D.m").span),
        "m(): void {}"
    );
    let field = symbol(&extracted, "D.name");
    assert_eq!(slice(source, field.span), "@Input() name: string");
    assert_eq!(field.signature.as_deref(), Some("@Input() name: string"));
}

/// The same bytes give the same records, and the case-sensitive lookup name is
/// the name as written.
#[test]
fn extraction_is_deterministic_and_lookup_is_case_sensitive() {
    let source = "\
export type SurveyId = number | string;
export type surveyId = string;
export class Survey {}
";
    let first = extract(source);
    let second = extract(source);
    assert_eq!(first, second);
    assert_eq!(
        listing(&first),
        owned(&[
            ("SurveyId", "type_alias"),
            ("surveyId", "type_alias"),
            ("Survey", "class"),
        ])
    );
    let alias = &first.symbols[0];
    assert_eq!(alias.kind, SymbolKind::TypeAlias);
    assert_eq!(
        alias.signature.as_deref(),
        Some("export type SurveyId = number | string")
    );
    assert_eq!(typescript::lookup_name(&alias.name, alias.kind), "SurveyId");
    assert_ne!(
        typescript::lookup_name("SurveyId", SymbolKind::TypeAlias),
        typescript::lookup_name("surveyId", SymbolKind::TypeAlias)
    );
}
