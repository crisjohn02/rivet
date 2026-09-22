//! PHP declaration signatures and doc comments (T14).
//!
//! A signature is the declaration header exactly as written in the source,
//! from the node start (including modifiers) through the body `{` or the final
//! `;`, with whitespace runs collapsed to single spaces. A doc comment is the
//! text of an immediately preceding `/** ... */` docblock, if any.
//! [`signature_summary`] renders the container collapsed form from stored
//! member signatures (spec §16.2).
//!
//! A declaration can name several members at once (`public int $left,
//! $right = 2;`). Each member is its own symbol, and its stored signature is
//! the shared modifier/type prefix plus that member's own element text, so it
//! names only that member (`public int $right = 2`) rather than repeating the
//! whole declaration under each name.

use rivet_core::{ExtractedSymbol, Span, SymbolKind};
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

/// The collapsed declaration header for one element of a multi-name
/// declaration.
///
/// A property or constant declaration can name several members at once. The
/// shared prefix — modifiers, type, and the declaration keyword — runs from the
/// declaration start to its first element; appending this element's own text
/// gives a signature that names only this member:
/// `public int $right = 2`, not `public int $left, $right = 2`.
pub(crate) fn element_header(declaration: Node<'_>, element: Node<'_>, source: &[u8]) -> String {
    let mut cursor = declaration.walk();
    let first = declaration
        .children(&mut cursor)
        .find(|child| matches!(child.kind(), "property_element" | "const_element"));
    let prefix_end = first.map_or(element.start_byte(), |node| node.start_byte());
    let prefix = text(source, declaration.start_byte(), prefix_end);
    let member = text(source, element.start_byte(), element.end_byte());
    collapse_whitespace(&format!("{prefix}{member}"))
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
/// and `}`. A method member with a body collapses it to ` { … }`; a bodyless
/// method (abstract or interface) and every other member kind (property,
/// constant, enum case) end with `;`. A non-container symbol returns its own
/// signature unchanged.
///
/// A promoted constructor property is declared inside the constructor's
/// parameter list and already appears there, so it is not listed again as a
/// standalone member: the constructor signature is the one place a promoted
/// property is visible in the collapsed form. It remains an addressable symbol
/// in the extracted facts; only this summary omits the duplicate line.
///
/// `source` is used to tell a method with a body from a bodyless one: the
/// stored signature stops at the body `{`, so only the declaration span shows
/// whether a body follows.
pub fn signature_summary(
    symbol: &ExtractedSymbol,
    members: &[ExtractedSymbol],
    source: &str,
) -> String {
    if !is_container(symbol.kind) {
        return symbol.signature.clone().unwrap_or_default();
    }

    let mut summary = String::new();
    summary.push_str(symbol.signature.as_deref().unwrap_or(""));
    summary.push_str("\n{\n");
    let mut emitted: Vec<Span> = Vec::new();
    for member in members {
        // Skip a member nested inside an already-emitted member. In practice
        // this is the promoted constructor property, whose span lies inside
        // the constructor's. Members of one multi-name declaration share a
        // span but do not contain each other, so they are all listed.
        if emitted
            .iter()
            .any(|span| strictly_contains(span, &member.span))
        {
            continue;
        }
        let signature = member.signature.as_deref().unwrap_or("");
        summary.push_str("    ");
        summary.push_str(signature);
        match member.kind {
            // Only a method that actually has a body collapses to `{ … }`; an
            // abstract or interface method has nothing to replace.
            SymbolKind::Method if has_body(member, source) => summary.push_str(" { … }\n"),
            _ => summary.push_str(";\n"),
        }
        emitted.push(member.span);
    }
    summary.push('}');
    summary
}

/// Orders a container's direct members into source order from stored facts.
///
/// Extraction already yields members in source order, but a caller that
/// rebuilds them from stored rows (the `context` signature form) only has
/// spans, names, and signatures. Sorting by `(start_byte, end_byte
/// descending, qualified_name)` recovers source order everywhere except
/// among the members of one multi-name declaration (`public int $left,
/// $right = 2;`), which share a span. For such a group the declaration's
/// collapsed text is scanned left to right: every member's stored signature is
/// the shared prefix plus that member's own element text (see
/// [`element_header`]), so each element is matched at the scan position in
/// turn. If the scan cannot account for every element (an interleaved
/// comment, say), the group keeps its `qualified_name` order rather than a
/// guessed one.
pub fn order_members(members: &mut [ExtractedSymbol], source: &str) {
    members.sort_by(|a, b| {
        a.span
            .start_byte()
            .cmp(&b.span.start_byte())
            .then_with(|| b.span.end_byte().cmp(&a.span.end_byte()))
            .then_with(|| a.qualified_name.as_bytes().cmp(b.qualified_name.as_bytes()))
    });
    let mut start = 0;
    while start < members.len() {
        let span = members[start].span;
        let mut end = start + 1;
        while end < members.len() && members[end].span == span {
            end += 1;
        }
        if end - start > 1
            && let Some(order) = shared_span_order(&members[start..end], source)
        {
            let group: Vec<ExtractedSymbol> = order
                .iter()
                .map(|&index| members[start + index].clone())
                .collect();
            members[start..end].clone_from_slice(&group);
        }
        start = end;
    }
}

/// The source order of members sharing one declaration span, as indices into
/// `group`, or `None` when the declaration text does not account for them.
fn shared_span_order(group: &[ExtractedSymbol], source: &str) -> Option<Vec<usize>> {
    let signatures: Vec<&str> = group
        .iter()
        .map(|member| member.signature.as_deref())
        .collect::<Option<_>>()?;
    let bytes = source.as_bytes();
    let start = group[0].span.start_byte() as usize;
    let end = group[0].span.end_byte() as usize;
    if start > end || end > bytes.len() {
        return None;
    }
    let raw = text(bytes, start, end);
    let raw = raw.trim_end();
    let declaration = collapse_whitespace(raw.strip_suffix(';').unwrap_or(raw));

    // The shared prefix ends with the whitespace before the first element
    // (`public int `); back the common byte prefix off to its last space so a
    // shared leading name fragment (`$left`, `$lower`) is not taken as prefix.
    let first = signatures[0].as_bytes();
    let mut common = first.len();
    for signature in &signatures[1..] {
        common = common.min(
            first
                .iter()
                .zip(signature.as_bytes())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    let prefix = first[..common].iter().rposition(|byte| *byte == b' ')? + 1;
    if !declaration.as_bytes().starts_with(&first[..prefix]) {
        return None;
    }

    let elements: Vec<&[u8]> = signatures
        .iter()
        .map(|signature| &signature.as_bytes()[prefix..])
        .collect();
    let text = declaration.as_bytes();
    let mut position = prefix;
    let mut order = Vec::with_capacity(group.len());
    let mut used = vec![false; group.len()];
    while order.len() < group.len() {
        let rest = &text[position..];
        // The longest element that matches here and ends at a separator, so
        // `$a` never claims the start of `$ab`.
        let chosen = (0..elements.len())
            .filter(|&index| {
                !used[index]
                    && rest.starts_with(elements[index])
                    && matches!(rest.get(elements[index].len()), None | Some(b',' | b' '))
            })
            .max_by(|&a, &b| elements[a].len().cmp(&elements[b].len()).then(b.cmp(&a)))?;
        used[chosen] = true;
        order.push(chosen);
        position += elements[chosen].len();
        while text.get(position) == Some(&b' ') {
            position += 1;
        }
        if text.get(position) == Some(&b',') {
            position += 1;
        }
        while text.get(position) == Some(&b' ') {
            position += 1;
        }
    }
    (position == text.len()).then_some(order)
}

/// Whether `outer` strictly contains `inner`.
///
/// Strict so that two members sharing one declaration span (a multi-name
/// property or constant) are not treated as nesting.
fn strictly_contains(outer: &Span, inner: &Span) -> bool {
    outer.start_byte() <= inner.start_byte()
        && inner.end_byte() <= outer.end_byte()
        && (outer.start_byte() < inner.start_byte() || inner.end_byte() < outer.end_byte())
}

/// Whether the declaration at `symbol.span` ends with a body block.
///
/// The stored signature stops at the body `{`, so it cannot distinguish an
/// abstract or interface method from one with a body. The declaration span
/// does: a body ends with `}`, a bodyless declaration with `;`.
fn has_body(symbol: &ExtractedSymbol, source: &str) -> bool {
    let bytes = source.as_bytes();
    let start = symbol.span.start_byte() as usize;
    let end = (symbol.span.end_byte() as usize).min(bytes.len());
    bytes[start..end]
        .iter()
        .rev()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'}')
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

    use super::{order_members, signature_summary};

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

    /// An interface summary lists its bodyless methods without a body marker:
    /// an interface method has no body to collapse.
    #[test]
    fn interface_collapsed_form_lists_bodyless_methods_without_a_body() {
        let (source, extracted) = extract_fixture("Interface.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");
        let (interface, members) = container_members(&extracted, "App\\Contracts\\Named");

        let expected = "\
interface Named
{
    public const KIND = 'named';
    public function name(): string;
    public function set(int $value): void;
}";
        let summary = signature_summary(&interface, &members, &source);
        assert_eq!(summary, expected);
        assert!(
            !summary.contains('…'),
            "no interface member may carry a body marker: {summary:?}"
        );
    }

    /// An interface summary proves the body-marker rule independently of the
    /// exact expected string: no rendered member line carries ` { … }`, because
    /// an interface method declares no body.
    #[test]
    fn interface_summary_has_no_body_marker() {
        let (source, extracted) = extract_fixture("Interface.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");
        let (interface, members) = container_members(&extracted, "App\\Contracts\\Named");

        let summary = signature_summary(&interface, &members, &source);
        assert!(
            !summary.contains('…') && !summary.contains("{ … }"),
            "an interface method has no body to collapse: {summary:?}"
        );
        let method_lines = summary
            .lines()
            .filter(|line| line.contains("function "))
            .collect::<Vec<_>>();
        assert_eq!(method_lines.len(), 2, "{summary:?}");
        assert!(
            method_lines
                .iter()
                .all(|line| line.trim_end().ends_with(';')),
            "each interface method line ends with `;`: {method_lines:?}"
        );
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

    /// Each member kind appears once per declared name: a multi-name
    /// declaration renders one line per name with only that name, a bodyless
    /// abstract method has no body marker, and a promoted property appears once
    /// inside the constructor signature rather than again as a member.
    #[test]
    fn class_collapsed_form_covers_multi_name_and_promoted_members() {
        let (source, extracted) = extract_fixture("Members.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");
        let (class, members) = container_members(&extracted, "App\\Members\\AbstractThing");

        let expected = "\
abstract class AbstractThing
{
    public const FIRST = 1;
    public const SECOND = 2;
    public static int $count = 0;
    public readonly string $title;
    public int $left;
    public int $right = 2;
    abstract public function describe(): string;
    public function __construct(private int $seed, public string $tag = 'x') { … }
}";
        assert_eq!(signature_summary(&class, &members, &source), expected);
    }

    /// Parses inline PHP source and extracts it.
    fn extract_source(source: &str) -> ExtractedFile {
        let mut parser = Parser::new();
        parser
            .set_language(&grammar(LanguageId::Php))
            .expect("pinned PHP grammar must load");
        let tree = parser
            .parse(source.as_bytes(), None)
            .expect("parser must return a tree");
        php::extract(source.as_bytes(), &tree)
    }

    /// Reversing the extracted members and re-ordering them from stored facts
    /// recovers source order, so the summary is unchanged.
    #[test]
    fn order_members_recovers_source_order_from_any_order() {
        let (source, extracted) = extract_fixture("Members.php");
        let source = String::from_utf8(source).expect("fixture is UTF-8");
        let (class, members) = container_members(&extracted, "App\\Members\\AbstractThing");
        let expected = signature_summary(&class, &members, &source);

        let mut shuffled: Vec<_> = members.iter().rev().cloned().collect();
        order_members(&mut shuffled, &source);
        assert_eq!(shuffled, members);
        assert_eq!(signature_summary(&class, &shuffled, &source), expected);
    }

    /// Members sharing one span are ordered by their position in the
    /// declaration, not by name, even when a value repeats another element's
    /// text or one name is a prefix of another.
    #[test]
    fn order_members_orders_a_shared_span_by_position() {
        let source = "<?php\nfinal class M\n{\n    public const ZED = 'A = 1', A = 1, AB = 2;\n    public int $zeta,$alpha = 2,\n        $al;\n}\n";
        let extracted = extract_source(source);
        let (class, members) = container_members(&extracted, "M");
        let mut sorted = members.clone();
        sorted.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
        order_members(&mut sorted, source);
        let names: Vec<&str> = sorted.iter().map(|member| member.name.as_str()).collect();
        assert_eq!(names, vec!["ZED", "A", "AB", "$zeta", "$alpha", "$al"]);
        assert_eq!(sorted, members);
        assert_eq!(
            signature_summary(&class, &sorted, source),
            signature_summary(&class, &members, source)
        );
    }

    /// When the declaration text does not account for every element (a
    /// comment between elements), the group keeps `qualified_name` order
    /// rather than a guessed one.
    #[test]
    fn order_members_falls_back_to_name_order_when_the_scan_fails() {
        let source = "<?php\nfinal class M\n{\n    public int $b, /* note */ $a;\n}\n";
        let extracted = extract_source(source);
        let (_class, members) = container_members(&extracted, "M");
        let mut ordered = members.clone();
        order_members(&mut ordered, source);
        let names: Vec<&str> = ordered.iter().map(|member| member.name.as_str()).collect();
        assert_eq!(names, vec!["$a", "$b"]);
    }
}
