//! T44: TypeScript direct relative imports, exports, namespace imports, and
//! same-file lexical bindings, end to end through the binary.
//!
//! Each test indexes a small temporary repository and reads the published
//! bindings back. A use is named by the file it is in and a needle that
//! starts at the identifier (`at("app.ts", "b; // renamed")`), so every
//! assertion says which use it is about. A binding is `(target, tier)`;
//! `None` means the use stays unresolved.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rusqlite::Connection;
use support::{git_repo, run, success, write};

/// `(target, resolution)` of every stored binding, by `(file, start_byte)`.
fn bindings(root: &Path) -> BTreeMap<(String, i64), (String, String)> {
    let conn = Connection::open(root.join(".rivet").join("index.db")).expect("open index.db");
    let mut stmt = conn
        .prepare(
            "SELECT uses.file, uses.start_byte, target_id, resolution
             FROM bindings JOIN uses USING (use_id)",
        )
        .expect("prepare");
    stmt.query_map([], |row| {
        Ok(((row.get(0)?, row.get(1)?), (row.get(2)?, row.get(3)?)))
    })
    .expect("query")
    .collect::<rusqlite::Result<BTreeMap<_, _>>>()
    .expect("rows")
}

/// Whether a use starts at `(file, start_byte)`.
fn is_use(root: &Path, file: &str, start: i64) -> bool {
    let conn = Connection::open(root.join(".rivet").join("index.db")).expect("open index.db");
    conn.query_row(
        "SELECT count(*) FROM uses WHERE file = ?1 AND start_byte = ?2",
        rusqlite::params![file, start],
        |row| row.get::<_, i64>(0),
    )
    .expect("count")
        == 1
}

/// One indexed repository and its bindings.
struct Indexed {
    root: support::TempDir,
    bindings: BTreeMap<(String, i64), (String, String)>,
}

impl Indexed {
    fn new(label: &str, files: &[(&str, &str)]) -> Indexed {
        let root = git_repo(label);
        for (path, text) in files {
            write(root.path(), path, text.as_bytes());
        }
        success(&run(root.path(), &["index", "--json"]));
        let bindings = bindings(root.path());
        Indexed { root, bindings }
    }

    /// The link of the use in `file` that starts where `needle` starts.
    fn at(&self, file: &str, needle: &str) -> Option<(String, String)> {
        let source = fs::read_to_string(self.root.path().join(file)).expect("read");
        assert_eq!(
            source.matches(needle).count(),
            1,
            "{file}: needle {needle:?} is not unique"
        );
        let start = source.find(needle).expect("needle") as i64;
        assert!(
            is_use(self.root.path(), file, start),
            "{file}: no use starts at {needle:?}"
        );
        self.bindings.get(&(file.to_string(), start)).cloned()
    }

    /// Asserts the use at `needle` binds `target` exactly.
    fn exact(&self, file: &str, needle: &str, target: &str) {
        assert_eq!(
            self.at(file, needle),
            Some((target.to_string(), "exact".to_string())),
            "{file}: {needle:?}"
        );
    }

    /// Asserts the use at `needle` stays unresolved.
    fn unresolved(&self, file: &str, needle: &str) {
        assert_eq!(self.at(file, needle), None, "{file}: {needle:?}");
    }
}

const EXPORTS_F: &str = "export function f(): number {\n  return 1;\n}\n";

#[test]
fn ambiguous_candidate_modules_stay_unresolved() {
    let repo = Indexed::new(
        "t44-ambiguous",
        &[
            // `./x` names both x.ts and x.d.ts.
            ("src/x.ts", EXPORTS_F),
            ("src/x.d.ts", "export declare function f(): number;\n"),
            // `./y` names both y.ts and y/index.ts.
            ("src/y.ts", EXPORTS_F),
            ("src/y/index.ts", EXPORTS_F),
            // `./z` names both z.ts and z.tsx.
            ("src/z.ts", EXPORTS_F),
            ("src/z.tsx", EXPORTS_F),
            // `./w` names w.ts only.
            ("src/w.ts", EXPORTS_F),
            (
                "src/app.ts",
                "import { f as fx } from \"./x\";\n\
                 import { f as fy } from \"./y\";\n\
                 import { f as fz } from \"./z\";\n\
                 import { f as fw } from \"./w\";\n\
                 fx(); fy(); fz(); fw();\n",
            ),
        ],
    );
    repo.unresolved("src/app.ts", "fx }");
    repo.unresolved("src/app.ts", "fx();");
    repo.unresolved("src/app.ts", "fy }");
    repo.unresolved("src/app.ts", "fy();");
    repo.unresolved("src/app.ts", "fz }");
    repo.unresolved("src/app.ts", "fz();");
    repo.exact("src/app.ts", "fw }", "src/w.ts#f");
    repo.exact("src/app.ts", "fw();", "src/w.ts#f");
}

#[test]
fn candidates_match_indexed_paths_byte_for_byte() {
    let repo = Indexed::new(
        "t44-byte-exact",
        &[
            ("src/util.ts", EXPORTS_F),
            (
                "src/dir/index.ts",
                "export function g(): number {\n  return 2;\n}\n",
            ),
            (
                "src/types.d.ts",
                "export interface Shape {\n  size: number;\n}\n",
            ),
            (
                "src/app.ts",
                "import { f as wrongCase } from \"./Util\";\n\
                 import { f as rightCase } from \"./util\";\n\
                 import { f as withExtension } from \"./util.ts\";\n\
                 import { f as dotted } from \"./../src/./util\";\n\
                 import { f as aboveRoot } from \"../../util\";\n\
                 import { f as doubleSlash } from \".//util\";\n\
                 import { g as trailing } from \"./dir/\";\n\
                 import { g as directory } from \"./dir\";\n\
                 import { Shape } from \"./types\";\n\
                 import { f as bare } from \"util\";\n\
                 wrongCase(); rightCase(); withExtension(); dotted(); aboveRoot();\n\
                 doubleSlash(); trailing(); directory(); bare();\n\
                 export const s: Shape = { size: 1 };\n",
            ),
        ],
    );
    // This volume may fold case, so the filesystem would find `./Util`; the
    // index never asks it.
    let folds = repo.root.path().join("src/Util.ts").exists();
    println!("case-insensitive volume: {folds}");
    repo.unresolved("src/app.ts", "wrongCase }");
    repo.unresolved("src/app.ts", "wrongCase();");
    repo.exact("src/app.ts", "rightCase();", "src/util.ts#f");
    repo.exact("src/app.ts", "withExtension();", "src/util.ts#f");
    repo.exact("src/app.ts", "dotted();", "src/util.ts#f");
    repo.unresolved("src/app.ts", "aboveRoot();");
    repo.unresolved("src/app.ts", "doubleSlash();");
    // T44a: a trailing `/` names the directory, so its index.
    repo.exact("src/app.ts", "trailing();", "src/dir/index.ts#g");
    repo.exact("src/app.ts", "directory();", "src/dir/index.ts#g");
    repo.exact("src/app.ts", "Shape = {", "src/types.d.ts#Shape");
    repo.unresolved("src/app.ts", "bare();");
}

// T44a: `.`, `..`, a last segment of `.` or `..`, and a trailing `/` name
// only a directory, whose index is the only candidate.
#[test]
fn directory_only_specifiers_name_only_the_directory_index() {
    let repo = Indexed::new(
        "t44a-directory",
        &[
            (
                "src/index.ts",
                "export function top(): number {\n  return 0;\n}\n",
            ),
            // Named after the directory `src/`: `..` must never name it.
            (
                "src.ts",
                "export function top(): number {\n  return 9;\n}\n",
            ),
            ("src/pick.ts", EXPORTS_F),
            ("src/pick/index.ts", EXPORTS_F),
            // `lone/` has no index, only a file named after it.
            ("lone.ts", EXPORTS_F),
            (
                "lone/sub/x.ts",
                "import { f as loneF } from \"..\";\nloneF();\n",
            ),
            // `both/` has two index candidates.
            ("both/index.ts", EXPORTS_F),
            ("both/index.tsx", EXPORTS_F),
            ("both/x.ts", "import { f as bothF } from \".\";\nbothF();\n"),
            (
                "src/pick/x.ts",
                "import { f as dotF } from \".\";\n\
                 import { f as slashF } from \"./\";\n\
                 import { f as dirF } from \"../pick/\";\n\
                 import { f as fileF } from \"../pick\";\n\
                 import { top as upTop } from \"..\";\n\
                 import { top as upSlashTop } from \"../\";\n\
                 import { f as backF } from \"./x/..\";\n\
                 import * as here from \".\";\n\
                 import { f as wrongCase } from \"../Pick/\";\n\
                 import { f as doubleSlash } from \"..//pick\";\n\
                 dotF(); slashF(); dirF(); fileF(); upTop(); upSlashTop(); backF();\n\
                 here.f(); wrongCase(); doubleSlash();\n",
            ),
            (
                "src/a/b/c.ts",
                "import { top as twoUp } from \"../..\";\ntwoUp();\n",
            ),
            (
                "top.ts",
                "import { top as aboveRoot } from \"..\";\naboveRoot();\n",
            ),
        ],
    );
    let x = "src/pick/x.ts";
    repo.exact(x, "dotF }", "src/pick/index.ts#f");
    repo.exact(x, "dotF();", "src/pick/index.ts#f");
    repo.exact(x, "slashF();", "src/pick/index.ts#f");
    // `../pick/` is the directory only; `../pick` also names src/pick.ts.
    repo.exact(x, "dirF();", "src/pick/index.ts#f");
    repo.unresolved(x, "fileF();");
    repo.exact(x, "upTop();", "src/index.ts#top");
    repo.exact(x, "upSlashTop();", "src/index.ts#top");
    repo.exact(x, "backF();", "src/pick/index.ts#f");
    repo.exact(x, "f(); wrongCase", "src/pick/index.ts#f");
    repo.unresolved(x, "wrongCase();");
    repo.unresolved(x, "doubleSlash();");
    repo.exact("src/a/b/c.ts", "twoUp();", "src/index.ts#top");
    repo.unresolved("lone/sub/x.ts", "loneF();");
    repo.unresolved("both/x.ts", "bothF();");
    repo.unresolved("top.ts", "aboveRoot();");
}

#[test]
fn unindexed_modules_stay_unresolved() {
    let repo = Indexed::new(
        "t44-unindexed",
        &[
            (
                "src/broken.ts",
                "export function broken(value: number {\n  return value;\n}\n",
            ),
            (
                "src/legacy.js",
                "export function legacy() {\n  return 1;\n}\n",
            ),
            (
                "src/esm.mts",
                "export function esm(): number {\n  return 1;\n}\n",
            ),
            // A broken x.ts beside a clean x.d.ts: TypeScript resolves x.ts
            // first, so binding x.d.ts's declaration would be wrong.
            ("src/pair.ts", "export function paired(: number {}\n"),
            (
                "src/pair.d.ts",
                "export declare function paired(): number;\n",
            ),
            (
                "src/app.ts",
                "import { broken } from \"./broken\";\n\
                 import { legacy } from \"./legacy\";\n\
                 import { legacy as legacyJs } from \"./legacy.js\";\n\
                 import { esm } from \"./esm\";\n\
                 import { esm as esmMts } from \"./esm.mts\";\n\
                 import { paired } from \"./pair\";\n\
                 broken(1); legacy(); legacyJs(); esm(); esmMts(); paired();\n",
            ),
        ],
    );
    for needle in [
        "broken(1)",
        "legacy();",
        "legacyJs();",
        "esm();",
        "esmMts();",
        "paired();",
    ] {
        repo.unresolved("src/app.ts", needle);
    }
    assert!(
        repo.bindings.keys().all(|(file, _)| file != "src/app.ts"),
        "{:?}",
        repo.bindings
    );
    // Each candidate above is in the snapshot but not indexed.
    let conn =
        Connection::open(repo.root.path().join(".rivet").join("index.db")).expect("open index.db");
    for (path, status) in [
        ("src/broken.ts", "parse_error"),
        ("src/pair.ts", "parse_error"),
        ("src/pair.d.ts", "ok"),
        ("src/legacy.js", "unsupported"),
        ("src/esm.mts", "unsupported"),
    ] {
        let stored: String = conn
            .query_row(
                "SELECT parse_status FROM files WHERE path = ?1",
                [path],
                |row| row.get(0),
            )
            .expect("a files row");
        assert_eq!(stored, status, "{path}");
    }
}

#[test]
fn renamed_exports_bind_the_local_declaration() {
    let repo = Indexed::new(
        "t44-renames",
        &[
            (
                "src/m.ts",
                "const a = 1;\nfunction d(): number {\n  return a;\n}\n\
                 export { a as b, d as default };\n",
            ),
            (
                "src/app.ts",
                "import { b } from \"./m\";\n\
                 import { b as c } from \"./m\";\n\
                 import dflt from \"./m\";\n\
                 import { default as named } from \"./m\";\n\
                 import { a } from \"./m\";\n\
                 console.log(b, c, a);\n\
                 dflt(); named();\n",
            ),
        ],
    );
    repo.exact("src/app.ts", "b }", "src/m.ts#a");
    repo.exact("src/app.ts", "b, c, a)", "src/m.ts#a");
    repo.exact("src/app.ts", "c }", "src/m.ts#a");
    repo.exact("src/app.ts", "c, a)", "src/m.ts#a");
    repo.exact("src/app.ts", "dflt from", "src/m.ts#d");
    repo.exact("src/app.ts", "dflt();", "src/m.ts#d");
    repo.exact("src/app.ts", "named }", "src/m.ts#d");
    repo.exact("src/app.ts", "named();", "src/m.ts#d");
    // `a` is exported only as `b`.
    repo.unresolved("src/app.ts", "a }");
    repo.unresolved("src/app.ts", "a);");
    // The export specifiers name the local declarations in m.ts.
    repo.exact("src/m.ts", "a as b", "src/m.ts#a");
    repo.exact("src/m.ts", "d as default", "src/m.ts#d");
}

#[test]
fn missing_exports_stay_unresolved() {
    let repo = Indexed::new(
        "t44-missing",
        &[
            (
                "src/m.ts",
                "function hidden(): number {\n  return 1;\n}\n\
                 export function shown(): number {\n  return hidden();\n}\n",
            ),
            (
                "src/app.ts",
                "import { hidden, shown, nowhere } from \"./m\";\n\
                 hidden(); shown(); nowhere();\n",
            ),
        ],
    );
    repo.unresolved("src/app.ts", "hidden,");
    repo.unresolved("src/app.ts", "hidden();");
    repo.unresolved("src/app.ts", "nowhere }");
    repo.unresolved("src/app.ts", "nowhere();");
    repo.exact("src/app.ts", "shown,", "src/m.ts#shown");
    repo.exact("src/app.ts", "shown();", "src/m.ts#shown");
    // Inside m.ts the unexported function binds by the same-file rule.
    repo.exact("src/m.ts", "hidden();", "src/m.ts#hidden");
}

#[test]
fn default_exports_bind_only_an_explicit_local_declaration() {
    let repo = Indexed::new(
        "t44-defaults",
        &[
            (
                "src/named.ts",
                "export default function named(): number {\n  return 1;\n}\n",
            ),
            (
                "src/value.ts",
                "const someConst = 1;\nexport default someConst;\n",
            ),
            ("src/arrow.ts", "export default () => {};\n"),
            (
                "src/anon.ts",
                "export default class {\n  run(): void {}\n}\n",
            ),
            ("src/expr.ts", "const inner = 1;\nexport default (inner);\n"),
            ("src/klass.ts", "export default class Klass {}\n"),
            ("src/none.ts", "export const only = 1;\n"),
            (
                "src/app.ts",
                "import n from \"./named\";\n\
                 import v from \"./value\";\n\
                 import ar from \"./arrow\";\n\
                 import an from \"./anon\";\n\
                 import ex from \"./expr\";\n\
                 import K from \"./klass\";\n\
                 import no from \"./none\";\n\
                 n(); console.log(v); ar(); new an(); console.log(ex); new K(); no();\n",
            ),
        ],
    );
    repo.exact("src/app.ts", "n(); console.log(v)", "src/named.ts#named");
    repo.exact("src/app.ts", "v);", "src/value.ts#someConst");
    repo.exact("src/app.ts", "K();", "src/klass.ts#Klass");
    repo.unresolved("src/app.ts", "ar();");
    repo.unresolved("src/app.ts", "an();");
    repo.unresolved("src/app.ts", "ex);");
    repo.unresolved("src/app.ts", "no();");
    // `export default someConst` names the local const.
    repo.exact("src/value.ts", "someConst;", "src/value.ts#someConst");
}

#[test]
fn namespace_import_members_bind_the_module_exports() {
    let repo = Indexed::new(
        "t44-namespace",
        &[
            (
                "src/m.ts",
                "export function member(): { deeper: number } {\n  return { deeper: 1 };\n}\n\
                 export interface Member {\n  size: number;\n}\n\
                 export class Klass {}\n\
                 export function Comp(): null {\n  return null;\n}\n",
            ),
            (
                "src/app.tsx",
                "import * as ns from \"./m\";\n\
                 ns.member();\n\
                 export const typed: ns.Member = { size: 1 };\n\
                 export const made = new ns.Klass();\n\
                 ns.missing();\n\
                 export const deep = ns.member().deeper;\n\
                 export const el = <ns.Comp />;\n\
                 export function shadowed(ns: { member(): void }): void {\n  ns.member();\n}\n",
            ),
        ],
    );
    repo.exact(
        "src/app.tsx",
        "member();\nexport const typed",
        "src/m.ts#member",
    );
    repo.exact("src/app.tsx", "Member = {", "src/m.ts#Member");
    repo.exact("src/app.tsx", "Klass();", "src/m.ts#Klass");
    repo.exact("src/app.tsx", "member().deeper", "src/m.ts#member");
    repo.exact("src/app.tsx", "Comp />", "src/m.ts#Comp");
    repo.unresolved("src/app.tsx", "missing();");
    // `ns.member().deeper` binds only `member`.
    repo.unresolved("src/app.tsx", "deeper;");
    // The namespace import itself names a module, not a declaration.
    repo.unresolved("src/app.tsx", "ns from");
    // A parameter `ns` hides the import.
    repo.unresolved("src/app.tsx", "member();\n}");
}

#[test]
fn a_local_hides_an_import_and_spaces_are_kept_apart() {
    let repo = Indexed::new(
        "t44-spaces",
        &[
            (
                "src/m.ts",
                "export interface X {\n  size: number;\n}\n\
                 export const X = 1;\n\
                 export function double(n: number): number {\n  return n * 2;\n}\n\
                 export interface OnlyType {\n  kind: string;\n}\n",
            ),
            (
                "src/app.ts",
                "import { X, double, OnlyType } from \"./m\";\n\
                 export const sized: X = { size: X };\n\
                 export type Copy = typeof X;\n\
                 export function shadowed(): number {\n  const double = (n: number): number => n + n;\n  return double(7);\n}\n\
                 export function param(X: number, v: OnlyType): number {\n  const inner: X = { size: X + 1 };\n  return inner.size + double(1);\n}\n\
                 export function generic<X>(value: X): X {\n  return value;\n}\n\
                 export const made = new OnlyType();\n",
            ),
            (
                "src/local.ts",
                "export interface Y {\n  a: number;\n}\n\
                 export interface Y {\n  b: number;\n}\n\
                 export interface Z {\n  c: number;\n}\n\
                 export const Z = 2;\n\
                 export const merged: Y = { a: 1, b: 2 };\n\
                 export const typed: Z = { c: Z };\n\
                 export type ZType = typeof Z;\n",
            ),
        ],
    );
    // Imported: a type use names the interface, a value use the const, and
    // `typeof` a value. The import use names both, so it binds neither.
    repo.exact("src/app.ts", "X = { size: X };", "src/m.ts#X#1");
    repo.exact("src/app.ts", "X };\nexport type", "src/m.ts#X#2");
    repo.exact("src/app.ts", "X;\nexport function shadowed", "src/m.ts#X#2");
    repo.unresolved("src/app.ts", "X, double");
    // A local `double` hides the imported one.
    repo.unresolved("src/app.ts", "double(7)");
    repo.exact("src/app.ts", "double(1)", "src/m.ts#double");
    // A value parameter `X` hides the const but not the interface.
    repo.exact("src/app.ts", "X = { size: X + 1 }", "src/m.ts#X#1");
    repo.unresolved("src/app.ts", "X + 1");
    // A type parameter `X` hides the interface.
    repo.unresolved("src/app.ts", "X): X {");
    // `new` needs a value; OnlyType is only a type.
    repo.unresolved("src/app.ts", "OnlyType();");
    repo.exact("src/app.ts", "OnlyType): number", "src/m.ts#OnlyType");

    // Same file: a double `interface Y` merges, so a type use is refused.
    repo.unresolved("src/local.ts", "Y = { a");
    repo.exact("src/local.ts", "Z = { c", "src/local.ts#Z#1");
    repo.exact("src/local.ts", "Z };", "src/local.ts#Z#2");
    repo.exact("src/local.ts", "Z;\n", "src/local.ts#Z#2");
}

#[test]
fn circular_imports_resolve_without_looping() {
    let repo = Indexed::new(
        "t44-circular",
        &[
            (
                "src/a.ts",
                "import { b } from \"./b\";\nexport function a(): number {\n  return b();\n}\n",
            ),
            (
                "src/b.ts",
                "import { a } from \"./a\";\nexport function b(): number {\n  return a();\n}\n",
            ),
            // A re-export cycle is not followed either.
            ("src/c.ts", "export { d } from \"./d\";\n"),
            ("src/d.ts", "export { d } from \"./c\";\n"),
            ("src/e.ts", "import { d } from \"./c\";\nd();\n"),
        ],
    );
    repo.exact("src/a.ts", "b();", "src/b.ts#b");
    repo.exact("src/b.ts", "a();", "src/a.ts#a");
    repo.unresolved("src/e.ts", "d();");
}

#[test]
fn re_exports_and_exported_imports_are_not_followed() {
    let repo = Indexed::new(
        "t44-re-exports",
        &[
            ("src/m.ts", EXPORTS_F),
            (
                "src/relay.ts",
                "import { f } from \"./m\";\nexport { f };\nexport { f as g } from \"./m\";\n\
                 export * as all from \"./m\";\nexport * from \"./m\";\nexport const own = 1;\n",
            ),
            (
                "src/app.ts",
                "import { f, g, all, own } from \"./relay\";\nf(); g(); all.f(); console.log(own);\n",
            ),
        ],
    );
    for needle in ["f(); g", "g();", "f(); console", "all.f"] {
        repo.unresolved("src/app.ts", needle);
    }
    repo.exact("src/app.ts", "own);", "src/relay.ts#own");
    // In relay.ts, `export { f }` names the import binding, which binds m.ts.
    repo.exact("src/relay.ts", "f };", "src/m.ts#f");
}

#[test]
fn import_type_and_type_only_exports_bind() {
    let repo = Indexed::new(
        "t44-type-only",
        &[
            (
                "src/m.ts",
                "interface Hidden {\n  a: number;\n}\nexport type { Hidden as Shown };\n",
            ),
            (
                "src/app.ts",
                "import type { Shown } from \"./m\";\nimport { type Shown as Again } from \"./m\";\n\
                 export const s: Shown = { a: 1 };\nexport const t: Again = { a: 2 };\n",
            ),
        ],
    );
    repo.exact("src/app.ts", "Shown } from", "src/m.ts#Hidden");
    repo.exact("src/app.ts", "Shown = {", "src/m.ts#Hidden");
    repo.exact("src/app.ts", "Again = {", "src/m.ts#Hidden");
}

#[test]
fn globals_bind_nothing_in_their_file() {
    let repo = Indexed::new(
        "t44-globals",
        &[
            // No top-level import or export: a script, whose declarations are
            // global and can merge with other files' declarations.
            (
                "src/script.ts",
                "function f(): number {\n  return 1;\n}\ninterface G {\n  a: number;\n}\n\
                 f();\nconst g: G = { a: f() };\n",
            ),
            (
                "src/module.ts",
                "declare global {\n  interface Window {\n    extra: number;\n  }\n}\n\
                 export interface Local {\n  a: number;\n}\n\
                 export const w: Window | null = null;\nexport const l: Local = { a: 1 };\n",
            ),
        ],
    );
    repo.unresolved("src/script.ts", "f();\nconst");
    repo.unresolved("src/script.ts", "G = {");
    repo.unresolved("src/module.ts", "Window | null");
    repo.exact("src/module.ts", "Local = {", "src/module.ts#Local");
}

#[test]
fn an_ambient_module_body_never_looks_past_itself() {
    let repo = Indexed::new(
        "t44-ambient",
        &[
            (
                "src/models.ts",
                "export interface User {\n  id: number;\n}\nexport interface Extra {\n  e: number;\n}\n",
            ),
            (
                "src/aug.ts",
                "import { User } from \"./models\";\n\
                 declare module \"./models\" {\n  interface Extra {\n    owner: User;\n  }\n}\n\
                 declare module \"some-package\" {\n  interface Options {\n    user: User;\n  }\n}\n\
                 export const u: User = { id: 1 };\n",
            ),
        ],
    );
    // Inside an augmentation the augmented module's exports are in scope too;
    // the index does not look past the body, even for a relative module.
    repo.unresolved("src/aug.ts", "User;\n  }\n}\ndeclare module \"some");
    repo.unresolved("src/aug.ts", "User;\n  }\n}\nexport");
    repo.exact("src/aug.ts", "User = {", "src/models.ts#User");
}

#[test]
fn merged_namespace_bodies_see_each_other() {
    let repo = Indexed::new(
        "t44-merged",
        &[(
            "src/n.ts",
            "export const a = 1;\n\
             export namespace N {\n  export const a = 2;\n}\n\
             export namespace N {\n  export const b = a;\n}\n\
             export namespace M {\n  export const m = 3;\n  export const n = m;\n}\n\
             export const outer = a;\n\
             export function overloaded(value: number): number;\n\
             export function overloaded(value: string): string;\n\
             export function overloaded(value: number | string): number | string {\n  return value;\n}\n\
             overloaded(1);\n\
             export enum E {\n  A = 1,\n  B = A,\n}\n",
        )],
    );
    // In N's second body, `a` is N's exported `a` from the first body, not
    // the module's `a`: refused rather than guessed.
    repo.unresolved("src/n.ts", "a;\n}\nexport namespace M");
    repo.exact("src/n.ts", "m;\n", "src/n.ts#M.m");
    repo.exact("src/n.ts", "a;\nexport function", "src/n.ts#a");
    repo.exact("src/n.ts", "overloaded(1)", "src/n.ts#overloaded");
    repo.exact("src/n.ts", "A,\n}", "src/n.ts#E.A");
}
