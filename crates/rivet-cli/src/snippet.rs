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
## rivet (code navigation)

- `rivet symbol <name>`: definition, signature, calls and callers.
- `rivet refs <name>`: references with their containing symbols.
- `rivet context <name> --tokens 1500`: the symbol plus related source; a `signature` segment is not the full body.

Names may be short, `Class.method`, a canonical ID, or `file:line`. Results refresh automatically. `?` marks a name-only match: verify it. rivet does not see comments, strings, dynamic calls, framework wiring or unsupported files, so an empty or short result is not proof of absence; use text search there.
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
