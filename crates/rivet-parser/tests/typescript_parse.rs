//! T41: TypeScript and TSX grammar dispatch and the v0.1 parse policy.
//!
//! `.ts` and `.d.ts` files parse with the TypeScript grammar and `.tsx` files
//! with the TSX grammar, chosen by [`language_for_path`]. A tree with an
//! ERROR or MISSING node is a parse failure ([`first_parse_error`]).
//! Since T43 [`parse_file`] dispatches both variants to the TypeScript
//! adapter, which extracts definitions, uses, and scopes, and a failing file
//! yields exactly the parse-policy diagnostic.

#![cfg(feature = "lang-typescript")]

use rivet_languages::{LanguageId, language_for_path};
use rivet_parser::{ParseFailure, first_parse_error, parse_file, parse_tree};

/// Parses `source` with the grammar `path` dispatches to and applies the
/// parse policy.
fn policy(path: &str, source: &str) -> Option<ParseFailure> {
    let language = language_for_path(path).expect("a TypeScript-family path");
    first_parse_error(&parse_tree(language, source.as_bytes()))
}

const VALID_TS: &str = "\
import { Base } from \"./base\";

export enum Mode {
  On,
  Off = \"off\",
}

export interface Named<T> {
  name: T;
}

export class Greeter extends Base implements Named<string> {
  static count = 0;
  #secret = 1;

  constructor(private readonly prefix: string, public name: string) {
    super();
  }

  get label(): string {
    return `${this.prefix} ${this.name}`;
  }
}

export const double = (n: number): number => n * 2;
";

const VALID_TSX: &str = "\
import { Button } from \"./Button\";

export function App({ title }: { title: string }) {
  return (
    <>
      <Button label={title} onPress={() => console.log(title)} />
      <span className=\"note\">Button text</span>
    </>
  );
}
";

const VALID_DTS: &str = "\
declare function greet(name: string): string;

declare const VERSION: string;

declare class Widget {
  render(): void;
}

declare namespace Lib {
  function version(): string;
}

declare module \"legacy-lib\" {
  export function start(): void;
}
";

/// A `.ts` file holding JSX: valid TSX, but not TypeScript.
const JSX_IN_TS: &str = "\
export function Label(name: string) {
  return <span className=\"label\">{name}</span>;
}
";

/// A genuinely broken file: the parameter list is never closed.
const BROKEN: &str = "\
export function broken(value: number {
  return value;
}
";

#[test]
fn extensions_dispatch_to_the_intended_grammar() {
    assert_eq!(language_for_path("src/a.ts"), Some(LanguageId::Typescript));
    assert_eq!(
        language_for_path("src/types.d.ts"),
        Some(LanguageId::Typescript)
    );
    assert_eq!(language_for_path("src/App.tsx"), Some(LanguageId::Tsx));
}

#[test]
fn valid_typescript_parses() {
    assert_eq!(policy("src/greeter.ts", VALID_TS), None);
}

#[test]
fn valid_tsx_with_jsx_parses() {
    assert_eq!(policy("src/App.tsx", VALID_TSX), None);
}

#[test]
fn valid_declaration_file_parses() {
    assert_eq!(policy("src/types.d.ts", VALID_DTS), None);
}

#[test]
fn jsx_in_a_ts_file_fails_under_the_typescript_grammar() {
    let failure = policy("src/label.ts", JSX_IN_TS).expect("JSX is not TypeScript");
    // The first failing node in pre-order: the grammar expects the `>` of a
    // type assertion after `<span`.
    assert_eq!(
        failure,
        ParseFailure {
            kind: ">".to_string(),
            start_byte: 52,
        }
    );
    // The same bytes are valid TSX: the grammar, not the source, differs.
    assert_eq!(policy("src/Label.tsx", JSX_IN_TS), None);
}

#[test]
fn broken_file_fails_under_both_grammars() {
    let expected = ParseFailure {
        kind: ")".to_string(),
        start_byte: 36,
    };
    assert_eq!(policy("src/broken.ts", BROKEN), Some(expected.clone()));
    assert_eq!(policy("src/broken.tsx", BROKEN), Some(expected));
}

/// Both grammar variants dispatch to the TypeScript adapter (T43): a valid
/// file yields definitions, uses, and scopes and no diagnostic, and a failing
/// file yields no facts and exactly the parse-policy diagnostic.
#[test]
fn parse_file_dispatches_to_the_typescript_adapter() {
    for (language, source, symbol, use_) in [
        (LanguageId::Typescript, VALID_TS, "Greeter.label", "Base"),
        (LanguageId::Tsx, VALID_TSX, "App", "Button"),
        (LanguageId::Typescript, VALID_DTS, "Lib.version", "Lib"),
    ] {
        let extracted = parse_file(language, source.as_bytes());
        assert!(
            extracted.diagnostics.is_empty(),
            "{language:?}: {extracted:?}"
        );
        assert!(
            extracted
                .symbols
                .iter()
                .any(|found| found.qualified_name == symbol),
            "{language:?}: {extracted:?}"
        );
        assert!(!extracted.scopes.is_empty(), "{language:?}");
        // The `.d.ts` sample's `Lib` is a declaration, not a use.
        let spelled = extracted.uses.iter().any(|found| found.spelling == use_);
        assert_eq!(spelled, source != VALID_DTS, "{language:?}: {extracted:?}");
        assert!(extracted.imports.is_empty(), "PHP `use` bindings only");
    }

    let extracted = parse_file(LanguageId::Typescript, BROKEN.as_bytes());
    assert!(extracted.symbols.is_empty() && extracted.uses.is_empty());
    assert!(extracted.imports.is_empty() && extracted.scopes.is_empty());
    assert_eq!(extracted.diagnostics.len(), 1, "{extracted:?}");
    assert_eq!(extracted.diagnostics[0].code, "parse_error");
    assert_eq!(extracted.diagnostics[0].detail, ") at byte 36");
    assert_eq!(extracted.diagnostics[0].start_byte, Some(36));

    let jsx = parse_file(LanguageId::Typescript, JSX_IN_TS.as_bytes());
    assert_eq!(jsx.diagnostics.len(), 1, "{jsx:?}");
    assert_eq!(jsx.diagnostics[0].code, "parse_error");
    assert!(
        parse_file(LanguageId::Tsx, JSX_IN_TS.as_bytes())
            .diagnostics
            .is_empty()
    );
}

/// GR1: the two constructs upstream `tree-sitter-typescript` 0.23.2 rejected
/// (Hono's parse failures) pass the parse policy under every dispatched
/// grammar with the vendored, patched `0.23.2-rivet.1`; `export type *`
/// without `from` still fails.
#[test]
fn patched_grammar_constructs_pass_the_parse_policy() {
    let generic_call_signatures = "\
export interface Get<E> {
  <Key extends keyof E>(key: Key): E[Key]
  <Key extends string>(key: Key): unknown
}
";
    let export_type_star = "export type * from './types'\nexport type * as ns from './ns'\n";
    for path in ["src/a.ts", "src/a.d.ts", "src/a.tsx"] {
        assert_eq!(policy(path, generic_call_signatures), None, "{path}");
        assert_eq!(policy(path, export_type_star), None, "{path}");
        let failure = policy(path, "export type *\n").expect("no `from` clause");
        assert_eq!(failure.start_byte, 0, "{path}: {failure:?}");
    }
}
