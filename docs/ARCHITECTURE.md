# Architecture

> **Status:** pre-implementation, Draft v0.3. This document owns storage, refresh, and query execution. [OUTPUT-CONTRACT](OUTPUT-CONTRACT.md) owns serialization; the [spec](../rivet-agent-native-codebase-cli-spec.md) owns product scope.

## Workspace boundaries

| Crate | Responsibility | Workspace dependencies |
|---|---|---|
| `rivet-cli` | Arguments, root discovery, scan/refresh orchestration, output, exit codes | core, parser, index, store |
| `rivet-core` | Owned extraction records, symbols, uses, imports, scopes, spans, errors | none |
| `rivet-parser` | Tree-sitter driver, grammar dispatch, source validation | core, languages |
| `rivet-languages` | Language extraction and receiver hints | core |
| `rivet-index` | Declaration resolution, query matching, context ranking and fitting | core, store |
| `rivet-store` | SQLite, atomic publication, snapshot reads | core |

These are boundaries, not six independently published products. The initial CLI package is tentatively `rivet-cli` with binary name `rivet`; registry availability is unverified. Defer publication until naming is settled. Extraction returns owned facts without Tree-sitter node lifetimes or database IDs. Language modules never query SQLite. The CLI orchestrates scanning so the store does not acquire an implicit parser dependency.

## Query execution

1. Discover the nearest root boundary (`.rivet/` or `.git` file/directory). Respect worktrees and nested repositories.
2. Load and validate config; determine the effective extraction/resolution fingerprint.
3. Open the local store, acquire its writer transaction when refreshing, and reconcile the complete eligible file set.
4. Parse changed bytes, persist raw facts, resolve declaration links, and publish one complete snapshot.
5. Start a read transaction and resolve the query against that snapshot: zero matches → exit 4; multiple matches → exit 5 with paginated candidates.
6. Fetch references or build context from the same snapshot and stored source bytes.
7. Filter, sort, count, paginate, and serialize. Libraries never print.

Queries may create a cache at a Git root but never modify `.gitignore` or instruction files. `init` owns those explicit setup changes. `snippet` needs no repository or index.

## Minimal logical schema

This SQL fixes storage invariants, not an already implemented migration. JSON schema versions and database format versions are separate.

```sql
PRAGMA foreign_keys = ON;

CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
); -- index_format_version, extractor_fingerprint, resolver_fingerprint,
   -- effective_config_fingerprint, snapshot_digest

CREATE TABLE files (
  path TEXT PRIMARY KEY,
  language TEXT,
  mtime_ns INTEGER NOT NULL,
  size INTEGER NOT NULL,
  content_hash TEXT,
  source BLOB,
  parse_status TEXT NOT NULL
); -- ok | parse_error | resource_limit | binary | size | encoding | unsupported
   -- source holds exactly the bytes parsed for ok files; NULL for skipped files

CREATE TABLE symbols (
  id TEXT PRIMARY KEY,
  file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  name TEXT NOT NULL,
  lookup_name TEXT NOT NULL,
  qualified_name TEXT NOT NULL,
  kind TEXT NOT NULL,
  parent_id TEXT REFERENCES symbols(id) ON DELETE SET NULL
    DEFERRABLE INITIALLY DEFERRED,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  signature TEXT,
  doc_comment TEXT,
  CHECK (0 <= start_byte AND start_byte < end_byte)
);
CREATE INDEX symbols_lookup ON symbols(lookup_name);
CREATE INDEX symbols_qname ON symbols(qualified_name);
CREATE INDEX symbols_file ON symbols(file, start_byte);

CREATE TABLE uses (
  use_id INTEGER PRIMARY KEY,
  file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  containing_symbol TEXT REFERENCES symbols(id) ON DELETE SET NULL,
  scope_key TEXT NOT NULL,
  spelling TEXT NOT NULL,
  lookup_name TEXT NOT NULL,
  ref_kind TEXT NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  line INTEGER NOT NULL,
  col INTEGER NOT NULL,
  receiver TEXT,
  hint_json TEXT NOT NULL,
  UNIQUE(file, start_byte, end_byte, ref_kind),
  CHECK (0 <= start_byte AND start_byte < end_byte)
);
CREATE INDEX uses_name ON uses(lookup_name);
CREATE INDEX uses_container ON uses(containing_symbol);

CREATE TABLE bindings (
  use_id INTEGER PRIMARY KEY REFERENCES uses(use_id) ON DELETE CASCADE,
  target_id TEXT NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
  resolution TEXT NOT NULL CHECK(resolution IN ('exact', 'scoped'))
);
CREATE INDEX bindings_target ON bindings(target_id);

CREATE TABLE receiver_classes (
  use_id INTEGER PRIMARY KEY REFERENCES uses(use_id) ON DELETE CASCADE,
  class_qname TEXT NOT NULL,
  class_id TEXT REFERENCES symbols(id) ON DELETE CASCADE
); -- LR2: receiver class determined for an unbound member/scoped use;
   -- replaced with bindings; evidence for exclusion, never a binding
CREATE INDEX receiver_classes_class ON receiver_classes(class_id);

CREATE TABLE scopes (
  file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  scope_key TEXT NOT NULL,
  parent_scope_key TEXT,
  facts_json TEXT NOT NULL,
  PRIMARY KEY(file, scope_key)
); -- language-neutral owned lexical bindings, import aliases/module specifiers,
   -- declarations, type hints, assignment spans and shadowing facts

CREATE TABLE diagnostics (
  file TEXT NOT NULL,
  code TEXT NOT NULL,
  detail TEXT NOT NULL,
  start_byte INTEGER
);
```

Every file, including files with only top-level code, can own uses. Definitions and literal text are not uses. Keep import/type/reference extraction deduplicated with calls; a call use has `ref_kind = call`. A missing binding means unresolved, not a deleted use. `name_match` is query-relative evidence, not a false database foreign key to whichever definition happened to be unique.

Source positions use zero-based half-open UTF-8 byte offsets. Lines and byte columns are one-based; end lines are inclusive (the line containing `end_byte - 1`). Do not normalize CRLF before computing spans. Source bytes and hashes must agree. Case folding belongs to the language's lookup keys, not the ID or path. Preserve filesystem spelling; reject colliding normalized paths rather than merging files.

## Refresh and invalidation

```text
begin immediate transaction (busy timeout: 5 seconds)
load current fingerprint and file inventory
walk regular eligible files with local ignore rules
for each eligible file in path order:
    read bounded bytes and hash (default content mode)
    metadata mode may reuse bytes when size + mtime are unchanged
    parse only changed content or changed extractor fingerprint
    replace file facts, including status and stored source
remove deleted or newly excluded file facts
if content, membership, or resolver fingerprint changed:
    clear bindings; re-resolve all persisted uses/scopes
recheck observed metadata and eligible path set
if a detected race: roll back and retry once, then error 9
compute deterministic digest and commit
```

Update mtime/size even when the new hash equals the old hash. Ignore/config changes trigger a new walk; grammar or extraction changes reparse affected languages; resolution changes recompute bindings. A branch switch may replace an entire inventory. `git status` is unsuitable as the sole freshness source because it compares against Git's index/HEAD, rather than Rivet's snapshot. See the [Git status definition](https://git-scm.com/docs/git-status).

The snapshot digest is BLAKE3 over a canonical length-prefixed sequence: index-format version, effective config, parser/extractor/resolver fingerprints, then sorted eligible paths with language, content hash (when read), and parse/skip status. Omit mtimes, absolute root, transaction counters, wall time, and SQLite row IDs. Include stable diagnostics codes/spans, not OS-specific prose. Identical supported content/configuration yields the same digest regardless of update history. Skipped content is outside the digest's content-verification guarantee.

MVP correctness favors full re-resolution and one transaction over selective graph invalidation. New same-name declarations can invalidate previously unique bindings; deleted imports can invalidate uses in unchanged files. Benchmark this cost before introducing dependency tracking.

## Concurrency and source consistency

Use SQLite WAL on supported local filesystems, foreign keys on every connection, and one `BEGIN IMMEDIATE` writer per refresh. Parse inside the writer transaction in the simple first implementation; this can hold the writer slot longer but prevents competing refreshes from publishing out of order. Readers use a committed read transaction. No partial batches become query-visible. A crash or cancellation before commit rolls back the refresh. SQLite provides committed-transaction visibility and WAL snapshot reads; see [SQLite isolation](https://www.sqlite.org/isolation.html).

Never slice a live file with positions read from the database. `--source`, signatures, and context read the stored bytes. Returned hashes let a consumer validate that live files still agree before acting. Content verification cannot prevent another process editing a file after the scan; this is stated in the output contract. Network filesystems are outside the v0.1 supported storage target.

`--no-refresh` reads a compatible existing snapshot, explicitly labeled `cached`. Default refresh failures do not fall back to cached answers. An index-format mismatch rebuilds the disposable cache under the writer lock, preserving config; never silently overwrite a database from a newer unsupported format or one failing integrity checks. Return exit 3 with an explicit rebuild hint. `index --force` may rebuild that cache after validation of the destination, never user source/config.

## Parse and coverage policy

If Tree-sitter returns any error or missing node, record `parse_error` and publish no facts for that file. This intentionally conservative MVP policy avoids apparently precise output from partially recovered syntax. Tree-sitter distinguishes [ERROR and MISSING nodes](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html). Clear formerly valid facts when a file becomes invalid; unrelated query results remain available with partial coverage.

Unsupported language, binary, oversize, encoding, and deterministic parser-resource skips are counted separately. I/O errors abort refresh. Ignore exclusions and nested repositories are outside the scan domain, not parse errors. Every successful indexed command exposes coverage counts and bounded diagnostics, even if its primary result list is empty.

## Resolution and context

Resolution applies the ordered rules in spec §11 using persisted lexical scope facts. Direct imports must have one supported declaration binding; receiver-based member links remain `scoped`. Ambiguous hints and unsupported dynamic binding remain unresolved. Candidate mode joins uses by normalized identifier as well as real bindings; reference mode removes uses bound to other declarations.

Context uses spec §16's fixed integer ordering, explicit graph-work caps, overlap suppression, and `ceil(UTF8_bytes / 3)` source estimates. No floating-point ranking or PageRank in v0.1. Test context is derived from callers in configured test paths, never claimed as execution coverage.

## Determinism

Sort extraction results before writing and query lists before serializing. Parallel parsing is optional; introduce it only after the sequential path passes fixtures. Internal integer IDs never reach output. Query results include no elapsed times or refresh-work counters. Administrative reports legitimately vary with prior index state. Actual query results, not SQLite file bytes, are the determinism contract.
