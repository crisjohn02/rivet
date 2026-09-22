//! Owned extraction records, symbols, uses, imports, scopes, spans, errors.
//!
//! This crate currently provides the shared, database-free primitives used by
//! later extraction and query work: byte [`Span`]s with line/column mapping,
//! canonical [`SymbolId`]s with duplicate ordinals, the declaration,
//! resolution, and reference-kind enums, nearest-root discovery, and validated
//! repository [`Config`].

pub mod config;
pub mod id;
pub mod kinds;
pub mod root;
pub mod span;
#[cfg(test)]
mod test_support;

pub use config::{
    Collapse, Config, ConfigError, ContextConfig, Freshness, IndexConfig, LanguagesConfig,
    OutputConfig,
};
pub use id::{SymbolId, SymbolIdError, assign_ordinals};
pub use kinds::{KindParseError, RefKind, Resolution, SymbolKind};
pub use root::{RootError, RootInfo, discover_root};
pub use span::{LineCol, LineIndex, Span, SpanError};
