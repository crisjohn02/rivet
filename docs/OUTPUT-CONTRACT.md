# Output Contract

> **Status:** proposed schema version 1, Draft v0.3. This is the authoritative wire contract; freeze it at the first tagged release. All examples below are valid JSON, with illustrative hashes.

## Transport and common rules

Every MVP command supports `--json`. Success writes exactly one UTF-8 JSON object followed by LF to stdout and no progress on stderr. Failure writes one error object followed by LF to stderr, leaves stdout empty, and uses the documented exit code. This includes argument validation whenever `--json` is present. `--help`/`--version` are text-only and cannot be combined with `--json`. Human mode uses human errors on stderr. JSON Lines is deferred.

Every object at the top level has `schema_version: 1`. Required nullable fields are present with `null`; optional fields are omitted unless requested. Consumers ignore unknown object fields. Existing enum sets, field types/meaning, coordinate conventions, and ordering are fixed within a schema version. Enum additions are breaking unless a field is explicitly documented as an open string.

Symbols and source are snapshot data. Navigation commands are byte-identical for the same snapshot, tool/grammar versions and options. `init` and `index` describe work performed and are not covered by repeated-run byte identity. `--timing` is supported only on `index` and adds `elapsed_ms`.

## Flag applicability

| Flag | Commands | Default / rule |
|---|---|---|
| `--json` | All six MVP commands | Off |
| `--freshness content\|metadata` | index, symbol, refs, context | Config, otherwise content |
| `--no-refresh` | symbol, refs, context | Off; conflicts with freshness; requires compatible cache |
| `--limit N` | symbol, refs, context | 50; context counts segments, others bound each list/candidate page |
| `--offset N` | symbol, refs | 0; context rejects it |
| `--min-resolution` | symbol, refs | refs: name_match; symbol call lists: scoped (SY1) |
| `--mode references\|candidates` | refs | references |
| `--kind` | refs | All; comma-separated supported `ref_kind` values |
| `--source`, `--signature-only` | symbol | Both off; mutually exclusive |
| `--tokens`, `--depth`, `--collapse` | context | Config, otherwise 4000 / 2 / auto |
| `--include-tests`, `--exclude-tests` | context | Config, otherwise include; mutually exclusive |
| `--include-callers`, `--exclude-callers` | context | Include; mutually exclusive |
| `--include-callees`, `--exclude-callees` | context | Include; mutually exclusive |
| `--force`, `--timing`, `--languages` | index | Off / off / configured languages |
| `--write-snippet`, `--snippet-file` | init | Off; snippet-file requires write-snippet |

Reject unknown flags and out-of-range values before filesystem work. A configured/requested language not compiled into the binary is an argument/configuration error (exit 2), never silently disabled. `--depth` accepts 1 or 2. Explicit positional query is required for symbol/refs/context. `init`, `index`, and `snippet` take no positional query.

## Coordinates and symbol objects

Paths are repository-relative UTF-8 with `/` separators and preserved case. Bytes are zero-based, half-open `[start_byte, end_byte)` offsets into original UTF-8 content. Lines and UTF-8 byte columns are one-based; end lines are inclusive. CRLF remains two bytes. Columns are not display columns or UTF-16 positions.

```json
{
  "id": "src/survey.ts#Survey.launch",
  "name": "launch",
  "qualified_name": "Survey.launch",
  "kind": "method",
  "language": "typescript",
  "file": "src/survey.ts",
  "start_byte": 40,
  "end_byte": 81,
  "start_line": 3,
  "end_line": 5,
  "content_hash": "blake3:example"
}
```

`kind`: `class`, `function`, `method`, `interface`, `struct`, `enum`, `module`, `property`, `const`. Only supported language constructs appear. `language` is an open string. Hashes in real output are `blake3:` plus 64 lowercase hex digits. Symbol IDs escape literal `%` and `#` in components and use file-order ordinals for duplicate declarations (spec §10). They survive line-only edits, not renames, moves, or duplicate reordering.

## Common index metadata

Every successful `index`, `symbol`, `refs`, or `context` response contains `index`:

```json
{
  "snapshot": "blake3:example",
  "freshness": "content",
  "coverage": {
    "complete": false,
    "files_seen": 12,
    "files_indexed": 8,
    "skipped": {
      "unsupported": 2,
      "binary": 0,
      "size": 1,
      "encoding": 0,
      "parse_error": 1,
      "resource_limit": 0
    }
  },
  "diagnostics": {
    "total": 2,
    "truncated": false,
    "items": [
      {"file": "src/big.ts", "code": "file_too_large", "detail": "exceeds max_file_size_kb"},
      {"file": "src/broken.ts", "code": "parse_error", "detail": "syntax error"}
    ]
  }
}
```

`freshness`: `content` (default, all eligible source content hashed), `metadata` (size/mtime trusted for unchanged files), or `cached` (no refresh). It describes verification, not a promise that the working tree cannot change afterward. `snapshot` identifies the indexed content/configuration; it is not a Git commit or counter.

`files_seen = files_indexed + sum(skipped)`, for regular files admitted by traversal. `complete` is true only when all skip counts are zero. It means scan-domain coverage, not semantic completeness; ignored files, symlinks, nested repositories and unsupported constructs inside parsed files are outside that guarantee. Empty references never proves absence of dynamic runtime uses. Non-UTF-8 paths are excluded with a diagnostic using escaped bytes and force `complete: false`.

Diagnostics exclude ordinary `unsupported` files (counted above) but include other skipped files and unsupported paths. Sort by `(file bytes, code, start_byte or 0, detail bytes)` and cap at 50 independently of `--limit`. Counts are exhaustive; `truncated` indicates omitted diagnostic items. `code` is an open string. Optional `start_byte` locates a source diagnostic.

Human output ends with one coverage line (CV1) whenever `complete` is false, any `skipped` count is non-zero, or `diagnostics.total` is non-zero; a complete snapshot with no diagnostics prints none. It is the last line of `index`, `symbol`, `refs` and `context` output (after a blank line, except in `context`) and of an index-dependent error on stderr. A `cached` snapshot's `snapshot: cached (--no-refresh); it may not match the working tree` line comes directly before it. The grammar is exactly `coverage <state>: <files_indexed>/<files_seen> files indexed[; skipped <count> <key>[, <count> <key>]...]; <diagnostics>`:

- `<state>` is `complete` or `incomplete`, from `complete`.
- The `skipped` clause lists only the non-zero counts, each as `<count> <key>`, in the key order of `skipped` (`unsupported`, `binary`, `size`, `encoding`, `parse_error`, `resource_limit`); it is omitted when every count is 0.
- `<diagnostics>` depends on `total`:
  - 0: `0 diagnostics`.
  - 1 or 2: `1 diagnostic: <file> (<code>)`, or `2 diagnostics: <file> (<code>), <file> (<code>)` in the sort order above. `<file>` is the item's `file` exactly: repository-relative, with a non-UTF-8 path's escaped bytes kept escaped.
  - More than 2: `<total> diagnostics (<count> <code>[, <count> <code>]...)`, naming no path. The counts are of the listed `items`, by descending count and then code bytes. When `truncated` is true, they are prefixed `first <M>: `, where M is the number of items listed.
- Ordinary `unsupported` files have no diagnostic, so they are counted and never named.

The line never points to `--json`. For example:

- `coverage incomplete: 9/10 files indexed; skipped 1 unsupported; 0 diagnostics`
- `coverage incomplete: 1219/2815 files indexed; skipped 1595 unsupported, 1 parse_error; 1 diagnostic: app/X.php (parse_error)`
- `coverage incomplete: 9/13 files indexed; skipped 1 unsupported, 1 binary, 2 parse_error; 3 diagnostics (2 parse_error, 1 binary_file)`
- `coverage incomplete: 250/691 files indexed; skipped 441 unsupported; 50 diagnostics (50 unsupported_language)`
- `coverage incomplete: 10/70 files indexed; skipped 60 parse_error; 60 diagnostics (first 50: 50 parse_error)`

## Pagination and resolution

`--limit` is 1–1,000 (default 50). `--offset` is a nonnegative integer (default 0). Apply filters, then sort, count, and slice. `total` counts all matches after filters; `truncated` means some matches are outside the page, including earlier pages. `next_offset` is the next offset if later results exist, otherwise null. An offset beyond the end gives an empty page. `by_resolution` counts all filtered matches before pagination.

Resolution values are `exact`, `scoped`, `name_match`; they describe evidence for declaration links, not runtime dispatch. A minimum of `scoped` retains exact and scoped. A candidate bound to another declaration has `resolution: name_match` relative to the query and preserves its actual `resolved_target`.

## `rivet refs <query> --json`

```json
{
  "schema_version": 1,
  "index": {},
  "symbol": {},
  "mode": "references",
  "total": 1,
  "truncated": false,
  "next_offset": null,
  "by_resolution": {"exact": 0, "scoped": 1, "name_match": 0},
  "by_exclusion": {"incompatible_form": 0, "unrelated_receiver": 0},
  "references": [
    {
      "file": "src/main.ts",
      "content_hash": "blake3:example",
      "start_byte": 120,
      "end_byte": 126,
      "line": 8,
      "column": 10,
      "containing_symbol": null,
      "ref_kind": "call",
      "resolution": "scoped",
      "resolved_target": "src/survey.ts#Survey.launch",
      "receiver": "survey"
    }
  ]
}
```

Here and in subsequent shape examples, `{}` at `index` or `symbol` means the complete common object defined above, not an empty object allowed in actual output. `containing_symbol` is a full symbol object or null for top-level uses; uses inside anonymous functions inherit the nearest named container. `receiver` is source text or null. `resolved_target` is a canonical ID or null. `ref_kind`: `call`, `type`, `import`, `assignment`, `read`, `write`, `unknown`; v0.1 may use `unknown` where finer classification is unsupported.

Default `mode` is `references`; `--mode candidates` also includes extracted same-name uses bound elsewhere. Both include alias uses bound to the queried declaration. Declarations and literal text are excluded. Dedupe by `(file, start_byte, end_byte, ref_kind)`; a call is never repeated as `unknown`.

`by_exclusion` (additive, LR2) is always present, with keys in the order shown. It counts the same-name unresolved uses reference mode left out by evidence (spec §11.5): `incompatible_form` when the use form cannot name the target's kind, `unrelated_receiver` when no object could be an instance of both the use's determined receiver class and the target's class. A use both kinds of evidence exclude counts once, as `incompatible_form`. Counts cover uses that pass the kind and resolution filters, are computed before pagination, and do not depend on `--limit`/`--offset`; excluded uses are not in `total` or `by_resolution`. Both are always 0 in `--mode candidates`, which excludes nothing by evidence and lists every same-name use, including those reference mode rules out. Human output adds one line directly after the count header when their sum N is non-zero (EX1): `N same-name use ruled out (<reasons>)` when N is 1, `N same-name uses ruled out (<reasons>)` otherwise. `<reasons>` lists only the non-zero counts, in this order and separated by `; `: `unrelated receiver class: <unrelated_receiver>`, then `form cannot reference <a|an> <kind>: <incompatible_form>`, where `<kind>` is the target's `kind` as the header prints it and the article is `an` when it starts with a vowel. For example, `3 same-name uses ruled out (unrelated receiver class: 2; form cannot reference a method: 1)`. `symbol.called_by` and `context` callers apply the same exclusion without reporting counts.

## `rivet symbol <query> --json`

```json
{
  "schema_version": 1,
  "index": {},
  "symbol": {},
  "signature": "launch(): void",
  "doc_comment": null,
  "parent": "src/survey.ts#Survey",
  "calls": {"total": 0, "truncated": false, "next_offset": null, "hidden_name_match": 0, "items": []},
  "called_by": {"total": 0, "truncated": false, "next_offset": null, "hidden_name_match": 0, "items": []}
}
```

`signature`, `doc_comment`, and `parent` are required nullable fields. `--source` adds `source`, a string from stored bytes. `--signature-only` omits `calls` and `called_by` and conflicts with `--source`. Otherwise both call lists are present, independently paginated with the supplied limit/offset; each item is a reference object as above with `ref_kind: call`. `calls` selects use sites contained by the target; `called_by` applies default reference-mode matching to call sites targeting the symbol. Count call sites, not unique functions. `--min-resolution` is supported on both lists. Before relationship extraction is implemented, milestone builds must not claim final v0.1 support.

Both call lists default to `--min-resolution scoped` (SY1), in text and `--json` alike: they list `exact` and `scoped` rows and leave out `name_match` rows. `--min-resolution name_match` lists every row, as before SY1; an explicit value always wins. `refs` and `context` keep `name_match` as their default. `total`, `truncated`, `next_offset`, and `items` describe the rows at or above the minimum tier.

`hidden_name_match` (additive, SY1) is always present in each list, after `next_offset` and before `items`. It is the integer number of `name_match` rows the tier filter removed from that list, that is, exactly the rows `--min-resolution name_match` would add under the same other options. It counts `name_match` rows only: a `scoped` row that an explicit `--min-resolution exact` removes is in no count. It is 0 under `--min-resolution name_match`. It is computed before pagination, is not part of `total`, and does not depend on `--limit`/`--offset`. Uses reference mode excludes by evidence (LR2) are listed under no tier and are not counted. Like every count, it is a function of the snapshot and options, so it is byte-identical across repeated queries and rebuilds of the same snapshot.

Human output adds the count to a list's heading line when it is non-zero: `calls: (+N name-only not listed)` and `called by: (+N name-only not listed)`, whether or not any row follows, with no command or flag pointer. A list with no rows at or above the minimum and N > 0 prints only that heading, never `none`; `calls: none` and `called by: none` mean no row is listed and no `name_match` row is hidden (under an explicit `exact`, uncounted `scoped` rows may still exist). In human `calls` rows, an unresolved call's receiver is shown with every whitespace run (line breaks and tabs included) collapsed to one space, so a receiver spanning several source lines stays on its row; JSON `receiver` keeps the exact source text.

## `rivet context <query> --tokens N --json`

```json
{
  "schema_version": 1,
  "index": {},
  "symbol": {},
  "budget_tokens": 3000,
  "estimated_tokens": 6,
  "tokenizer": "utf8-bytes-v1",
  "budget_scope": "source",
  "segments": [
    {
      "symbol": {},
      "form": "full",
      "reason": "target",
      "resolution": "exact",
      "estimated_tokens": 6,
      "source": "launch(): void {}"
    }
  ],
  "omitted": {"budget": 0, "overlap": 0, "limit": 0},
  "candidate_limit_reached": false
}
```

`N` is a positive integer up to 1,000,000; default 4,000. Estimate each final source string as `ceil(UTF8_bytes / 3)` and sum. Successful `estimated_tokens <= budget_tokens`; this excludes headers/metadata and is not an actual model-token limit. No partial-body truncation and no success that exceeds the estimate. If no allowed target form fits, error 8 reports `required_tokens` (the minimum estimate of its allowed forms).

`form`: `full` or `signature`. A signature is a derived summary, not a contiguous source slice; its symbol coordinates describe the original definition. `reason`: `target`, `type`, `callee`, `caller`, `import`, `test`, `parent`, `second_degree`. Target resolution is `exact` for its selected identity; other segment resolutions describe the link/path used to select them. Omission counts cover only explored unique candidates, assigned once in order: overlap, limit, then budget. `candidate_limit_reached` signals that additional candidates may exist beyond the fixed traversal caps. Context supports `--limit` for segment count, rejects `--offset`, and uses rank order. See spec §16 for ranking and collapse rules.

## Administrative commands

`init --json`:

```json
{
  "schema_version": 1,
  "created": [".rivet/", ".rivet/config.toml"],
  "modified": [".gitignore"],
  "snippet_file": null
}
```

Paths are root-relative, sorted by UTF-8 bytes. Existing configuration is preserved. Repeated init can return empty lists. `snippet_file` is the destination or null. The managed block rules are in spec §25.

Explicit `--snippet-file` selects that file even when the other instruction file exists; create it if missing. It requires `--write-snippet`. Auto-selection is used only when no explicit destination was supplied.

`index --json`:

```json
{
  "schema_version": 1,
  "index": {},
  "symbols": 100,
  "uses": 250,
  "bindings": 170,
  "updated": 2,
  "unchanged": 8,
  "deleted": 1
}
```

Counts are after commit. `updated` counts current file rows with changed source/status/language or regenerated facts (all current rows for `--force`); `unchanged` counts the remaining current file rows. Metadata-only updates with equal content are unchanged. `deleted` counts formerly present rows now deleted/excluded. `updated + unchanged = index.coverage.files_seen`. `--timing` adds `elapsed_ms`; `index` rejects `--no-refresh`.

`snippet` without JSON prints exactly the managed block from [AGENT-SNIPPET](AGENT-SNIPPET.md), ending in LF. JSON wraps those bytes:

```json
{"schema_version": 1, "snippet": "<!-- rivet:start -->\n...\n<!-- rivet:end -->\n"}
```

## Errors

```json
{
  "schema_version": 1,
  "error": "ambiguous_symbol",
  "message": "query 'launch' matched 2 symbols",
  "hint": "Re-run with a returned canonical ID.",
  "total": 2,
  "truncated": true,
  "next_offset": 1,
  "candidates": [{}]
}
```

| Error | Exit | Required additional fields |
|---|---|---|
| `general` | 1 | none |
| `invalid_arguments` | 2 | none |
| `repository_unavailable` | 3 | none; includes lock/I/O/incompatible-cache failures |
| `symbol_not_found` | 4 | `suggestions` (at most 5 strings) |
| `ambiguous_symbol` | 5 | `total`, `truncated`, `next_offset`, `candidates` (symbol objects) |
| `parse_failure` | 6 | `file`, `detail` |
| `unsupported_language` | 7 | `file`, nullable `language` |
| `budget_too_small` | 8 | `budget_tokens`, `required_tokens` |
| `repository_changed` | 9 | none |

All errors have `schema_version`, `error`, `message`, and `hint`. Index-dependent errors also include `index` if a compatible snapshot was successfully acquired. Directly addressing an unsupported file returns exit 7; parse/resource-limit files return exit 6; known binary/oversize/encoding exclusions return exit 3 with the reason and a corrective hint. A missing name elsewhere is exit 4 even when coverage is partial. Invalid arguments take precedence over filesystem work. Context ambiguity candidates use offset zero because context rejects `--offset`; the hint may direct the caller to `symbol` for further pages.

## Ordering and versioning

| List | Ascending key |
|---|---|
| References and call sites | `(file bytes, start_byte, end_byte, ref_kind, resolved_target or empty)` |
| Symbol candidates | `(file bytes, start_byte, id)` |
| Suggestions | `(Unicode-scalar Levenshtein distance to query, name bytes)`; unique qualified names |
| Context segments | Inclusion order from spec §16 |
| Diagnostics | As defined above |
| Init paths | UTF-8 byte order |

Object keys follow the order defined here; empty lists remain lists. JSON escapes control characters, preserves other valid Unicode as UTF-8, uses integer counts, and uses no insignificant whitespace in actual output. Pretty examples above are for reading.

After the initial freeze, removing/renaming fields, changing types/enums/semantics/order, and changing the estimator's interpretation require a schema version increment. Adding optional object fields or new commands is additive. Snapshot changes caused by better extraction on changed fixture data are behavior changes, not automatically schema changes; classify them explicitly. No schema-version negotiation or support for old versions is promised in v0.1.
