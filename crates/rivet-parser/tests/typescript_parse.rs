//! T41: TypeScript and TSX grammar dispatch and the v0.1 parse policy.
//!
//! `.ts` and `.d.ts` files parse with the TypeScript grammar and `.tsx` files
//! with the TSX grammar, chosen by [`language_for_path`]. A tree with an
//! ERROR or MISSING node is a parse failure ([`first_parse_error`]). No
//! TypeScript adapter exists until T42, so [`parse_file`] extracts nothing
//! from these files and reports only the parse-policy diagnostic.

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

/// With no TypeScript adapter, a valid file yields no facts and no
/// diagnostic, and a failing file yields exactly the parse-policy diagnostic.
#[test]
fn parse_file_extracts_nothing_until_an_adapter_exists() {
    for (language, source) in [
        (LanguageId::Typescript, VALID_TS),
        (LanguageId::Tsx, VALID_TSX),
        (LanguageId::Typescript, VALID_DTS),
    ] {
        let extracted = parse_file(language, source.as_bytes());
        assert_eq!(extracted, Default::default(), "{language:?}");
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
