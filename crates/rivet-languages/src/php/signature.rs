//! PHP declaration signatures and doc comments (T14).
//!
//! A signature is the declaration header exactly as written in the source,
//! from the node start (including modifiers) through the body `{` or the final
//! `;`, with whitespace runs collapsed to single spaces. A doc comment is the
//! text of an immediately preceding `/** ... */` docblock, if any.
//! [`signature_summary`] renders the container collapsed form from stored
//! member signatures (spec §16.2).

use rivet_core::{ExtractedSymbol, SymbolKind};
use tree_sitter::Node;

use super::is_container;

/// The declaration header and attached doc comment for `node`.
///
/// `mod.rs` calls this once per matched declaration; the returned values are
/// frozen into the owning [`ExtractedSymbol`].
pub(crate) fn declaration_header(node: Node<'_>, source: &[u8]) -> (String, Option<String>) {
    (header(node, source), doc_comment(node, source))
}

/// The collapsed declaration header for `node`.
///
/// The header runs from the declaration start through the start of its body
/// (`{`), or through the end of the declaration when it has no body. A
/// trailing `;` on a bodyless declaration is dropped. Every run of whitespace
/// becomes one space and the result is trimmed.
fn header(node: Node<'_>, source: &[u8]) -> String {
    let end = match node.child_by_field_name("body") {
        Some(body) => body.start_byte(),
        None => node.end_byte(),
    };
    let raw = text(source, node.start_byte(), end);
    let raw = raw.strip_suffix(';').unwrap_or(&raw);
    collapse_whitespace(raw)
}

/// The immediately preceding docblock for `node`, or `None`.
///
/// Only a `/** ... */` docblock attaches: a `//` or `/* ... */` comment does
/// not, a different statement between the comment and the declaration does
/// not, and a blank line between them breaks attachment. The delimiters are
/// kept and leading indentation is trimmed from every line.
fn doc_comment(node: Node<'_>, source: &[u8]) -> Option<String> {
    let comment = node.prev_named_sibling()?;
    if comment.kind() != "comment" {
        return None;
    }
    let start = comment.start_byte();
    let end = comment.end_byte();
    let raw = text(source, start, end);
    if !raw.starts_with("/**") {
        return None;
    }
    // A blank line between the comment and the declaration breaks attachment;
    // one newline (comment above the declaration) and none (same line) attach.
    let between = &source[end..node.start_byte()];
    if between.iter().filter(|byte| **byte == b'\n').count() > 1 {
        return None;
    }
    // The comment node starts at `/**`, so the first line carries no code
    // indentation; the continuation lines do. Measure the indentation of the
    // line the comment starts on and strip that much from every line.
    Some(trim_indentation(&raw, line_indent(source, start)))
}

/// The number of space/tab bytes immediately before `start` on its line.
fn line_indent(source: &[u8], start: usize) -> usize {
    let mut index = start;
    while index > 0 && matches!(source[index - 1], b' ' | b'\t') {
        index -= 1;
    }
    start - index
}

/// Removes up to `indent` leading space/tab bytes from every line.
///
/// This drops the code indentation while preserving the docblock's own ` *`
/// alignment; a line with less indentation (for example a blank line) is
/// stripped only as far as it goes.
fn trim_indentation(raw: &str, indent: usize) -> String {
    raw.split('\n')
        .map(|line| {
            let leading = line
                .bytes()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            &line[leading.min(indent)..]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The collapsed form of a container symbol (spec §16.2).
///
/// For a class/interface/enum (and a trait, which is class-kind), this is the
/// container signature line, `{`, one line per direct member in source order,
/// and `}`. A method member collapses its body to ` { … }`; every other member
/// kind (property, constant, enum case) ends with `;`. A non-container symbol
/// returns its own signature unchanged.
///
/// `source` is part of the adapter contract; member headers are already stored
/// on each [`ExtractedSymbol`], so it is not consulted here.
pub fn signature_summary(
    symbol: &ExtractedSymbol,
    members: &[ExtractedSymbol],
    _source: &str,
) -> String {
    if !is_container(symbol.kind) {
        return symbol.signature.clone().unwrap_or_default();
    }

    let mut summary = String::new();
    summary.push_str(symbol.signature.as_deref().unwrap_or(""));
    summary.push_str("\n{\n");
    for member in members {
        let signature = member.signature.as_deref().unwrap_or("");
        summary.push_str("    ");
        summary.push_str(signature);
        match member.kind {
            // A method body is replaced by `{ … }`; an interface or abstract
            // method has no body to replace but is still callable, so it keeps
            // the same collapsed shape.
            SymbolKind::Method => summary.push_str(" { … }\n"),
            // Properties, constants, and enum cases are body-free already.
            _ => summary.push_str(";\n"),
        }
    }
    summary.push('}');
    summary
}

/// Decodes a source byte range as UTF-8 (lossy only if the caller violated the
/// valid-UTF-8 input domain).
fn text(source: &[u8], start: usize, end: usize) -> String {
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

/// Collapses every run of whitespace to one space and trims the ends.
fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rivet_core::{ExtractedFile, SymbolKind};
    use tree_sitter::Parser;

    use crate::{LanguageId, grammar, php};

    use super::signature_summary;

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/php/authored")
            .join(name)
    }

    fn extract_fixture(name: &str) -> (Vec<u8>, ExtractedFile) {
        let source = std::fs::read(fixture_path(name)).expect("read fixture");
        let mut parser = Parser::new();
        parser
            .set_language(&grammar(LanguageId::Php))
            .expect("pinned PHP grammar must load");
        let tree = parser
            .parse(&source, None)
            .expect("parser must return a tree");
        let extracted = php::extract(&source, &tree);
        assert!(
            extracted.diagnostics.is_empty(),
            "{:?}",
            extracted.diagnostics
        );
        (source, extracted)
    }

    /// The exact collapsed form of the fixture's `SurveyService` class, in
    /// source member order (const, property, then the two methods).
    #[test]
    fn survey_service_collapsed_form_is_exact() {
        let (source, extracted) = extract_fixture("SurveyService.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");
        let class_index = extracted
            .symbols
            .iter()
            .position(|symbol| symbol.qualified_name == "App\\Services\\SurveyService")
            .expect("SurveyService class");
        let class = &extracted.symbols[class_index];
        let members: Vec<rivet_core::ExtractedSymbol> = extracted
            .symbols
            .iter()
            .filter(|symbol| symbol.parent_index == Some(class_index))
            .cloned()
            .collect();

        let expected = "\
final class SurveyService
{
    public const DEFAULT_LABEL = 'survey';
    private string $label = 'survey';
    public function launch(): void { … }
    public function relaunch(): void { … }
}";
        assert_eq!(signature_summary(class, &members, &source), expected);
    }

    /// A non-container symbol's summary is its own signature, unchanged.
    #[test]
    fn non_container_summary_is_the_signature() {
        let (source, extracted) = extract_fixture("SurveyService.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");
        let launch = extracted
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "App\\Services\\SurveyService::launch")
            .expect("launch method");
        assert_eq!(
            signature_summary(launch, &[], &source),
            "public function launch(): void"
        );
    }

    /// Declarations carry their written header; only a `/** ... */` docblock
    /// immediately above (no blank line) attaches.
    #[test]
    fn signatures_and_doc_comments_from_fixture() {
        let (_source, extracted) = extract_fixture("Documented.php");

        let class = extracted
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "App\\Documented\\Documented")
            .expect("Documented class");
        assert_eq!(class.signature.as_deref(), Some("final class Documented"));
        assert_eq!(
            class.doc_comment.as_deref(),
            Some("/**\n * A documented service.\n */")
        );

        let run = extracted
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "App\\Documented\\Documented::run")
            .expect("run method");
        assert_eq!(
            run.signature.as_deref(),
            Some("public function run(): void")
        );
        assert_eq!(
            run.doc_comment.as_deref(),
            Some("/**\n * Runs the documented work.\n */")
        );

        let plain = extracted
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "App\\Documented\\Documented::plain")
            .expect("plain method");
        assert_eq!(
            plain.signature.as_deref(),
            Some("public function plain(): void")
        );
        assert_eq!(plain.doc_comment, None, "no comment above plain");

        let separated = extracted
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "App\\Documented\\Documented::separated")
            .expect("separated method");
        assert_eq!(
            separated.doc_comment, None,
            "a blank line breaks docblock attachment"
        );
    }

    /// The container at `qualified_name` and its direct members in extraction
    /// order (already sorted by declaration start byte).
    fn container_members(
        extracted: &ExtractedFile,
        qualified_name: &str,
    ) -> (
        rivet_core::ExtractedSymbol,
        Vec<rivet_core::ExtractedSymbol>,
    ) {
        let index = extracted
            .symbols
            .iter()
            .position(|symbol| symbol.qualified_name == qualified_name)
            .unwrap_or_else(|| panic!("missing {qualified_name}"));
        let container = extracted.symbols[index].clone();
        let members = extracted
            .symbols
            .iter()
            .filter(|symbol| symbol.parent_index == Some(index))
            .cloned()
            .collect();
        (container, members)
    }

    /// An interface summary lists its bodyless methods with collapsed bodies.
    #[test]
    fn interface_collapsed_form_lists_bodyless_methods() {
        let (source, extracted) = extract_fixture("Interface.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");
        let (interface, members) = container_members(&extracted, "App\\Contracts\\Named");

        let expected = "\
interface Named
{
    public const KIND = 'named';
    public function name(): string { … }
    public function set(int $value): void { … }
}";
        assert_eq!(signature_summary(&interface, &members, &source), expected);
    }

    /// An enum summary lists its cases (recorded as `const`) with `;`.
    #[test]
    fn enum_collapsed_form_lists_cases() {
        let (source, extracted) = extract_fixture("Enums.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");

        let (pure, members) = container_members(&extracted, "App\\Enums\\Suit");
        let expected = "\
enum Suit
{
    case Hearts;
    case Spades;
}";
        assert_eq!(signature_summary(&pure, &members, &source), expected);

        let (backed, members) = container_members(&extracted, "App\\Enums\\Status");
        let expected = "\
enum Status: string
{
    case Active = 'active';
    case Closed = 'closed';
}";
        assert_eq!(signature_summary(&backed, &members, &source), expected);
    }

    /// A trait is class-kind and summarizes like a class.
    #[test]
    fn trait_collapsed_form_is_class_kind() {
        let (source, extracted) = extract_fixture("Trait.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");
        let (trait_symbol, members) = container_members(&extracted, "App\\Concerns\\Greets");
        assert_eq!(trait_symbol.kind, SymbolKind::Class);

        let expected = "\
trait Greets
{
    public const SALUTE = 'hi';
    public function greet(): string { … }
}";
        assert_eq!(
            signature_summary(&trait_symbol, &members, &source),
            expected
        );
    }

    /// Every member kind appears once per declared name: multi-name
    /// declarations repeat the shared header, and promoted properties are
    /// ordinary `;` members after the constructor.
    #[test]
    fn class_collapsed_form_covers_multi_name_and_promoted_members() {
        let (source, extracted) = extract_fixture("Members.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");
        let (class, members) = container_members(&extracted, "App\\Members\\AbstractThing");

        let expected = "\
abstract class AbstractThing
{
    public const FIRST = 1, SECOND = 2;
    public const FIRST = 1, SECOND = 2;
    public static int $count = 0;
    public readonly string $title;
    public int $left, $right = 2;
    public int $left, $right = 2;
    abstract public function describe(): string { … }
    public function __construct(private int $seed, public string $tag = 'x') { … }
    private int $seed;
    public string $tag = 'x';
}";
        assert_eq!(signature_summary(&class, &members, &source), expected);
    }
}
