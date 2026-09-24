//! TypeScript declaration signatures and doc comments (T42).
//!
//! A signature is the declaration header exactly as written, mirroring the
//! PHP adapter: from the span start (so it includes `export`, `export
//! default`, `declare`, decorators on a class, and modifiers) through the
//! start of the body, or through the end of a bodyless declaration, with
//! every whitespace run collapsed to one space and a trailing `;` dropped.
//!
//! The body is the `{ ... }` of a function, method, class, interface, enum, or
//! namespace. A `const` or class field bound to an arrow function, function
//! expression, generator function, or class expression stops at that value's
//! body, so `export const double = (n: number): number => n * 2;` has the
//! signature `export const double = (n: number): number =>`. Any other
//! binding, a type alias, a property, and an enum member keep their whole
//! text, value included, as PHP constants and properties do.
//!
//! A getter and a setter keep their keyword (`get label(): string`, `set
//! label(value: string)`), so the two symbols of an accessor pair are told
//! apart by signature as well as by ordinal.
//!
//! A doc comment is the text of a `/** ... */` block comment directly above
//! the declaration's span, attached by the PHP rule: a `//` or plain `/* */`
//! comment does not attach, and neither does one separated by a blank line or
//! by another statement. A method's decorators sit between its doc comment
//! and the method node in the pinned grammar (a field's are inside the field
//! node), so they are skipped.

use tree_sitter::Node;

/// The collapsed header of `node`, whose span starts at `span`.
///
/// `span` is `node` itself or the `export`/`declare` wrapper around it.
pub(super) fn declaration(span: Node<'_>, node: Node<'_>, source: &[u8]) -> String {
    let end = body_start(node).unwrap_or(span.end_byte());
    collapse(&text(source, span.start_byte(), end))
}

/// The collapsed header of one declarator of a `const` statement.
///
/// A statement can bind several names (`export const MIN = 1, MAX = 10;`).
/// Each is its own symbol, and its signature is the shared prefix (from the
/// span start to the first declarator) plus that declarator's own text, so it
/// names only that binding (`export const MAX = 10`), as a PHP multi-name
/// constant does.
pub(super) fn binding(
    span: Node<'_>,
    first: Node<'_>,
    declarator: Node<'_>,
    source: &[u8],
) -> String {
    let prefix = text(source, span.start_byte(), first.start_byte());
    let end = body_start(declarator).unwrap_or(declarator.end_byte());
    let element = text(source, declarator.start_byte(), end);
    collapse(&format!("{prefix}{element}"))
}

/// Where the header of `node` ends: the start of its body, if it has one.
fn body_start(node: Node<'_>) -> Option<usize> {
    match node.kind() {
        "variable_declarator" | "public_field_definition" => node
            .child_by_field_name("value")
            .filter(|value| {
                matches!(
                    value.kind(),
                    "arrow_function" | "function_expression" | "generator_function" | "class"
                )
            })
            .and_then(|value| value.child_by_field_name("body"))
            .map(|body| body.start_byte()),
        _ => node
            .child_by_field_name("body")
            .map(|body| body.start_byte()),
    }
}

/// The attached JSDoc comment of the declaration spanning `node`, or `None`.
///
/// The delimiters are kept and the comment's own indentation is removed from
/// every line, exactly as the PHP adapter does.
pub(super) fn doc_comment(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut anchor = node;
    let mut previous = node.prev_named_sibling();
    while let Some(decorator) = previous.filter(|sibling| sibling.kind() == "decorator") {
        anchor = decorator;
        previous = decorator.prev_named_sibling();
    }
    let comment = previous?;
    if comment.kind() != "comment" {
        return None;
    }
    let raw = text(source, comment.start_byte(), comment.end_byte());
    // `/**/` is an empty block comment, not a doc comment.
    if !raw.starts_with("/**") || raw.starts_with("/**/") {
        return None;
    }
    // A blank line between the comment and the declaration breaks
    // attachment; one newline (comment above) and none (same line) attach.
    let between = &source[comment.end_byte()..anchor.start_byte()];
    if between.iter().filter(|byte| **byte == b'\n').count() > 1 {
        return None;
    }
    Some(trim_indentation(
        &raw,
        line_indent(source, comment.start_byte()),
    ))
}

/// The number of space/tab bytes immediately before `start` on its line.
fn line_indent(source: &[u8], start: usize) -> usize {
    let mut index = start;
    while index > 0 && matches!(source[index - 1], b' ' | b'\t') {
        index -= 1;
    }
    start - index
}

/// Removes up to `indent` leading space/tab bytes from every line, keeping the
/// comment's own ` *` alignment.
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

/// Collapses every whitespace run to one space, trims, and drops one trailing
/// `;`.
fn collapse(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.strip_suffix(';') {
        Some(stripped) => stripped.trim_end().to_string(),
        None => collapsed,
    }
}

/// Decodes a source byte range as UTF-8 (lossy only if the caller violated
/// the valid-UTF-8 input domain).
fn text(source: &[u8], start: usize, end: usize) -> String {
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::collapse;

    #[test]
    fn collapse_joins_whitespace_and_drops_one_semicolon() {
        assert_eq!(
            collapse("export  function f(\n  a: number,\n): void;"),
            "export function f( a: number, ): void"
        );
        assert_eq!(
            collapse("declare const K: number ;"),
            "declare const K: number"
        );
        assert_eq!(collapse("x = 1"), "x = 1");
    }
}
