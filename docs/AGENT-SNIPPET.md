# Agent Snippet

This managed block is exactly what `rivet snippet` prints (including markers and a final newline). `snippet --json` wraps it in a versioned object. `init --write-snippet` installs or updates one block idempotently; text outside it is preserved. The benchmark's treatment uses exactly this text. Changes require a new recorded snippet hash. Since 2026-09-23 (SN1) the shipped block's SHA-256 is `1819530610c1418d4b0d9bce84363504d99f5029d628558185ede35a82d3b5f7`; pilot-01 used the previous block, `0d5a7d0a2195199ecdefd96a23da4ba6e4653d15830b0109f76c1d096be6119f`, which recommended `--json`.

```markdown
<!-- rivet:start -->
## Code navigation with rivet

Use `rivet` for structural lookup in supported source files:

Choose the command that answers the current question; these commands are not a required sequence. For understanding a known symbol, start directly with `context`.

- `rivet symbol <name>` locates a definition, signature, and call sites. Use it before searching a definition and reading its whole file.
- `rivet refs <name>` returns likely references with their containing symbols. `?` means name-only evidence; verify before relying on it. `--mode candidates` also includes unrelated same-name uses for auditing. Neither mode proves runtime completeness.
- `rivet context <name> --tokens 3000` returns target and related source within an estimated source-text budget. Inspect segment forms: `signature` is a summary, not the full body. Metadata and actual model-token counts are outside this budget.

Names can be short, dotted, or repository-relative `file:line`. Ambiguity returns candidate IDs; rerun with a quoted canonical ID. Lists default to 50 results; check totals and use `--offset` to page references/call lists. Use `rivet symbol` to page ambiguity candidates for a context query.

The default text output is compact and meant for you to read; `--json` emits the full machine contract at several times the size, so reserve it for scripts that parse the result. Queries refresh automatically; no routine `rivet index` call is needed. Check coverage/skipped files and resolution tiers before relying on an empty result. Use text tools for unsupported syntax/languages, comments, strings, dynamic references, or missing context. Results describe the indexed snapshot; verify live source before editing.
<!-- rivet:end -->
```

## Installation behavior

If only one of `AGENTS.md` and `CLAUDE.md` exists, use it. If neither exists, create `AGENTS.md`. If both exist, require `--snippet-file AGENTS.md|CLAUDE.md`. Existing duplicate/malformed markers are an error, not a reason to append another block. Plain shell redirection still works but can duplicate content.

An explicit `--snippet-file` takes precedence over auto-selection and creates the selected file if missing. See [INTEGRATION](INTEGRATION.md) for host-specific setup. This block is the default integration; an optional skill is a separate treatment, not silently added to the benchmark.
