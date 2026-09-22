Implement AF1 from docs/TASKS.md only. This is an audit-fix task, not new scope. Read AGENTS.md first, then `orchestration/review-notes/audit-2026-09-22.md` findings 1, 2 and 3 (reproductions included), the AF1 row in docs/TASKS.md, `docs/ADDING-A-LANGUAGE.md` "MVP support boundary" (the qualified-name row), spec §11.3, `crates/rivet-languages/src/php/uses.rs`, `crates/rivet-languages/src/php/mod.rs`, `crates/rivet-index/src/resolve/mod.rs`, and the rules under `crates/rivet-index/src/resolve/rules/`. Nothing else.

AF1 — Fix PHP namespace and scope structure. All three findings produce wrong `exact` bindings or wrong symbol identities that look right, which breaks the project's honesty rule.

The defects, each reproduced by the orchestrator on the current binary:

1. A top-level closure or arrow function loses the file's namespace and imports. `parent_scope_key` in `uses.rs` returns `None` for any `top:N` key, so the closure scope has no parent chain to the file scope. With `namespace App; use App\Lib\Tool; $f = function(){ launch(); new Thing(); new Tool(); };` and global `launch` and `Thing` declared elsewhere, `new Thing()` binds `exact` to the global class.
2. A file with several namespaces resolves every block against the first. `resolve/mod.rs` takes the first `Module` in the declares list, and every block's imports and functions land in one `top:file` scope. `namespace First; class Widget{} function build(){}` followed by `namespace Second; function caller(){ build(); new Widget(); }` binds both uses `exact` to `First\...`. A `use` inside one braced block also leaks into another block.
3. A global `namespace { }` block inherits the previous block's name. `php/mod.rs` records a namespace only when it has a name. `namespace A { class X {} } namespace { class Y {} function helper(){} }` produces IDs `a.php#A\Y` and `a.php#A\helper` instead of `a.php#Y` and `a.php#helper`.

Read this before fixing finding 1. The function rule in `rules/functions.rs` deliberately implements PHP's runtime fallback: an unqualified function call inside a namespace resolves to the namespaced function when one exists, and otherwise to the global function. PHP has NO such fallback for classes. So once the closure correctly sees `namespace App`, `launch()` may legitimately bind to the global `launch` through that fallback when no `App\launch` exists, while `new Thing()` must NOT bind to the global `Thing`, and `new Tool()` must bind to `App\Lib\Tool` through the import. Do not "fix" the function fallback; it is existing, intended behavior. Assert each of the three uses separately.

Requirements:

1. Every scope inside a namespace block must resolve against that block's namespace and only that block's imports. That covers top-level code, closures, arrow functions, functions, methods, and anonymous functions nested at any depth.
2. PHP has two namespace syntaxes. With the unbraced form, `namespace X;`, a namespace runs until the next namespace statement or the end of the file. With the braced form, `namespace X { }`, it runs for the braces. A `use` import belongs to the block it appears in. Handle both, and a file that has no namespace at all.
3. A global `namespace { }` block declares names in the global namespace, so its symbols carry no namespace prefix. This changes those symbols' canonical IDs, which is the intended correction. Say which IDs change.
4. The default on any structure the extractor cannot attribute to exactly one namespace block is no binding. Never guess, and never `exact` without a rule.
5. Do not change resolution for files with a single namespace, which is every file in the authored fixture. `crates/rivet-cli/tests/bindings.rs` asserts an exact binding count on the fixture, and `python3 tests/gold/check_gold.py` must still verify 51 entries. If either would change, stop and report rather than adjusting the expected numbers.
6. Bump the `fact-schema` component in `EXTRACTOR_FINGERPRINT`, since recorded scope facts change shape or meaning and an existing index must reparse. Add a history line to its doc comment in the existing style.
7. Tests. One test per audit reproduction, asserting the exact correct binding or its absence, including the three separate assertions for finding 1. Add: an unbraced multi-namespace file where each block's `use` import resolves only within its own block; a braced multi-namespace file; a nested closure inside a method inside the second namespace block; and the global-block symbol IDs from finding 3. Put end-to-end coverage in `crates/rivet-cli/tests/bindings.rs` or a new sibling file, and say which.
8. Out of scope, but report if you encounter it: the audit suspects that when a namespaced function's own file failed to parse, a call can fall back to a global function and bind `exact` under partial coverage. Do not fix it here.
9. Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, and `python3 tests/gold/check_gold.py`. Paste real results with exact per-crate counts.
10. Update docs/TASKS.md: tick AF1 and update "Last checks" only. Do NOT change "Last completed task" or "Next task": the main task queue owns that checkpoint. Do not touch other docs. Do not run git.

Report contradictions rather than silently resolving them.

Finish with the AGENTS.md final block.
