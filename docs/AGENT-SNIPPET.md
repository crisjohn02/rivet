# Agent Snippet

This managed block is exactly what `rivet snippet` prints (including markers and a final newline). `snippet --json` wraps it in a versioned object. `init --write-snippet` installs or updates one block idempotently; text outside it is preserved. The benchmark's treatment uses exactly this text. Changes require a new recorded snippet hash. Hash history of the shipped block (SHA-256):

- Since 2026-09-24 (SN2): `908710a5ba23e4ffc3a4fe08513ceb01a9f5b1ed02227b97d0529f7ee38e5898`, the lean block below (624 bytes).
- 2026-09-23 to SN2 (SN1): `1819530610c1418d4b0d9bce84363504d99f5029d628558185ede35a82d3b5f7`, which recommended the text output (1,745 bytes); pilot-02 to pilot-04 and heldout-01 used it.
- Before SN1: `0d5a7d0a2195199ecdefd96a23da4ba6e4653d15830b0109f76c1d096be6119f`, which recommended `--json` (1,603 bytes); pilot-01 used it.

```markdown
<!-- rivet:start -->
## rivet (code navigation)

- `rivet symbol <name>`: definition, signature, calls and callers.
- `rivet refs <name>`: references with their containing symbols.
- `rivet context <name> --tokens 1500`: the symbol plus related source; a `signature` segment is not the full body.

Names may be short, `Class.method`, a canonical ID, or `file:line`. Results refresh automatically. `?` marks a name-only match: verify it. rivet does not see comments, strings, dynamic calls, framework wiring or unsupported files, so an empty or short result is not proof of absence; use text search there.
<!-- rivet:end -->
```

## Installation behavior

If only one of `AGENTS.md` and `CLAUDE.md` exists, use it. If neither exists, create `AGENTS.md`. If both exist, require `--snippet-file AGENTS.md|CLAUDE.md`. Existing duplicate/malformed markers are an error, not a reason to append another block. Plain shell redirection still works but can duplicate content.

An explicit `--snippet-file` takes precedence over auto-selection and creates the selected file if missing. See [INTEGRATION](INTEGRATION.md) for host-specific setup. This block is the default integration; an optional skill is a separate treatment, not silently added to the benchmark.
