//! GR1: the two constructs the vendored, patched `tree-sitter-typescript`
//! (`0.23.2-rivet.1`, `vendor/tree-sitter-typescript/RIVET-PATCHES.md`) parses
//! and upstream 0.23.2 rejected, extracted end to end by the TypeScript adapter.
//!
//! 1. Generic call signatures separated only by a newline in an interface or
//!    object type. A `<` on a new line between members starts a call
//!    signature; right after a member's name it still opens that method
//!    signature's type parameters, and outside object types (expressions,
//!    `return`, class bodies, variable annotations) it continues the previous
//!    line exactly as before.
//! 2. `export type * from` and `export type * as ns from` (TypeScript 5.0): a
//!    type-only `ReExportAll` module import, like `export *`, never a binding;
//!    without `from` it is still a parse error.
//!
//! Both grammars (TypeScript and TSX) are exercised.

#![cfg(feature = "lang-typescript")]

use rivet_core::extract::{ModuleImport, ModuleImportKind};
use rivet_core::{ExtractedFile, RefKind, SymbolKind};
use rivet_languages::{LanguageId, grammar, typescript};
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

fn extract(language: LanguageId, source: &str) -> ExtractedFile {
    typescript::extract(source.as_bytes(), &parse(language, source))
}

/// Extracts `source` with `language`'s grammar and requires a clean parse.
fn clean(language: LanguageId, source: &str) -> ExtractedFile {
    let extracted = extract(language, source);
    assert!(
        extracted.diagnostics.is_empty(),
        "{language:?}: unexpected diagnostics: {:?}",
        extracted.diagnostics
    );
    extracted
}

const BOTH: [LanguageId; 2] = [LanguageId::Typescript, LanguageId::Tsx];

/// `(qualified name, kind)` of every symbol, in extraction order.
fn symbols(extracted: &ExtractedFile) -> Vec<(&str, SymbolKind)> {
    extracted
        .symbols
        .iter()
        .map(|symbol| (symbol.qualified_name.as_str(), symbol.kind))
        .collect()
}

fn imports(extracted: &ExtractedFile) -> &[ModuleImport] {
    &extracted
        .scopes
        .iter()
        .find(|scope| scope.scope_key == typescript::MODULE_SCOPE_KEY)
        .expect("module scope")
        .facts
        .module_imports
}

type ImportRow<'a> = (
    ModuleImportKind,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    &'a str,
    bool,
);

fn import_rows(extracted: &ExtractedFile) -> Vec<ImportRow<'_>> {
    imports(extracted)
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
        .collect()
}

// ---------------------------------------------------------------------------
// Construct 1: newline-separated generic call signatures.
// ---------------------------------------------------------------------------

/// Hono's `src/context.ts` shape: an interface whose generic call signatures
/// are separated only by newlines. It parses, and the interface is a symbol;
/// call signatures are never symbols.
#[test]
fn newline_separated_generic_call_signatures_parse() {
    let source = "\
export interface Get<E> {
  <Key extends keyof E>(key: Key): E[Key]
  <Key extends string>(key: Key): unknown
  (): void
  <T>(value: T)
  // a comment between members
  <T>(value: T): Map<T, string>
  <T>(value: T): T[]
}
";
    for language in BOTH {
        let extracted = clean(language, source);
        assert_eq!(
            symbols(&extracted),
            [("Get", SymbolKind::Interface)],
            "{language:?}"
        );
    }
}

/// The same in a type alias's object type, mixed with named members, a
/// construct signature, and `;`/`,` separators, which keep working. (A type
/// literal's members are not symbols; the tree shows each member.)
#[test]
fn newline_separated_generic_call_signatures_in_an_object_type() {
    let source = "\
type Factory = {
  name: string
  <T>(value: T): T
  new <T>(value: T): Factory
  <T>(value: T, extra: number): T;
  <T>(): T,
  size(): number
  <T>(value: T): T
}
";
    for language in BOTH {
        let extracted = clean(language, source);
        assert_eq!(
            symbols(&extracted),
            [("Factory", SymbolKind::TypeAlias)],
            "{language:?}"
        );
        let tree = parse(language, source);
        let object_type = tree
            .root_node()
            .named_child(0)
            .and_then(|alias| alias.child_by_field_name("value"))
            .expect("object type");
        let mut cursor = object_type.walk();
        let members: Vec<&str> = object_type
            .named_children(&mut cursor)
            .map(|member| member.kind())
            .collect();
        assert_eq!(
            members,
            [
                "property_signature",
                "call_signature",
                "construct_signature",
                "call_signature",
                "call_signature",
                "method_signature",
                "call_signature",
            ],
            "{language:?}"
        );
    }
}

/// A type never takes type arguments across a line break (TypeScript reads
/// `a: Foo` then a call signature), so a generic call signature after a type
/// that could take type arguments is a new member, not `Foo<K>(...)`.
#[test]
fn a_generic_call_signature_after_a_generic_capable_type_is_a_new_member() {
    let source = "\
interface Loader {
  cache: Store
  <K>(key: K): Promise
  <K>(key: K): typeof load
  <K>(key: K): K
}
";
    for language in BOTH {
        let extracted = clean(language, source);
        assert_eq!(
            symbols(&extracted),
            [
                ("Loader", SymbolKind::Interface),
                ("Loader.cache", SymbolKind::Property),
            ],
            "{language:?}"
        );
    }
}

/// Right after a member's name, a `<` on the next line still opens that
/// method signature's type parameters, as upstream and TypeScript read it:
/// each name is a method, not a property followed by a call signature. (A
/// string-named member is not a symbol; the tree shows it.)
#[test]
fn type_parameters_on_the_line_after_a_member_name_stay_a_method() {
    let source = "\
interface Box {
  map
  <U>(f: U): Box
  flat?
  <U>(f: U): Box
  'quoted'
  <U>(): U
}
";
    for language in BOTH {
        let extracted = clean(language, source);
        assert_eq!(
            symbols(&extracted),
            [
                ("Box", SymbolKind::Interface),
                ("Box.map", SymbolKind::Method),
                ("Box.flat", SymbolKind::Method),
            ],
            "{language:?}"
        );
        let sexp = parse(language, source).root_node().to_sexp();
        assert_eq!(sexp.matches("(method_signature").count(), 3, "{sexp}");
        assert!(!sexp.contains("call_signature"), "{sexp}");
    }
}

/// Outside an object type the patch changes nothing: `a\n<b>(c)` is still
/// one generic call, a class method's type parameters may start on the next
/// line, and a variable annotation's type arguments still continue across
/// the line (as upstream parses them).
#[test]
fn a_less_than_on_a_new_line_outside_object_types_keeps_its_parse() {
    let source = "\
declare function a<T>(x: T): void;
declare const c: number;
a
<string>(c)
class K {
  m
  <T>(x: T): T {
    return x;
  }
}
declare const y: Map<string, number>;
let x: Map
<string, number> = y;
";
    let extracted = clean(LanguageId::Typescript, source);
    assert_eq!(
        symbols(&extracted),
        [
            ("a", SymbolKind::Function),
            ("c", SymbolKind::Const),
            ("K", SymbolKind::Class),
            ("K.m", SymbolKind::Method),
            ("y", SymbolKind::Const),
        ]
    );
    let tree = parse(LanguageId::Typescript, source);
    let sexp = tree.root_node().to_sexp();
    assert!(
        sexp.contains(
            "(expression_statement (call_expression function: (identifier) \
             type_arguments: (type_arguments (predefined_type)) arguments: (arguments (identifier))))"
        ),
        "{sexp}"
    );
    assert!(
        sexp.contains(
            "type: (type_annotation (generic_type name: (type_identifier) \
             type_arguments: (type_arguments (predefined_type) (predefined_type))))"
        ),
        "{sexp}"
    );
}

/// In TSX, `return` followed by JSX on the next line keeps its upstream
/// parse (a `return` of the element).
#[test]
fn tsx_return_of_jsx_on_the_next_line_keeps_its_parse() {
    let source = "\
export function View() {
  return
  <p />
}
";
    clean(LanguageId::Tsx, source);
    let sexp = parse(LanguageId::Tsx, source).root_node().to_sexp();
    assert!(
        sexp.contains("(return_statement (jsx_self_closing_element name: (identifier)))"),
        "{sexp}"
    );
}

// ---------------------------------------------------------------------------
// Construct 2: `export type * from`.
// ---------------------------------------------------------------------------

/// `export type * from` is a type-only `ReExportAll` like `export *`, and
/// `export type * as ns from` carries the exported namespace name. Neither
/// binds a local name or records an `import` use.
#[test]
fn export_type_star_is_a_type_only_re_export_all() {
    let source = "\
export type * from './types'
export type * as ns from \"./ns\";
export type * as 'quoted name' from './q'
export * from './values'
";
    use ModuleImportKind::*;
    for language in BOTH {
        let extracted = clean(language, source);
        assert_eq!(
            import_rows(&extracted),
            [
                (ReExportAll, None, None, None, "./types", true),
                (ReExportAll, None, None, Some("ns"), "./ns", true),
                (ReExportAll, None, None, Some("quoted name"), "./q", true),
                (ReExportAll, None, None, None, "./values", false),
            ],
            "{language:?}"
        );
        // The `export *` span is the `*` token; `export * as` spans the name.
        let starts: Vec<usize> = imports(&extracted)
            .iter()
            .map(|import| import.span.start_byte() as usize)
            .collect();
        assert_eq!(
            starts,
            [
                source.find("* from './types'").unwrap(),
                source.find("ns from").unwrap(),
                source.find("'quoted name'").unwrap(),
                source.find("* from './values'").unwrap(),
            ],
            "{language:?}"
        );
        assert!(
            extracted
                .uses
                .iter()
                .all(|use_| use_.ref_kind != RefKind::Import),
            "{language:?}: {:?}",
            extracted.uses
        );
        assert!(extracted.symbols.is_empty(), "{language:?}");
    }
}

/// `export type *` needs a `from` clause: without one, or combined with a
/// clause, the file is a parse failure and yields no facts.
#[test]
fn export_type_star_without_a_source_is_a_parse_error() {
    for source in [
        "export type *\n",
        "export type * as ns;\n",
        "export type * as ns, { a } from './x'\n",
        "export type *, { a } from './x'\n",
    ] {
        for language in BOTH {
            let extracted = extract(language, source);
            assert!(
                !extracted.diagnostics.is_empty(),
                "{language:?}: {source:?} should be a parse error"
            );
            assert!(extracted.symbols.is_empty() && extracted.uses.is_empty());
            assert!(extracted.scopes.is_empty(), "{language:?}: {source:?}");
        }
    }
}
