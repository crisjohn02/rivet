//! Language extraction and receiver hints.
//!
//! T02 exposes the grammar surface: language identifiers, the Tree-sitter
//! grammars for the enabled features, and the extension dispatch that decides
//! which files a build can handle. T41 adds [`LanguageId::has_extractor`], the
//! one switch that says whether a language's files can be indexed. The PHP
//! adapter lives in [`php`]. The TypeScript/TSX adapter lives in `typescript`
//! (T42 definitions, T43 uses, scopes, and imports); T43 turns its switch on.

use std::path::Path;

#[cfg(feature = "lang-php")]
pub mod php;

#[cfg(feature = "lang-typescript")]
pub mod typescript;

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

    /// Whether this language has an extraction adapter in this build.
    ///
    /// This is the single switch that decides whether a file in an enabled
    /// language can be indexed. A language without an adapter yields no facts
    /// even when its file parses, so refresh counts such a file as
    /// `unsupported` with an `unsupported_language` diagnostic instead of
    /// claiming coverage rivet does not have (AF5, audit finding 14). The
    /// CLI's refresh and cached-coverage paths consult this method directly;
    /// nothing else decides it.
    ///
    /// TypeScript and TSX parse with their grammars (T41), T42 adds their
    /// definition extractor, and T43 adds uses, scopes, and imports and turns
    /// their arm on, together with their dispatch arm in `rivet_parser`.
    /// `rivet_parser`'s tests fail if the two disagree. Every compiled
    /// language now has an adapter; the switch stays the one place a future
    /// language without one is kept unindexed.
    pub const fn has_extractor(self) -> bool {
        match self {
            #[cfg(feature = "lang-php")]
            LanguageId::Php => true,
            #[cfg(feature = "lang-typescript")]
            LanguageId::Typescript | LanguageId::Tsx => true,
        }
    }
}

/// Deterministic per-file parser resource bounds (spec §27).
///
/// Both limits are *counts*, never wall-clock time, so the same bytes always
/// produce the same result: "Deterministic limits avoid wall-clock-dependent
/// query results." A file that exceeds either bound is recorded as
/// `resource_limit`, contributes no symbols, uses, or scopes, and gets one
/// diagnostic. A count equal to the bound is still within it.
///
/// Production code always uses [`ResourceLimits::DEFAULT`]. The fields are
/// public so tests (and the CLI's debug-only test hook) can lower them instead
/// of building million-node files. Changing a default is a change to which
/// files carry facts, so it must bump `fact-schema` in
/// [`EXTRACTOR_FINGERPRINT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    /// The most Tree-sitter nodes (named and anonymous) one file's tree may
    /// contain, counted by a pre-order visit before any extraction.
    pub max_visited_nodes: u64,
    /// The most uses one file may yield.
    pub max_extracted_uses: u64,
}

/// The spec's initial node bound: one million visited syntax nodes per file.
pub const MAX_VISITED_NODES: u64 = 1_000_000;

/// The spec's initial use bound: 500,000 extracted uses per file.
pub const MAX_EXTRACTED_USES: u64 = 500_000;

impl ResourceLimits {
    /// The spec §27 bounds.
    pub const DEFAULT: ResourceLimits = ResourceLimits {
        max_visited_nodes: MAX_VISITED_NODES,
        max_extracted_uses: MAX_EXTRACTED_USES,
    };
}

impl Default for ResourceLimits {
    fn default() -> ResourceLimits {
        ResourceLimits::DEFAULT
    }
}

/// Returns the language whose grammar handles `rel_path`, or `None` when the
/// extension maps to no compiled language.
///
/// `rel_path` is a repository-relative `/`-separated path. Only `.php` (PHP),
/// `.ts` (the TypeScript grammar, which includes `.d.ts` declaration files),
/// and `.tsx` (the TSX grammar) are registered in v0.1. JavaScript extensions
/// and the module-kind TypeScript extensions `.mts`, `.cts`, `.d.mts`, and
/// `.d.cts` are deliberately not inferred, although the TypeScript grammar
/// could parse the latter (docs/ADDING-A-LANGUAGE.md).
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
/// not enough: a change to what the adapter records invalidates stored facts
/// without touching a grammar, and a reused index would deserialize stale facts
/// through serde defaults. Any change to a persisted fact's shape or to the
/// meaning of an existing field must bump this integer so the next refresh
/// reparses every enabled-language file.
///
/// History, newest first:
///
/// - **14** — T45: TypeScript receiver facts. `UseHint::This` gained
///   `is_static` (whether the `this` is in a static member, static block, or
///   static field initializer), `UseHint::Typed` and `UseHint::NewExpr`
///   gained `name_span` (the span of the `type` use of the annotation's type
///   name or the `new` target, so the resolver looks the class up in the
///   scope where it is written), and a TypeScript file's module scope gained
///   `member_sides` (whether each class and interface member symbol is
///   static). PHP records none of the new fields, and each is omitted from
///   the fact JSON when absent, so PHP facts are byte-identical.
/// - **13** — T44: TypeScript scopes gained `module_exports` (what a module
///   exports from its own declarations: `export` on a declaration, `export
///   default` of a named declaration or an identifier, local `export { a as
///   b }` specifiers, and anonymous or expression defaults with no local name)
///   and `value_type_uses` (the `type` uses that name values: `new` targets,
///   `instanceof` and `typeof` operands, class `extends` expressions) and
///   `ambient_module` (a string-named ambient module's body); and a
///   TypeScript scope's `declares` no longer lists global declarations (those
///   of a script file, which has no top-level `import` or `export`, and those
///   inside `declare global`). PHP facts are unchanged.
/// - **12** — T43: TypeScript and TSX files are indexed. Their facts are new
///   (symbols with case-sensitive lookup names, uses, scopes with
///   `locals` and `module_imports`, and `UseHint::Typed` with the new
///   `TypedOrigin::Variable`), and a cache written before T43 recorded every
///   `.ts`/`.tsx` file as `unsupported` with no content hash. Metadata-mode
///   refresh would otherwise reuse that stored status for an unchanged file,
///   so the bump makes every enabled-language file reparse.
/// - **11** — LR2: `ScopeFacts` gained `anonymous_supertypes`, the names in
///   each anonymous class's `extends`/`implements` clauses.
/// - **10** — T36d: the names in a class's `extends`/`implements` clauses, an
///   enum's `implements` clause, and an interface's `extends` list are recorded
///   as `type` uses (named and anonymous classes alike); an expression scope
///   before `::` in a class-constant or static property access is walked as
///   code; and `ScopeFacts` gained `supertypes`, each named class-like's
///   declared supertypes.
/// - **9** — AF4: a use's stored `lookup_name` is its normalized short name
///   (last qualified segment, no leading `$`), and `unknown` uses and `use
///   const` aliases keep their case; anonymous class bodies are walked, with
///   no `$this`/`self`/`static` receiver hint inside them; the class before
///   `::` in a static call, class-constant access, static property access, or
///   `::class` is recorded as a `type` use and its member carries the new
///   `UseHint::NamedClass`; `::class` records no member use; an `instanceof`
///   class operand is a `type` use rather than `unknown`; and an enum case's
///   own name is no longer recorded as a use.
/// - **8** — AF3: `UseHint::Typed` gained `origin` (parameter or property), and
///   a union, intersection, or DNF type or a by-reference parameter no longer
///   records a typed receiver; a reference taken to a variable (`$y = &$x`, a
///   by-reference `foreach`) is recorded as a rebinding of it; constructor
///   arguments are walked and recorded as `constructor` call arguments;
///   `NewBinding` gained `value_end`; and `ScopeFacts` gained `global_scope`,
///   `call_sites`, `goto_present`, `global_names`, `dynamic_global_write`, and
///   `parameter_lists`, the tree-read by-reference parameter flags that replace
///   re-parsing signature text.
/// - **7** — AF2: stored PHP `lookup_name`s for symbols and uses fold case by
///   ASCII only, as PHP does, instead of by Unicode, and `ScopeFacts` gained
///   `class_constant_accesses`, so a class-constant read is told apart from an
///   instance property read.
/// - **6** — AF1: each namespace block of a PHP file gets its own top-level
///   scope (`ns{block}:file`) instead of sharing `top:file`, top-level closures
///   chain to it, a global `namespace { }` block's declarations lost their
///   inherited namespace prefix, and `ScopeFacts` gained
///   `namespace_unattributed`.
/// - **5** — the merge of T21b into T25a. T25/T25a had reached 4 while T21b
///   independently reached 3, so neither value described the union and the
///   merge took a new integer.
/// - **T21b** — `ScopeFacts` gained `call_args` and `unanalysable`, so a scope
///   holding a by-reference argument or an unanalysable construct records no
///   `new`-receiver binding.
/// - **T25a** — stored signature text changed: one line per declared name, and
///   no body marker on a bodyless declaration.
/// - **T25** — enum cases and promoted constructor properties became
///   addressable symbols, so an older index is missing declarations.
/// - **T21a** — `new_bindings` widened to every recognized rebinding form.
/// - **T21** — `new_bindings` gained `direct_new` and `block`, and
///   `UseHint::NewExpr` gained `use_block`.
/// - **T22** — introduced this component.
#[cfg(all(feature = "lang-php", feature = "lang-typescript"))]
pub const EXTRACTOR_FINGERPRINT: &str = "php=0.24.2;ts=0.23.2;fact-schema=14";

#[cfg(all(feature = "lang-php", not(feature = "lang-typescript")))]
pub const EXTRACTOR_FINGERPRINT: &str = "php=0.24.2;fact-schema=14";

#[cfg(all(not(feature = "lang-php"), feature = "lang-typescript"))]
pub const EXTRACTOR_FINGERPRINT: &str = "ts=0.23.2;fact-schema=14";

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

/// The collapsed signature form of `symbol` (spec §16.2), dispatched by
/// language as [`grammar`] is.
///
/// `members` are the symbol's direct members in any order; each language
/// orders them itself (for PHP, [`php::order_members`]) because only the
/// adapter knows how several members can share one declaration. `source` is
/// the full text of the file that declares `symbol`, as stored in the
/// snapshot. No parse tree is needed: the form is rendered from stored spans,
/// signatures, and source text.
///
/// The result is the adapter's summary alone. It does not include the doc
/// comment; a caller that needs the full §16.2 signature form prepends it.
///
/// Returns `None` when the language has no collapsed-form renderer, so a
/// caller can treat the signature form as unavailable rather than emit an
/// empty one. TypeScript has none yet: T42 records each definition's own
/// signature, not the container form, so a TypeScript `context` segment has
/// no signature form (its full form is still available).
pub fn signature_summary(
    language: LanguageId,
    symbol: &rivet_core::ExtractedSymbol,
    members: &[rivet_core::ExtractedSymbol],
    source: &str,
) -> Option<String> {
    #[cfg(not(any(feature = "lang-php", feature = "lang-typescript")))]
    let _ = (symbol, members, source);
    match language {
        #[cfg(feature = "lang-php")]
        LanguageId::Php => {
            let mut ordered = members.to_vec();
            php::order_members(&mut ordered, source);
            Some(php::signature_summary(symbol, &ordered, source))
        }
        #[cfg(feature = "lang-typescript")]
        LanguageId::Typescript | LanguageId::Tsx => {
            let _ = (symbol, members, source);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    // A build with no grammar has an empty fingerprint and nothing to check,
    // so the fingerprint tests and their helper need at least one grammar
    // (and would be unused, failing `-D warnings`, without one).
    #[cfg(any(feature = "lang-php", feature = "lang-typescript"))]
    use super::EXTRACTOR_FINGERPRINT;

    /// The locked `version` of `package`, read from the workspace `Cargo.lock`.
    #[cfg(any(feature = "lang-php", feature = "lang-typescript"))]
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
    #[cfg(any(feature = "lang-php", feature = "lang-typescript"))]
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

    /// The dispatch renders PHP through the PHP adapter, orders members
    /// itself, and does not add the doc comment.
    #[test]
    #[cfg(feature = "lang-php")]
    fn signature_summary_dispatches_php_and_orders_members() {
        use rivet_core::ExtractedFile;

        let source = "<?php\n/**\n * Doc.\n */\nfinal class K\n{\n    public int $b, $a;\n    public function go(): void\n    {\n    }\n}\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::grammar(super::LanguageId::Php))
            .expect("grammar");
        let tree = parser.parse(source, None).expect("tree");
        let extracted: ExtractedFile = super::php::extract(source.as_bytes(), &tree);
        let class = extracted.symbols[0].clone();
        assert_eq!(class.doc_comment.as_deref(), Some("/**\n * Doc.\n */"));
        let mut members: Vec<_> = extracted.symbols[1..].to_vec();
        members.reverse();
        let summary = super::signature_summary(super::LanguageId::Php, &class, &members, source)
            .expect("PHP renders a summary");
        assert_eq!(
            summary,
            "final class K\n{\n    public int $b;\n    public int $a;\n    public function go(): void { … }\n}"
        );
    }

    /// `.ts` (including `.d.ts`) and `.tsx` dispatch to their grammars; every
    /// other JavaScript or TypeScript-family extension stays unsupported, even
    /// `.mts`/`.cts`, which the TypeScript grammar could parse (T41).
    #[test]
    #[cfg(feature = "lang-typescript")]
    fn typescript_dispatch_claims_only_ts_dts_and_tsx() {
        use super::{LanguageId, language_for_path};

        assert_eq!(language_for_path("src/a.ts"), Some(LanguageId::Typescript));
        assert_eq!(
            language_for_path("src/types.d.ts"),
            Some(LanguageId::Typescript)
        );
        assert_eq!(language_for_path("src/App.tsx"), Some(LanguageId::Tsx));

        const UNSUPPORTED: [&str; 8] = [
            ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".d.mts", ".d.cts",
        ];
        for extension in UNSUPPORTED {
            let path = format!("src/a{extension}");
            assert_eq!(language_for_path(&path), None, "{path}");
        }
    }

    /// Every compiled language is indexed: PHP, and TypeScript and TSX from
    /// T43.
    #[test]
    fn every_compiled_language_has_an_extractor() {
        #[cfg(feature = "lang-php")]
        assert!(super::LanguageId::Php.has_extractor());
        #[cfg(feature = "lang-typescript")]
        {
            assert!(super::LanguageId::Typescript.has_extractor());
            assert!(super::LanguageId::Tsx.has_extractor());
        }
    }

    /// A language without a renderer reports no summary rather than an empty
    /// one.
    #[test]
    #[cfg(feature = "lang-typescript")]
    fn signature_summary_is_none_without_a_renderer() {
        let symbol = rivet_core::ExtractedSymbol {
            qualified_name: "f".to_string(),
            name: "f".to_string(),
            kind: rivet_core::SymbolKind::Function,
            span: rivet_core::Span::new(0, 1).expect("span"),
            name_span: None,
            parent_index: None,
            signature: Some("function f()".to_string()),
            doc_comment: None,
        };
        assert_eq!(
            super::signature_summary(super::LanguageId::Typescript, &symbol, &[], "f"),
            None
        );
    }
}
