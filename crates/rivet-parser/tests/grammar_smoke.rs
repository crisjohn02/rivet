//! Grammar smoke tests for the pinned Tree-sitter grammar set.
//!
//! These prove the grammar crates link against the same `tree-sitter-language`
//! ABI as core and that PHP, TypeScript, and TSX sources parse cleanly. The
//! malformed-PHP case proves error detection for the T11 parse policy. Nothing
//! here extracts facts; later tasks consume the parse trees.

#[cfg(any(feature = "lang-php", feature = "lang-typescript"))]
use rivet_languages::{LanguageId, grammar};
#[cfg(any(feature = "lang-php", feature = "lang-typescript"))]
use tree_sitter::{Parser, Tree};

#[cfg(any(feature = "lang-php", feature = "lang-typescript"))]
fn parse(id: LanguageId, source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&grammar(id))
        .expect("pinned grammar must load against the core ABI");
    parser
        .parse(source, None)
        .expect("parser must return a tree")
}

/// A five-line PHP class with one method parses without errors.
#[cfg(feature = "lang-php")]
#[test]
fn php_class_parses_without_errors() {
    let source = "\
<?php
namespace App;
class Greeter {
  public function greet(): string { return \"hi\"; }
}";
    let tree = parse(LanguageId::Php, source);
    let root = tree.root_node();
    assert_eq!(root.kind(), "program");
    assert!(
        !root.has_error(),
        "unexpected PHP parse errors in:\n{source}"
    );
}

/// Deliberately malformed PHP reports errors, so the T11 parse policy has a
/// detectable failure signal.
#[cfg(feature = "lang-php")]
#[test]
fn php_malformed_source_reports_errors() {
    let source = "\
<?php
class Broken {
  public function oops( {
}";
    let tree = parse(LanguageId::Php, source);
    let root = tree.root_node();
    assert!(
        root.has_error(),
        "malformed PHP must report errors in:\n{source}"
    );
}

/// A five-line TypeScript class parses without errors.
#[cfg(feature = "lang-typescript")]
#[test]
fn typescript_class_parses_without_errors() {
    let source = "\
class Greeter {
  name = \"hi\";

  greet(): string { return this.name; }
}";
    let tree = parse(LanguageId::Typescript, source);
    let root = tree.root_node();
    assert_eq!(root.kind(), "program");
    assert!(
        !root.has_error(),
        "unexpected TypeScript parse errors in:\n{source}"
    );
}

/// A five-line TSX function returning JSX parses without errors.
#[cfg(feature = "lang-typescript")]
#[test]
fn tsx_function_parses_without_errors() {
    let source = "\
function App() {
  const label = \"hi\";
  return <div>{label}</div>;
}
export default App;";
    let tree = parse(LanguageId::Tsx, source);
    let root = tree.root_node();
    assert_eq!(root.kind(), "program");
    assert!(
        !root.has_error(),
        "unexpected TSX parse errors in:\n{source}"
    );
}
