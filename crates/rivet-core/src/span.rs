//! Byte spans and position mapping.
//!
//! All byte offsets are zero-based and half-open. Lines and UTF-8 byte columns
//! are one-based; end lines are inclusive. CRLF is two bytes and is never
//! normalized, and columns count bytes, not display or UTF-16 units.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A zero-based, half-open byte range `[start_byte, end_byte)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Span {
    start_byte: u32,
    end_byte: u32,
}

impl Span {
    /// Creates a span, requiring `start_byte < end_byte`.
    ///
    /// Empty and reversed spans are rejected; a valid span always covers at
    /// least one byte.
    pub fn new(start_byte: u32, end_byte: u32) -> Result<Span, SpanError> {
        if start_byte < end_byte {
            Ok(Span {
                start_byte,
                end_byte,
            })
        } else {
            Err(SpanError {
                start_byte,
                end_byte,
            })
        }
    }

    /// The inclusive start offset.
    pub fn start_byte(&self) -> u32 {
        self.start_byte
    }

    /// The exclusive end offset.
    pub fn end_byte(&self) -> u32 {
        self.end_byte
    }

    /// The number of bytes covered by the span.
    pub fn byte_len(&self) -> u32 {
        self.end_byte - self.start_byte
    }
}

/// Error returned when a [`Span`] violates `start_byte < end_byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanError {
    start_byte: u32,
    end_byte: u32,
}

impl SpanError {
    /// The rejected start offset.
    pub fn start_byte(&self) -> u32 {
        self.start_byte
    }

    /// The rejected end offset.
    pub fn end_byte(&self) -> u32 {
        self.end_byte
    }
}

impl fmt::Display for SpanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid span: start_byte ({}) must be less than end_byte ({})",
            self.start_byte, self.end_byte
        )
    }
}

impl std::error::Error for SpanError {}

/// A one-based line and one-based UTF-8 byte column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    /// One-based line number.
    pub line: u32,
    /// One-based UTF-8 byte column within the line.
    pub column: u32,
}

/// Maps byte offsets in one source buffer to line/column positions.
///
/// CRLF is treated as two bytes: the carriage return stays on the preceding
/// line and the line feed starts the next line. Columns count UTF-8 bytes, so a
/// multibyte character advances the column by its byte length.
#[derive(Debug, Clone)]
pub struct LineIndex {
    line_starts: Vec<u32>,
    len: u32,
}

impl LineIndex {
    /// Builds an index over raw source bytes.
    pub fn new(source: &[u8]) -> LineIndex {
        let mut line_starts = Vec::new();
        line_starts.push(0);
        for (offset, byte) in source.iter().enumerate() {
            if *byte == b'\n' {
                line_starts.push(offset as u32 + 1);
            }
        }
        LineIndex {
            line_starts,
            len: source.len() as u32,
        }
    }

    /// Resolves a byte offset to a one-based line and byte column.
    ///
    /// Returns `None` when `byte` lies past the end of the source. The offset
    /// equal to the source length is accepted and maps to the position just
    /// after the final byte.
    pub fn line_col(&self, byte: u32) -> Option<LineCol> {
        if byte > self.len {
            return None;
        }
        let index = self.line_starts.partition_point(|&start| start <= byte) - 1;
        let column = byte - self.line_starts[index] + 1;
        Some(LineCol {
            line: index as u32 + 1,
            column,
        })
    }

    /// The one-based line containing `span.start_byte()`.
    pub fn start_line(&self, span: Span) -> u32 {
        let offset = span.start_byte().min(self.len);
        self.line_col(offset)
            .map(|position| position.line)
            .unwrap_or(1)
    }

    /// The inclusive one-based end line: the line containing `end_byte - 1`.
    ///
    /// A span ending immediately after a line feed therefore ends on the line
    /// that feed terminates, not on the following line.
    pub fn end_line(&self, span: Span) -> u32 {
        let offset = (span.end_byte() - 1).min(self.len);
        self.line_col(offset)
            .map(|position| position.line)
            .unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::{LineIndex, Span};

    #[test]
    fn line_and_byte_column_ignore_display_width() {
        // "é line\n" is 8 bytes: é takes two, so byte columns run ahead of
        // display columns.
        let source = "é line\nsecond\r\nthird";
        let index = LineIndex::new(source.as_bytes());

        assert_eq!(index.line_col(0).unwrap().line, 1);
        assert_eq!(index.line_col(0).unwrap().column, 1);
        // Offset 3 is 'l'; two bytes of 'é' push the byte column to 4.
        assert_eq!(index.line_col(3).unwrap().column, 4);
        // The line feed itself is the last byte of line 1, at byte column 8.
        let newline = index.line_col(7).unwrap();
        assert_eq!((newline.line, newline.column), (1, 8));

        // Line 2 starts right after the LF. Its CR is a normal byte.
        assert_eq!(index.line_col(8).unwrap().line, 2);
        assert_eq!(index.line_col(8).unwrap().column, 1);
        assert_eq!(index.line_col(14).unwrap().column, 7);

        assert_eq!(index.line_col(16).unwrap().line, 3);
        assert_eq!(index.line_col(16).unwrap().column, 1);

        // Past the end of the source there is no position.
        assert!(index.line_col(source.len() as u32 + 1).is_none());
    }

    #[test]
    fn end_line_is_inclusive_and_newline_aware() {
        let source = b"alpha\nbeta\n";
        let index = LineIndex::new(source);

        let first = Span::new(0, 5).unwrap();
        assert_eq!(index.start_line(first), 1);
        assert_eq!(index.end_line(first), 1);

        // [0, 6) includes the first LF: end_byte - 1 is the LF on line 1.
        let through_newline = Span::new(0, 6).unwrap();
        assert_eq!(index.end_line(through_newline), 1);

        // [0, 7) reaches the 'b' of "beta" on line 2.
        let onto_second = Span::new(0, 7).unwrap();
        assert_eq!(index.end_line(onto_second), 2);

        // The span ending at the source length ends on the last non-empty
        // line, because end_byte - 1 is the trailing LF on that line.
        let all = Span::new(0, source.len() as u32).unwrap();
        assert_eq!(index.end_line(all), 2);

        // The byte just past the source is the empty line 3.
        assert_eq!(index.line_col(source.len() as u32).unwrap().line, 3);
    }

    #[test]
    fn span_rejects_empty_and_reversed_ranges() {
        assert!(Span::new(0, 0).is_err());
        assert!(Span::new(5, 5).is_err());
        assert!(Span::new(5, 3).is_err());
        assert!(Span::new(0, 1).is_ok());
        assert_eq!(Span::new(2, 9).unwrap().byte_len(), 7);
    }
}
