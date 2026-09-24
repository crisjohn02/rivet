//! T45: TypeScript receiver hints (`this`, annotations, `new`) as `scoped`
//! bindings, from the real extractor through the resolver.
//!
//! Each test writes a few small TypeScript files in memory, extracts them
//! with the TypeScript adapter, builds the rows refresh persists (as
//! `typescript_gold.rs` does), resolves them, and checks the binding of
//! chosen member uses: which declaration, and that it is `scoped`, or that
//! the use stays unresolved.

use std::collections::BTreeMap;

use rivet_core::{ExtractedFile, LineIndex, ParseStatus, Resolution, SymbolId, assign_ordinals};
use rivet_index::{ResolvedLinks, Resolver};
use rivet_languages::{language_for_path, typescript};
use rivet_store::{FileRow, ScopeRow, SymbolRow, UseRow};

/// The rows refresh would publish for a set of in-memory files.
struct Rows {
    files: Vec<FileRow>,
    symbols: Vec<SymbolRow>,
    uses: Vec<UseRow>,
    scopes: Vec<ScopeRow>,
}

fn canonical_ids(path: &str, extracted: &ExtractedFile) -> Vec<String> {
    let items: Vec<_> = extracted
        .symbols
        .iter()
        .map(|symbol| (symbol.qualified_name.as_str(), symbol.span, symbol.kind))
        .collect();
    extracted
        .symbols
        .iter()
        .zip(assign_ordinals(&items))
        .map(|(symbol, ordinal)| {
            SymbolId::new(path, &symbol.qualified_name, ordinal)
                .expect("an ID")
                .as_canonical()
        })
        .collect()
}

fn rows(files: &[(&str, &str)]) -> Rows {
    let mut rows = Rows {
        files: Vec::new(),
        symbols: Vec::new(),
        uses: Vec::new(),
        scopes: Vec::new(),
    };
    let mut next_use_id = 1_i64;
    for (path, source) in files {
        let language = language_for_path(path).expect("a TypeScript path");
        let extracted = rivet_parser::parse_file(language, source.as_bytes());
        assert!(
            extracted.diagnostics.is_empty(),
            "{path}: {:?}",
            extracted.diagnostics
        );
        rows.files.push(FileRow {
            path: path.to_string(),
            language: Some(language.name().to_string()),
            mtime_ns: 0,
            size: source.len() as u64,
            content_hash: None,
            source: None,
            parse_status: ParseStatus::Ok,
        });
        let ids = canonical_ids(path, &extracted);
        let lines = LineIndex::new(source.as_bytes());
        for (index, symbol) in extracted.symbols.iter().enumerate() {
            rows.symbols.push(SymbolRow {
                id: ids[index].clone(),
                file: path.to_string(),
                name: symbol.name.clone(),
                lookup_name: typescript::lookup_name(&symbol.name, symbol.kind),
                qualified_name: symbol.qualified_name.clone(),
                kind: symbol.kind,
                parent_id: symbol.parent_index.map(|parent| ids[parent].clone()),
                start_byte: symbol.span.start_byte(),
                end_byte: symbol.span.end_byte(),
                start_line: lines.start_line(symbol.span),
                end_line: lines.end_line(symbol.span),
                signature: symbol.signature.clone(),
                doc_comment: symbol.doc_comment.clone(),
            });
        }
        for use_ in &extracted.uses {
            let position = lines.line_col(use_.span.start_byte()).expect("a position");
            rows.uses.push(UseRow {
                use_id: Some(next_use_id),
                file: path.to_string(),
                containing_symbol: use_.containing_symbol_index.map(|index| ids[index].clone()),
                scope_key: use_.scope_key.clone(),
                spelling: use_.spelling.clone(),
                lookup_name: typescript::use_lookup_name(&use_.spelling),
                ref_kind: use_.ref_kind,
                start_byte: use_.span.start_byte(),
                end_byte: use_.span.end_byte(),
                line: position.line,
                col: position.column,
                receiver: use_.receiver.clone(),
                hint_json: serde_json::to_string(&use_.hint).expect("hint JSON"),
            });
            next_use_id += 1;
        }
        for scope in &extracted.scopes {
            // The persisted shape: symbol indices rewritten to canonical IDs.
            let mut facts = serde_json::to_value(&scope.facts).expect("facts JSON");
            facts["declares"] = scope
                .facts
                .declares
                .iter()
                .map(|index| serde_json::Value::from(ids[*index].clone()))
                .collect();
            facts["member_sides"] = scope
                .facts
                .member_sides
                .iter()
                .map(|side| {
                    serde_json::json!({"symbol": ids[side.symbol], "is_static": side.is_static})
                })
                .collect();
            rows.scopes.push(ScopeRow {
                file: path.to_string(),
                scope_key: scope.scope_key.clone(),
                parent_scope_key: scope.parent_scope_key.clone(),
                facts_json: facts.to_string(),
            });
        }
    }
    rows
}

/// A resolved in-memory project.
struct Project {
    sources: BTreeMap<String, String>,
    rows: Rows,
    links: ResolvedLinks,
}

fn project(files: &[(&str, &str)]) -> Project {
    let rows = rows(files);
    let links = resolve(&rows);
    Project {
        sources: files
            .iter()
            .map(|(path, source)| (path.to_string(), source.to_string()))
            .collect(),
        rows,
        links,
    }
}

fn resolve(rows: &Rows) -> ResolvedLinks {
    Resolver::new(&rows.files, &rows.symbols, &rows.uses, &rows.scopes).resolve_links()
}

impl Project {
    /// The use of `name` inside the `nth` (0-based) occurrence of `context`
    /// in `file`.
    fn use_at(&self, file: &str, context: &str, name: &str, nth: usize) -> &UseRow {
        let source = &self.sources[file];
        let start = source
            .match_indices(context)
            .nth(nth)
            .unwrap_or_else(|| panic!("{file}: no occurrence #{nth} of {context:?}"))
            .0;
        let offset = context
            .find(name)
            .unwrap_or_else(|| panic!("{name:?} not in {context:?}"));
        let start = (start + offset) as u32;
        let end = start + name.len() as u32;
        let found: Vec<&UseRow> = self
            .rows
            .uses
            .iter()
            .filter(|row| row.file == file && row.start_byte == start && row.end_byte == end)
            .collect();
        assert_eq!(found.len(), 1, "{file} {context:?} {name:?}: {found:?}");
        found[0]
    }

    /// The binding of that use: `Some((target, tier))`, or `None`.
    fn bound_nth(
        &self,
        file: &str,
        context: &str,
        name: &str,
        nth: usize,
    ) -> Option<(String, Resolution)> {
        let use_id = self.use_at(file, context, name, nth).use_id;
        self.links
            .bindings
            .iter()
            .find(|binding| Some(binding.use_id) == use_id)
            .map(|binding| (binding.target_id.clone(), binding.resolution))
    }

    fn bound(&self, file: &str, context: &str, name: &str) -> Option<(String, Resolution)> {
        self.bound_nth(file, context, name, 0)
    }

    /// Asserts that the use binds `target` at `scoped`.
    fn scoped(&self, file: &str, context: &str, name: &str, target: &str) {
        self.scoped_nth(file, context, name, 0, target);
    }

    fn scoped_nth(&self, file: &str, context: &str, name: &str, nth: usize, target: &str) {
        assert_eq!(
            self.bound_nth(file, context, name, nth),
            Some((target.to_string(), Resolution::Scoped)),
            "{file} {context:?} #{nth} {name:?}"
        );
    }

    /// Asserts that the use stays unresolved.
    fn unbound(&self, file: &str, context: &str, name: &str) {
        self.unbound_nth(file, context, name, 0);
    }

    fn unbound_nth(&self, file: &str, context: &str, name: &str, nth: usize) {
        assert_eq!(
            self.bound_nth(file, context, name, nth),
            None,
            "{file} {context:?} #{nth} {name:?}"
        );
    }
}

#[test]
fn this_binds_the_enclosing_class_member_never_a_same_name_one() {
    let p = project(&[
        (
            "svc.ts",
            "export class SurveyService {\n  launch(): void {}\n  relaunch(): void { this.launch(); }\n}\n",
        ),
        (
            "report.ts",
            "import { SurveyService } from \"./svc\";\n\
             export class ReportService {\n  launch(): void {}\n  \
             run(): void { this.launch(); }\n  \
             go(s: SurveyService): void { s.launch(); }\n}\n",
        ),
    ]);
    p.scoped(
        "svc.ts",
        "this.launch()",
        "launch",
        "svc.ts#SurveyService.launch",
    );
    p.scoped(
        "report.ts",
        "this.launch()",
        "launch",
        "report.ts#ReportService.launch",
    );
    p.scoped(
        "report.ts",
        "{ s.launch()",
        "launch",
        "svc.ts#SurveyService.launch",
    );
    // No TypeScript use gets a receiver class (no exclusion, spec §11.5).
    assert!(p.links.receiver_classes.is_empty());
}

#[test]
fn an_annotation_is_resolved_where_it_is_written() {
    let p = project(&[(
        "a.ts",
        "export class Foo { m(): void {} }\n\
         const a: Foo = new Foo();\n\
         namespace N {\n  class Foo { m(): void {} }\n  a.m();\n  \
         const b: Foo = new Foo();\n  b.m();\n  const c = new Foo();\n  c.m();\n}\n",
    )]);
    // `a`'s annotation names the module's `Foo`, not `N.Foo`.
    p.scoped("a.ts", "a.m()", "m", "a.ts#Foo.m");
    p.scoped("a.ts", "b.m()", "m", "a.ts#N.Foo.m");
    p.scoped("a.ts", "c.m()", "m", "a.ts#N.Foo.m");

    // With no module-level `Foo`, `a` has no class at all: never `N.Foo`.
    let p = project(&[(
        "a.ts",
        "export const a: Foo = make();\n\
         namespace N {\n  class Foo { m(): void {} }\n  a.m();\n}\n\
         declare function make(): any;\n",
    )]);
    p.unbound("a.ts", "a.m()", "m");
}

#[test]
fn a_generic_type_parameter_hides_the_class() {
    let p = project(&[(
        "a.ts",
        "export class Foo { m(): void {} }\n\
         export function f<Foo>(x: Foo): void { x.m(); }\n\
         export class Box<Foo> {\n  f: Foo;\n  g(): void { this.f.m(); }\n}\n\
         export function h(y: Foo): void { y.m(); }\n\
         export class Plain {\n  f: Foo;\n  g(): void { this.f.m(); }\n}\n",
    )]);
    p.unbound("a.ts", "x.m()", "m");
    p.unbound_nth("a.ts", "this.f.m()", "m", 0);
    // The controls, with no type parameter, bind.
    p.scoped("a.ts", "y.m()", "m", "a.ts#Foo.m");
    p.scoped_nth("a.ts", "this.f.m()", "m", 1, "a.ts#Foo.m");
}

#[test]
fn receivers_typed_through_imports() {
    let p = project(&[
        (
            "lib/svc.ts",
            "export class Svc { m(): void {} }\nexport default class Def { d(): void {} }\n",
        ),
        ("lib/barrel.ts", "export { Svc } from \"./svc\";\n"),
        ("pick.ts", "export class P { p(): void {} }\n"),
        ("pick/index.ts", "export class P { p(): void {} }\n"),
        (
            "app.ts",
            "import { Svc } from \"./lib/svc\";\n\
             import { Svc as Alias } from \"./lib/svc\";\n\
             import Def from \"./lib/svc\";\n\
             import * as ns from \"./lib/svc\";\n\
             import { Svc as Pkg } from \"some-package\";\n\
             import { Svc as Re } from \"./lib/barrel\";\n\
             import { P } from \"./pick\";\n\
             export function run(a: Svc, b: Alias, c: Def, d: ns.Svc, e: Pkg, f: Re, g: P): void {\n  \
             a.m(); b.m(); c.d(); d.m(); e.m(); f.m(); g.p();\n  \
             const n = new ns.Svc(); n.m();\n  const k = new Alias(); k.m();\n}\n",
        ),
    ]);
    p.scoped("app.ts", "a.m()", "m", "lib/svc.ts#Svc.m");
    p.scoped("app.ts", "b.m()", "m", "lib/svc.ts#Svc.m");
    p.scoped("app.ts", "c.d()", "d", "lib/svc.ts#Def.d");
    p.scoped("app.ts", "d.m()", "m", "lib/svc.ts#Svc.m");
    p.scoped("app.ts", "n.m()", "m", "lib/svc.ts#Svc.m");
    p.scoped("app.ts", "k.m()", "m", "lib/svc.ts#Svc.m");
    // A package import, a re-export, and an ambiguous module give no class.
    p.unbound("app.ts", "e.m()", "m");
    p.unbound("app.ts", "f.m()", "m");
    p.unbound("app.ts", "g.p()", "p");
}

#[test]
fn only_a_class_or_interface_is_a_receiver_class() {
    let p = project(&[(
        "a.ts",
        "export class Svc { m(): void {} }\n\
         export type Alias = Svc;\n\
         export enum E { A, B }\n\
         export class X { m(): void {} }\n\
         export interface X { n(): void }\n\
         export interface I { m(): void; p: number }\n\
         export namespace NS { export function f(): void {} }\n\
         export class Impl implements I { m(): void {} p = 1; }\n\
         export function run(a: Alias, e: E, x: X, i: I, n: NS): void {\n  \
         a.m(); e.A; x.m(); x.n(); i.m(); i.p; n.f();\n}\n",
    )]);
    p.unbound("a.ts", "a.m()", "m");
    p.unbound("a.ts", "e.A", "A");
    // `class X` and `interface X` merge: two declarations of one name.
    p.unbound("a.ts", "x.m()", "m");
    p.unbound("a.ts", "x.n()", "n");
    p.unbound("a.ts", "n.f()", "f");
    // An interface receiver binds the interface's own member, never an
    // implementing class's.
    p.scoped("a.ts", "i.m()", "m", "a.ts#I.m");
    p.scoped("a.ts", "i.p", "p", "a.ts#I.p");
}

#[test]
fn the_static_side_matches_the_receiver() {
    let source = "\
export class Base { m(): void {} }
export class S {
  static sm(): void {}
  im(): void {}
  static both(): void {}
  both(): void {}
  static run(): void { this.sm(); this.im(); this.both(); }
  go(): void { this.sm(); this.im(); this.both(); }
  static field = this.sm();
  inst = this.im();
  static { this.sm(); }
}
export class C extends Base {
  static m(): void {}
  run(): void { this.m(); }
  static srun(): void { this.m(); }
}
export function use(c: C, s: S): void { c.m(); s.im(); s.sm(); s.both(); S.sm(); }
";
    let p = project(&[("a.ts", source)]);
    // A static method's `this` binds static members only.
    p.scoped_nth("a.ts", "this.sm()", "sm", 0, "a.ts#S.sm");
    p.unbound_nth("a.ts", "this.im()", "im", 0);
    p.scoped_nth("a.ts", "this.both()", "both", 0, "a.ts#S.both#1");
    // An instance method's `this` binds instance members only.
    p.unbound_nth("a.ts", "this.sm()", "sm", 1);
    p.scoped_nth("a.ts", "this.im()", "im", 1, "a.ts#S.im");
    p.scoped_nth("a.ts", "this.both()", "both", 1, "a.ts#S.both#2");
    // A static field initializer and a static block are static; an instance
    // field initializer is not.
    p.scoped_nth("a.ts", "this.sm()", "sm", 2, "a.ts#S.sm");
    p.scoped_nth("a.ts", "this.im()", "im", 2, "a.ts#S.im");
    p.scoped_nth("a.ts", "this.sm()", "sm", 3, "a.ts#S.sm");
    // A static `m` beside an inherited instance `m`: an instance receiver's
    // `m` stays unresolved, a static one binds the static `m`.
    p.unbound_nth("a.ts", "this.m()", "m", 0);
    p.scoped_nth("a.ts", "this.m()", "m", 1, "a.ts#C.m");
    // A typed receiver is an instance.
    p.unbound("a.ts", " c.m()", "m");
    p.scoped("a.ts", " s.im()", "im", "a.ts#S.im");
    p.unbound("a.ts", " s.sm()", "sm");
    p.scoped("a.ts", " s.both()", "both", "a.ts#S.both#2");
    // A class-name receiver is not bound in v0.1.
    p.unbound("a.ts", " S.sm()", "sm");
}

#[test]
fn unknown_staticness_binds_nothing() {
    let source = "export class S {\n  im(): void {}\n  go(x: S): void { this.im(); x.im(); }\n}\n";
    let mut stripped_hint = rows(&[("a.ts", source)]);
    // A `this` hint with no recorded side (as PHP records it) binds nothing.
    for row in &mut stripped_hint.uses {
        if row.hint_json.contains("\"is_static\"") {
            row.hint_json = "{\"kind\":\"this\"}".to_string();
        }
    }
    let links = resolve(&stripped_hint);
    let receivers: Vec<_> = links
        .bindings
        .iter()
        .filter(|binding| binding.resolution == Resolution::Scoped)
        .collect();
    assert_eq!(receivers.len(), 1, "only x.im() binds: {receivers:?}");

    // With no member sides at all, no member has a known side.
    let mut no_sides = rows(&[("a.ts", source)]);
    for scope in &mut no_sides.scopes {
        let mut facts: serde_json::Value = serde_json::from_str(&scope.facts_json).expect("JSON");
        facts["member_sides"] = serde_json::json!([]);
        scope.facts_json = facts.to_string();
    }
    let links = resolve(&no_sides);
    assert!(
        links
            .bindings
            .iter()
            .all(|binding| binding.resolution != Resolution::Scoped),
        "{links:?}"
    );

    // A typed hint with no type-name span (as PHP records it) binds nothing.
    let mut no_span = rows(&[("a.ts", source)]);
    for row in &mut no_span.uses {
        if row.hint_json.contains("\"typed\"") {
            let mut hint: serde_json::Value = serde_json::from_str(&row.hint_json).expect("JSON");
            hint.as_object_mut().expect("object").remove("name_span");
            row.hint_json = hint.to_string();
        }
    }
    let links = resolve(&no_span);
    let receivers: Vec<_> = links
        .bindings
        .iter()
        .filter(|binding| binding.resolution == Resolution::Scoped)
        .collect();
    assert_eq!(receivers.len(), 1, "only this.im() binds: {receivers:?}");
}

#[test]
fn a_getter_and_setter_pair_is_two_candidates() {
    let p = project(&[(
        "a.ts",
        "export class L {\n  get label(): string { return \"\"; }\n  \
         set label(v: string) {}\n  get only(): string { return \"\"; }\n  \
         show(): void { this.label; this.only; this.label = \"x\"; }\n}\n",
    )]);
    p.unbound_nth("a.ts", "this.label", "label", 0);
    p.unbound_nth("a.ts", "this.label", "label", 1);
    p.scoped("a.ts", "this.only", "only", "a.ts#L.only");
}

#[test]
fn receivers_without_a_supported_hint_stay_unresolved() {
    let p = project(&[(
        "a.ts",
        "export class C { m(): void {} }\n\
         export class D { m(): void {} }\n\
         export function gaps(u: C | D, arr: C[], obj: C, name: \"m\"): void {\n  \
         let x = new C();\n  x.m();\n  var y = new C();\n  y.m();\n  \
         u.m();\n  arr[0].m();\n  obj[name]();\n  const z = obj;\n  z.m();\n}\n\
         export class H {\n  m(): void {}\n  a: H;\n  run(): void {\n    \
         function inner() { this.m(); }\n    \
         const o = { f() { this.m(); } };\n    \
         const anon = class { g() { this.m(); } };\n    \
         const arrow = () => this.m();\n    \
         this.a.a.m();\n    this.constructor;\n  }\n}\n",
    )]);
    p.unbound("a.ts", "x.m()", "m");
    p.unbound("a.ts", "y.m()", "m");
    p.unbound("a.ts", "u.m()", "m");
    p.unbound("a.ts", "arr[0].m()", "m");
    p.unbound("a.ts", "z.m()", "m");
    p.unbound("a.ts", "inner() { this.m()", "m");
    p.unbound("a.ts", "f() { this.m()", "m");
    p.unbound("a.ts", "g() { this.m()", "m");
    p.unbound("a.ts", "this.a.a.m()", "m");
    p.unbound("a.ts", "this.constructor", "constructor");
    // The field `a` itself binds; an arrow function keeps the method's `this`.
    p.scoped("a.ts", "this.a.a.m()", "a", "a.ts#H.a");
    p.scoped("a.ts", "() => this.m()", "m", "a.ts#H.m");
    // `obj[name]()` names no member: nothing in it binds `C.m`.
    let source = &p.sources["a.ts"];
    let start = source.find("obj[name]()").expect("computed call") as u32;
    let end = start + "obj[name]()".len() as u32;
    for row in p
        .rows
        .uses
        .iter()
        .filter(|row| row.start_byte >= start && row.end_byte <= end)
    {
        let bound = p
            .links
            .bindings
            .iter()
            .find(|binding| Some(binding.use_id) == row.use_id);
        assert!(
            bound.is_none_or(|binding| binding.target_id != "a.ts#C.m"),
            "{row:?}"
        );
    }
}

#[test]
fn a_member_the_class_does_not_declare_stays_unresolved() {
    let p = project(&[(
        "a.ts",
        "export class Base { inherited(): void {} }\n\
         export class M extends Base { a(): void {} }\n\
         export function f(x: M): void { x.b(); x.inherited(); x.a(); }\n",
    )]);
    p.unbound("a.ts", "x.b()", "b");
    p.unbound("a.ts", "x.inherited()", "inherited");
    p.scoped("a.ts", "x.a()", "a", "a.ts#M.a");
}

#[test]
fn fields_parameter_properties_and_private_names_are_members() {
    let p = project(&[(
        "a.ts",
        "export class Dep { go(): void {} }\n\
         export class K {\n  #count = 0;\n  static shared: Dep;\n  \
         constructor(private readonly dep: Dep, plain: Dep) {\n    \
         this.#count += 1;\n    this.dep.go();\n    plain.go();\n  }\n  \
         static s(): void { this.shared.go(); }\n}\n",
    )]);
    p.scoped("a.ts", "this.#count", "#count", "a.ts#K.%23count");
    p.scoped("a.ts", "this.dep.go()", "dep", "a.ts#K.dep");
    p.scoped("a.ts", "this.dep.go()", "go", "a.ts#Dep.go");
    p.scoped("a.ts", "plain.go()", "go", "a.ts#Dep.go");
    p.scoped("a.ts", "this.shared.go()", "shared", "a.ts#K.shared");
    p.scoped("a.ts", "this.shared.go()", "go", "a.ts#Dep.go");
    assert_eq!(
        p.bound("a.ts", "this.dep.go()", "go").map(|(_, tier)| tier),
        Some(Resolution::Scoped)
    );
}

#[test]
fn receiver_links_do_not_depend_on_row_order() {
    let files = [
        (
            "svc.ts",
            "export class Svc { m(): void {} static s(): void {} }\n",
        ),
        (
            "app.ts",
            "import { Svc } from \"./svc\";\nexport function f(a: Svc): void { a.m(); const b = new Svc(); b.m(); }\n",
        ),
    ];
    let forward = rows(&files);
    let mut reverse = rows(&[files[1], files[0]]);
    reverse.files.reverse();
    reverse.symbols.reverse();
    reverse.uses.reverse();
    reverse.scopes.reverse();
    let mut a = resolve(&forward).bindings;
    let mut b = resolve(&reverse).bindings;
    // Use IDs differ with the file order, so compare by use position.
    let key = |rows: &Rows, bindings: &mut Vec<rivet_store::BindingRow>| {
        let by_id: BTreeMap<i64, &UseRow> = rows
            .uses
            .iter()
            .map(|row| (row.use_id.expect("an ID"), row))
            .collect();
        let mut out: Vec<(String, u32, String, Resolution)> = bindings
            .drain(..)
            .map(|binding| {
                let row = by_id[&binding.use_id];
                (
                    row.file.clone(),
                    row.start_byte,
                    binding.target_id,
                    binding.resolution,
                )
            })
            .collect();
        out.sort();
        out
    };
    let a = key(&forward, &mut a);
    let b = key(&reverse, &mut b);
    assert_eq!(a, b);
    assert_eq!(
        a.iter()
            .filter(|(_, _, _, tier)| *tier == Resolution::Scoped)
            .count(),
        2,
        "{a:?}"
    );
}
