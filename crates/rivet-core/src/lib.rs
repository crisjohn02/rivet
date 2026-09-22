//! Owned extraction records, symbols, uses, imports, scopes, spans, errors.
//!
//! This crate currently provides the shared, database-free primitives used by
//! later extraction and query work: byte [`Span`]s with line/column mapping,
//! canonical [`SymbolId`]s with duplicate ordinals, and the declaration,
//! resolution, and reference-kind enums.

pub mod id;
pub mod kinds;
pub mod span;

pub use id::{SymbolId, SymbolIdError, assign_ordinals};
pub use kinds::{KindParseError, RefKind, Resolution, SymbolKind};
pub use span::{LineCol, LineIndex, Span, SpanError};
