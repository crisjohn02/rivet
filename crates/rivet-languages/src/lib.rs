//! Language extraction and receiver hints.
//!
//! T02 exposes the grammar surface: language identifiers, the Tree-sitter
//! grammars for the enabled features, and the extension dispatch that decides
//! which files a build can handle. Extraction, queries, and receiver hints
//! arrive in later tasks.

use std::path::Path;

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

impl LanguageId {
    /// The contract language name.
    ///
    /// TSX shares the `"typescript"` language name; the grammar variant is an
    /// internal distinction.
    pub fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "lang-php")]
            LanguageId::Php => "php",
            #[cfg(feature = "lang-typescript")]
            LanguageId::Typescript | LanguageId::Tsx => "typescript",
        }
    }
}

/// Returns the language whose grammar handles `rel_path`, or `None` when the
/// extension maps to no compiled language.
///
/// `rel_path` is a repository-relative `/`-separated path. Only `.php` (PHP)
/// and `.ts`/`.tsx` (TypeScript) are registered in v0.1; JavaScript extensions
/// are deliberately not inferred (docs/ADDING-A-LANGUAGE.md).
pub fn language_for_path(rel_path: &str) -> Option<LanguageId> {
    let extension = Path::new(rel_path).extension()?.to_str()?;
    match extension {
        #[cfg(feature = "lang-php")]
        "php" => Some(LanguageId::Php),
        #[cfg(feature = "lang-typescript")]
        "ts" => Some(LanguageId::Typescript),
        #[cfg(feature = "lang-typescript")]
        "tsx" => Some(LanguageId::Tsx),
        _ => None,
    }
}

/// Reports whether `name` (`"php"` or `"typescript"`) is compiled into this
/// binary.
///
/// A configured or requested language that is not compiled is an argument
/// error, never a silent downgrade (OUTPUT-CONTRACT "Flag applicability").
pub fn is_language_compiled(name: &str) -> bool {
    match name {
        #[cfg(feature = "lang-php")]
        "php" => true,
        #[cfg(feature = "lang-typescript")]
        "typescript" => true,
        _ => false,
    }
}

/// The grammar/extractor fingerprint recorded in `meta.extractor_fingerprint`.
///
/// It lists every grammar compiled into this build with the exact grammar crate
/// version pinned in the workspace manifest, so changing the enabled grammar
/// set or bumping a grammar invalidates stored facts. `env!("CARGO_PKG_VERSION")`
/// names this crate rather than its dependencies, so the grammar crate versions
/// are compile-time constants kept in sync with the root `Cargo.toml` pins
/// (`tree-sitter-php = "=0.24.2"`, `tree-sitter-typescript = "=0.23.2"`).
#[cfg(all(feature = "lang-php", feature = "lang-typescript"))]
pub const EXTRACTOR_FINGERPRINT: &str = "php=0.24.2;ts=0.23.2";

#[cfg(all(feature = "lang-php", not(feature = "lang-typescript")))]
pub const EXTRACTOR_FINGERPRINT: &str = "php=0.24.2";

#[cfg(all(not(feature = "lang-php"), feature = "lang-typescript"))]
pub const EXTRACTOR_FINGERPRINT: &str = "ts=0.23.2";

#[cfg(not(any(feature = "lang-php", feature = "lang-typescript")))]
pub const EXTRACTOR_FINGERPRINT: &str = "";

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
