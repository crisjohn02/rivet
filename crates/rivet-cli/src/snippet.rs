//! `rivet snippet` (spec §25; OUTPUT-CONTRACT "Administrative commands";
//! docs/AGENT-SNIPPET.md).
//!
//! The managed instruction block is embedded here as a literal, from
//! `<!-- rivet:start -->` through `<!-- rivet:end -->` plus one final LF. It is
//! the benchmark's treatment text and carries a recorded hash, so it is kept
//! byte-identical to the fenced block in `docs/AGENT-SNIPPET.md`; the
//! `snippet_json` integration test extracts that block from the document
//! itself and compares it with this constant and with `rivet snippet` output,
//! so the two cannot drift. The command needs no repository and touches no
//! file.

use serde_json::{Value, json};

/// The first line of the managed block.
pub const START_MARKER: &str = "<!-- rivet:start -->";

/// The last line of the managed block.
pub const END_MARKER: &str = "<!-- rivet:end -->";

/// The exact managed block `rivet snippet` prints, ending in one LF.
pub const SNIPPET: &str = r#"<!-- rivet:start -->
## Code navigation with rivet

Use `rivet` for structural lookup in supported source files:

Choose the command that answers the current question; these commands are not a required sequence. For understanding a known symbol, start directly with `context`.

- `rivet symbol <name>` locates a definition, signature, and call sites. Use it before searching a definition and reading its whole file.
- `rivet refs <name>` returns likely references with their containing symbols. `?` means name-only evidence; verify before relying on it. `--mode candidates` also includes unrelated same-name uses for auditing. Neither mode proves runtime completeness.
- `rivet context <name> --tokens 3000` returns target and related source within an estimated source-text budget. Inspect segment forms: `signature` is a summary, not the full body. Metadata and actual model-token counts are outside this budget.

Names can be short, dotted, or repository-relative `file:line`. Ambiguity returns candidate IDs; rerun with a quoted canonical ID. Lists default to 50 results; check totals and use `--offset` to page references/call lists. Use `rivet symbol` to page ambiguity candidates for a context query.

Add `--json` for structured results. Queries refresh automatically; no routine `rivet index` call is needed. Check coverage/skipped files and resolution tiers before relying on an empty result. Use text tools for unsupported syntax/languages, comments, strings, dynamic references, or missing context. Results describe the indexed snapshot; verify live source before editing.
<!-- rivet:end -->
"#;

/// The `snippet --json` success object: `schema_version`, then `snippet`.
pub fn success_json() -> Value {
    json!({
        "schema_version": 1,
        "snippet": SNIPPET,
    })
}

#[cfg(test)]
mod tests {
    use super::{END_MARKER, SNIPPET, START_MARKER};

    #[test]
    fn block_is_framed_by_the_markers_and_one_final_lf() {
        assert!(SNIPPET.starts_with(&format!("{START_MARKER}\n")));
        assert!(SNIPPET.ends_with(&format!("\n{END_MARKER}\n")));
        assert!(!SNIPPET.ends_with("\n\n"));
        assert!(!SNIPPET.contains('\r'));
        assert_eq!(SNIPPET.matches(START_MARKER).count(), 1);
        assert_eq!(SNIPPET.matches(END_MARKER).count(), 1);
    }
}
