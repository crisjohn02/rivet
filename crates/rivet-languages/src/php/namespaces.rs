//! PHP namespace block layout (AF1).
//!
//! PHP has two namespace syntaxes. The unbraced form, `namespace X;`, runs
//! until the next namespace statement or the end of the file. The braced form,
//! `namespace X { ... }`, runs for its braces, and `namespace { ... }` declares
//! names in the global namespace. A file with no namespace statement is in the
//! global namespace throughout.
//!
//! [`NamespaceLayout`] maps a byte offset to the one namespace block that owns
//! it. Qualified names (`mod.rs`) and lexical scopes (`uses.rs`) share this
//! layout, so a symbol's identity and the scope its uses resolve in always
//! agree.
//!
//! A position that cannot be attributed to exactly one block is
//! [`Attribution::Unattributed`]. That covers code before the first unbraced
//! namespace statement, code outside every braced block, every position in a
//! file that mixes the braced and unbraced forms, and every position in a file
//! with a namespace definition that is not a top-level statement. PHP rejects
//! all of these at compile time; the resolver records no binding for a use
//! there rather than guessing a namespace.

use tree_sitter::Node;

/// Which namespace block owns one byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Attribution {
    /// The file has no namespace statement: the global namespace.
    NoNamespace,
    /// The 0-based namespace block in source order.
    Block(usize),
    /// No single block can be attributed.
    Unattributed,
}

/// One namespace block.
#[derive(Debug, Clone)]
struct Block {
    /// The namespace name as written, or `None` for a global `namespace { }`.
    name: Option<String>,
    /// First byte the block owns.
    start: u32,
    /// One past the last byte the block owns.
    end: u32,
}

/// The namespace blocks of one file, in source order.
#[derive(Debug, Clone)]
pub(crate) struct NamespaceLayout {
    /// `None` when the file has no namespace statement.
    blocks: Option<Vec<Block>>,
    /// Set when no position in the file can be attributed (mixed forms or a
    /// nested namespace definition).
    invalid: bool,
}

impl NamespaceLayout {
    /// Computes the layout of a parsed, error-free file.
    pub(crate) fn new(root: Node<'_>, source: &[u8]) -> NamespaceLayout {
        let mut cursor = root.walk();
        let top_level: Vec<Node<'_>> = root
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "namespace_definition")
            .collect();
        let total = count_namespace_definitions(root);
        if total == 0 {
            return NamespaceLayout {
                blocks: None,
                invalid: false,
            };
        }
        let braced = top_level
            .iter()
            .filter(|node| node.child_by_field_name("body").is_some())
            .count();
        // A nested namespace definition, or a mix of the braced and unbraced
        // forms, cannot be attributed block by block.
        let invalid = total != top_level.len() || (braced != 0 && braced != top_level.len());
        let name_of = |node: &Node<'_>| {
            node.child_by_field_name("name").map(|name| {
                String::from_utf8_lossy(&source[name.start_byte()..name.end_byte()]).into_owned()
            })
        };
        let blocks: Vec<Block> = if braced == top_level.len() {
            top_level
                .iter()
                .map(|node| Block {
                    name: name_of(node),
                    start: node.start_byte() as u32,
                    end: node.end_byte() as u32,
                })
                .collect()
        } else {
            top_level
                .iter()
                .enumerate()
                .map(|(index, node)| Block {
                    name: name_of(node),
                    start: node.start_byte() as u32,
                    end: top_level
                        .get(index + 1)
                        .map(|next| next.start_byte() as u32)
                        .unwrap_or(u32::MAX),
                })
                .collect()
        };
        NamespaceLayout {
            blocks: Some(blocks),
            invalid,
        }
    }

    /// The block that owns the byte offset `byte`.
    pub(crate) fn attribution(&self, byte: u32) -> Attribution {
        let Some(blocks) = &self.blocks else {
            return Attribution::NoNamespace;
        };
        if self.invalid {
            return Attribution::Unattributed;
        }
        match blocks
            .iter()
            .position(|block| block.start <= byte && byte < block.end)
        {
            Some(index) => Attribution::Block(index),
            None => Attribution::Unattributed,
        }
    }

    /// The namespace name that qualifies a declaration starting at `byte`.
    ///
    /// `None` means a bare global name: the file has no namespace, the block
    /// is a global `namespace { }`, or the position is unattributed (invalid
    /// PHP, whose uses the resolver never binds).
    pub(crate) fn namespace_at(&self, byte: u32) -> Option<String> {
        match self.attribution(byte) {
            Attribution::Block(index) => self
                .blocks
                .as_ref()
                .and_then(|blocks| blocks[index].name.clone()),
            Attribution::NoNamespace | Attribution::Unattributed => None,
        }
    }

    /// The number of attributable namespace blocks: `0` for a file with none
    /// and for a file in which no position can be attributed.
    pub(crate) fn block_count(&self) -> usize {
        if self.invalid {
            return 0;
        }
        self.blocks.as_ref().map_or(0, Vec::len)
    }

    /// Whether the file has at least one namespace statement.
    pub(crate) fn is_namespaced(&self) -> bool {
        self.blocks.is_some()
    }
}

/// Counts every `namespace_definition` node in the tree.
fn count_namespace_definitions(root: Node<'_>) -> usize {
    let mut count = 0;
    let mut cursor = root.walk();
    loop {
        if cursor.node().kind() == "namespace_definition" {
            count += 1;
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return count;
            }
        }
    }
}
