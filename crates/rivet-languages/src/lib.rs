//! Language extraction and receiver hints.
//!
//! T02 exposes the grammar surface: language identifiers, the Tree-sitter
//! grammars for the enabled features, and the extension dispatch that decides
//! which files a build can handle. Extraction, queries, and receiver hints
//! arrive in later tasks.

use std::path::Path;

#[cfg(feature = "lang-php")]
pub mod php;

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
///
/// The trailing `fact-schema=N` component covers the *shape and meaning* of the
/// persisted extractor facts, not just the grammar. Grammar versions alone are
/// not enough: T21 changed what the adapter records (`new_bindings` gained
/// `direct_new` and `block`, and `UseHint::NewExpr` gained `use_block`) without
/// touching a grammar, so a repository indexed by the previous binary would be
/// reused without reparsing and its stale facts would deserialize with serde
/// defaults. T21a widened `new_bindings` again to include recognized rebinding
/// forms, so a variable rebound by `foreach`, destructuring, `catch`, and the
/// like is no longer treated as singly assigned. T21b adds `call_args` and
/// `unanalysable` to the persisted scope facts, so a scope containing a
/// by-reference argument or an unanalysable construct (a dynamic variable
/// write, `$GLOBALS` write, `extract`, `eval`) records no `new`-receiver
/// binding. A change to any persisted fact shape or to the meaning of an
/// existing field must bump this integer so the next refresh reparses every
/// enabled-language file. The current schema is 3 (post-T21b); it was
/// introduced by T22 together with this component.
#[cfg(all(feature = "lang-php", feature = "lang-typescript"))]
pub const EXTRACTOR_FINGERPRINT: &str = "php=0.24.2;ts=0.23.2;fact-schema=3";

#[cfg(all(feature = "lang-php", not(feature = "lang-typescript")))]
pub const EXTRACTOR_FINGERPRINT: &str = "php=0.24.2;fact-schema=3";

#[cfg(all(not(feature = "lang-php"), feature = "lang-typescript"))]
pub const EXTRACTOR_FINGERPRINT: &str = "ts=0.23.2;fact-schema=3";

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

#[cfg(test)]
mod tests {
    use super::EXTRACTOR_FINGERPRINT;

    /// The locked `version` of `package`, read from the workspace `Cargo.lock`.
    fn locked_version(lock: &str, package: &str) -> Option<String> {
        let needle = format!("name = \"{package}\"");
        let mut lines = lock.lines();
        while let Some(line) = lines.next() {
            if line.trim() != needle {
                continue;
            }
            for next in lines.by_ref() {
                let trimmed = next.trim();
                if let Some(version) = trimmed.strip_prefix("version = \"") {
                    return Some(version.trim_end_matches('"').to_string());
                }
                if trimmed.starts_with("[[package]]") {
                    break;
                }
            }
        }
        None
    }

    /// The hand-maintained fingerprint must name the grammar versions that are
    /// actually locked, or stored facts silently outlive a grammar bump.
    #[test]
    fn fingerprint_tracks_locked_grammar_versions() {
        let lock_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
        let lock = std::fs::read_to_string(&lock_path).expect("read workspace Cargo.lock");

        #[cfg(feature = "lang-php")]
        {
            let version = locked_version(&lock, "tree-sitter-php")
                .expect("tree-sitter-php must be pinned in Cargo.lock");
            assert!(
                EXTRACTOR_FINGERPRINT.contains(&version),
                "EXTRACTOR_FINGERPRINT {EXTRACTOR_FINGERPRINT:?} does not name locked \
                 tree-sitter-php {version}"
            );
        }

        #[cfg(feature = "lang-typescript")]
        {
            let version = locked_version(&lock, "tree-sitter-typescript")
                .expect("tree-sitter-typescript must be pinned in Cargo.lock");
            assert!(
                EXTRACTOR_FINGERPRINT.contains(&version),
                "EXTRACTOR_FINGERPRINT {EXTRACTOR_FINGERPRINT:?} does not name locked \
                 tree-sitter-typescript {version}"
            );
        }
    }

    /// The fingerprint must also name the persisted fact schema (T22), so a
    /// change to recorded fact shape or meaning invalidates stored facts even
    /// when every grammar version is unchanged.
    #[test]
    #[cfg(any(feature = "lang-php", feature = "lang-typescript"))]
    fn fingerprint_names_the_fact_schema() {
        assert!(
            EXTRACTOR_FINGERPRINT.contains(";fact-schema="),
            "EXTRACTOR_FINGERPRINT {EXTRACTOR_FINGERPRINT:?} must name the fact schema; bump \
             the `fact-schema` integer whenever a persisted fact's shape or meaning changes"
        );
    }
}
