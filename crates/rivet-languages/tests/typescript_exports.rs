//! T44 TypeScript facts: module exports, value-position `type` uses, and
//! global declarations kept out of `declares`.
//!
//! The resolver reads these facts; its behavior over the authored fixture is
//! checked in `rivet-index/tests/typescript_gold.rs` and end to end in
//! `rivet-cli/tests/typescript_bindings.rs`. These tests pin the facts
//! themselves, form by form.

#![cfg(feature = "lang-typescript")]

use rivet_core::extract::{ExtractedScope, ModuleExport};
use rivet_core::{ExtractedFile, RefKind};
use rivet_languages::{LanguageId, grammar, typescript};
use tree_sitter::Parser;

fn ts(source: &str) -> ExtractedFile {
    let mut parser = Parser::new();
    parser
        .set_language(&grammar(LanguageId::Typescript))
        .expect("pinned grammar must load");
    let tree = parser
        .parse(source.as_bytes(), None)
        .expect("parser must return a tree");
    let extracted = typescript::extract(source.as_bytes(), &tree);
    assert!(
        extracted.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        extracted.diagnostics
    );
    extracted
}

fn module_scope(extracted: &ExtractedFile) -> &ExtractedScope {
    extracted
        .scopes
        .iter()
        .find(|scope| scope.scope_key == typescript::MODULE_SCOPE_KEY)
        .expect("a module scope")
}

/// `(exported, local, type_only, text at span)` of each export of `scope`.
fn exports<'a>(
    source: &'a str,
    scope: &'a ExtractedScope,
) -> Vec<(&'a str, Option<&'a str>, bool, &'a str)> {
    scope
        .facts
        .module_exports
        .iter()
        .map(|export: &ModuleExport| {
            (
                export.exported.as_str(),
                export.local.as_deref(),
                export.type_only,
                &source[export.span.start_byte() as usize..export.span.end_byte() as usize],
            )
        })
        .collect()
}

#[test]
fn every_local_export_form_is_recorded() {
    let source = "\
export function f(): void {}
export function g(value: number): void;
export class C {}
export abstract class Abs {}
export interface I {}
export type T = number;
export enum E { A }
export declare const D: number;
export declare function h(): void;
export const p = 1, q = 2;
export let l = 1;
export var v = 1;
export const { r, s: renamed, t = 3, ...rest } = { r: 1, s: 2 };
export namespace A.B {}
export import Al = A.B;
function helper(): void {}
const a = 1;
export { a, a as b, helper as default, a as \"string name\" };
export type { I as J };
export { type T as U };
export {};
";
    let extracted = ts(source);
    let got = exports(source, module_scope(&extracted));
    assert_eq!(
        got,
        [
            ("f", Some("f"), false, "f"),
            ("g", Some("g"), false, "g"),
            ("C", Some("C"), false, "C"),
            ("Abs", Some("Abs"), false, "Abs"),
            ("I", Some("I"), false, "I"),
            ("T", Some("T"), false, "T"),
            ("E", Some("E"), false, "E"),
            ("D", Some("D"), false, "D"),
            ("h", Some("h"), false, "h"),
            ("p", Some("p"), false, "p"),
            ("q", Some("q"), false, "q"),
            ("l", Some("l"), false, "l"),
            ("v", Some("v"), false, "v"),
            ("r", Some("r"), false, "r"),
            ("renamed", Some("renamed"), false, "renamed"),
            ("t", Some("t"), false, "t"),
            ("rest", Some("rest"), false, "rest"),
            ("A", Some("A"), false, "A"),
            ("Al", Some("Al"), false, "Al"),
            ("a", Some("a"), false, "a"),
            ("b", Some("a"), false, "a"),
            ("default", Some("helper"), false, "helper"),
            ("string name", Some("a"), false, "a"),
            ("J", Some("I"), true, "I"),
            ("U", Some("T"), true, "T"),
        ]
    );
    // A local specifier's name is also its `unknown` use, at the same span;
    // a declared name is no use.
    let clauses: Vec<(u32, u32)> = [
        "export { a, a as b, helper as default, a as \"string name\" };",
        "export type { I as J };",
        "export { type T as U };",
    ]
    .iter()
    .map(|clause| {
        let start = source.find(clause).expect("the clause") as u32;
        (start, start + clause.len() as u32)
    })
    .collect();
    for export in &module_scope(&extracted).facts.module_exports {
        let in_clause = clauses.iter().any(|&(start, end)| {
            start <= export.span.start_byte() && export.span.end_byte() <= end
        });
        let is_use = extracted
            .uses
            .iter()
            .any(|use_| use_.span == export.span && use_.ref_kind == RefKind::Unknown);
        assert_eq!(is_use, in_clause, "{export:?}");
    }
}

#[test]
fn default_exports_record_a_local_name_only_for_a_named_declaration_or_identifier() {
    let cases: [(&str, Option<&str>, &str); 10] = [
        (
            "export default function named(): void {}\n",
            Some("named"),
            "named",
        ),
        ("export default class Named {}\n", Some("Named"), "Named"),
        ("export default abstract class Abs {}\n", Some("Abs"), "Abs"),
        (
            "export default interface Shape {}\n",
            Some("Shape"),
            "Shape",
        ),
        ("const x = 1;\nexport default x;\n", Some("x"), "x"),
        ("export default () => {};\n", None, "default"),
        ("export default class {}\n", None, "default"),
        ("export default function () {}\n", None, "default"),
        ("const x = 1;\nexport default (x);\n", None, "default"),
        (
            "const o = { x: 1 };\nexport default o.x;\n",
            None,
            "default",
        ),
    ];
    for (source, local, text) in cases {
        let extracted = ts(source);
        assert_eq!(
            exports(source, module_scope(&extracted)),
            [("default", local, false, text)],
            "{source}"
        );
    }
    // `export =` and `export as namespace` are not ES exports.
    for source in [
        "const x = 1;\nexport = x;\n",
        "export as namespace Lib;\nexport {};\n",
    ] {
        let extracted = ts(source);
        assert!(
            module_scope(&extracted).facts.module_exports.is_empty(),
            "{source}"
        );
    }
}

#[test]
fn only_a_modules_own_scope_records_exports() {
    let source = "\
export namespace N {
  export const inner = 1;
}
declare global {
  export interface G {}
}
declare module \"ambient\" {
  export function start(): void;
}
export { x } from \"./m\";
export * from \"./m\";
export * as all from \"./m\";
export const top = 1;
";
    let extracted = ts(source);
    // Re-exports stay imports; a namespace member and a global are not
    // module exports.
    assert_eq!(
        exports(source, module_scope(&extracted)),
        [
            ("N", Some("N"), false, "N"),
            ("top", Some("top"), false, "top")
        ]
    );
    assert_eq!(module_scope(&extracted).facts.module_imports.len(), 3);
    // The string-named ambient module's body is that module's own scope.
    let ambient: Vec<_> = extracted
        .scopes
        .iter()
        .filter(|scope| scope.scope_key != typescript::MODULE_SCOPE_KEY)
        .map(|scope| exports(source, scope))
        .filter(|exports| !exports.is_empty())
        .collect();
    assert_eq!(ambient, [vec![("start", Some("start"), false, "start")]]);
    // Only that body is an ambient module body.
    let flagged: Vec<usize> = extracted
        .scopes
        .iter()
        .filter(|scope| scope.facts.ambient_module)
        .map(|scope| scope.facts.module_exports.len())
        .collect();
    assert_eq!(flagged, [1]);
}

#[test]
fn type_uses_that_name_values_are_recorded_in_their_scope() {
    let source = "\
import * as ns from \"./m\";
export class K extends Base implements Iface {}
export class K2 extends ns.Base {}
export const made = new Foo();
export const madeQualified = new ns.Foo();
export const test = made instanceof Bar;
export type Q = typeof val;
export type Q2 = typeof ns.val;
export let annotated: Ann;
export function f(): void {
  new Inner();
}
";
    let extracted = ts(source);
    let named = |scope: &ExtractedScope| -> Vec<String> {
        scope
            .facts
            .value_type_uses
            .iter()
            .map(|span| {
                let text = &source[span.start_byte() as usize..span.end_byte() as usize];
                let use_ = extracted
                    .uses
                    .iter()
                    .find(|use_| use_.span == *span)
                    .unwrap_or_else(|| panic!("no use at {span:?}"));
                assert_eq!(use_.ref_kind, RefKind::Type, "{text}");
                assert_eq!(use_.scope_key, scope.scope_key, "{text}");
                format!(
                    "{}{text}",
                    use_.receiver
                        .as_deref()
                        .map_or(String::new(), |r| format!("{r}."))
                )
            })
            .collect()
    };
    assert_eq!(
        named(module_scope(&extracted)),
        ["Base", "ns.Base", "Foo", "ns.Foo", "Bar", "val", "ns.val"]
    );
    let inner: Vec<Vec<String>> = extracted
        .scopes
        .iter()
        .filter(|scope| scope.scope_key != typescript::MODULE_SCOPE_KEY)
        .map(named)
        .filter(|names| !names.is_empty())
        .collect();
    assert_eq!(inner, [vec!["Inner".to_string()]]);
}

/// The canonical-ID-free view of a scope's `declares`: the declared names.
fn declared(extracted: &ExtractedFile, scope: &ExtractedScope) -> Vec<String> {
    scope
        .facts
        .declares
        .iter()
        .map(|&index| extracted.symbols[index].qualified_name.clone())
        .collect()
}

#[test]
fn global_declarations_are_locals_but_never_declares() {
    // A script: no top-level import or export.
    let script = ts("\
function f(): void {}
interface G {}
namespace N {
  export const x = 1;
}
enum E { A }
");
    for scope in &script.scopes {
        assert!(declared(&script, scope).is_empty(), "{scope:?}");
    }
    let names: Vec<&str> = module_scope(&script)
        .facts
        .locals
        .iter()
        .map(|local| local.name.as_str())
        .collect();
    assert_eq!(names, ["f", "G", "N", "E"]);
    // f, G, N, N.x, E, E.A.
    assert_eq!(script.symbols.len(), 6, "the globals are still symbols");

    // A module: `declare global` members are globals, the rest module-local.
    let module = ts("\
export function f(): void {}
declare global {
  interface W {}
  function g(): void;
}
namespace Local {
  export const y = 1;
}
");
    assert_eq!(declared(&module, module_scope(&module)), ["f", "Local"]);
    let names: Vec<&str> = module_scope(&module)
        .facts
        .locals
        .iter()
        .map(|local| local.name.as_str())
        .collect();
    assert_eq!(names, ["f", "W", "g", "Local"]);
    let body: Vec<Vec<String>> = module
        .scopes
        .iter()
        .filter(|scope| scope.scope_key != typescript::MODULE_SCOPE_KEY)
        .map(|scope| declared(&module, scope))
        .collect();
    assert!(body.contains(&vec!["Local.y".to_string()]), "{body:?}");

    // A side-effect import makes a module.
    let side = ts("import \"./side\";\nfunction h(): void {}\n");
    assert_eq!(declared(&side, module_scope(&side)), ["h"]);
}
