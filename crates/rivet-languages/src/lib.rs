//! Language extraction and receiver hints.
//!
//! T02 exposes only the grammar surface: language identifiers and the
//! Tree-sitter grammars for the enabled features. Extraction, queries, and
//! receiver hints arrive in later tasks.

/// A source language supported by the enabled crate features.
///
/// A variant exists only when its feature is enabled, so a build with
/// `--no-default-features` compiles an empty enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    /// PHP, parsed with the mixed HTML/PHP grammar.
    #[cfg(feature = "lang-php")]
    Php,
    /// TypeScript source (`.ts`).
    #[cfg(feature = "lang-typescript")]
    Typescript,
    /// TypeScript with JSX (`.tsx`).
    #[cfg(feature = "lang-typescript")]
    Tsx,
}

/// Return the Tree-sitter grammar for `id`.
///
/// Only grammars whose variant is enabled at compile time can be requested;
/// the returned language is ready to hand to a `tree_sitter::Parser`.
pub fn grammar(id: LanguageId) -> tree_sitter::Language {
    match id {
        #[cfg(feature = "lang-php")]
        LanguageId::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        #[cfg(feature = "lang-typescript")]
        LanguageId::Typescript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        #[cfg(feature = "lang-typescript")]
        LanguageId::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
    }
}
