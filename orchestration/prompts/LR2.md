# LR2: evidence-based exclusion in `refs --mode references`

Read `AGENTS.md` first and follow it. Then read only:

- `docs/TASKS.md`, the checkpoint table and rows T36d and LR2.
- `rivet-agent-native-codebase-cli-spec.md` §11.3, §11.4 (the rule list around
  line 401) and §11.5.
- `docs/OUTPUT-CONTRACT.md`, the `refs` and `context` sections.
- `docs/ADDING-A-LANGUAGE.md`, the language support matrix.
- `crates/rivet-cli/src/references.rs` (`collect_matches`, `Selection`, `Mode`).
- `crates/rivet-index/src/hierarchy.rs` (T36d: `Hierarchy`, `ancestors`).
- `crates/rivet-index/src/resolve/mod.rs` and `rules/receivers.rs`, `rules/new_expr.rs`:
  how a receiver's class is determined from `UseHint` (`This`, `SelfOrStatic`,
  `NewExpr`, `Typed`) and when a member binding is created.
- `crates/rivet-store/src/lib.rs`, the `bindings` table and schema versioning.

Work in this worktree only. Do not commit; the reviewer commits.

## Why

On a real Laravel application, about 93% of call uses are unbound. Reference
mode today keeps every unbound use whose name matches the target, so
`refs SomeModel::save` lists every `->save()` call in the project, including
calls on framework objects whose class is known and unrelated. The pilot agent
then reads and discards them. The resolver often *knows* the receiver's class (a
typed parameter, `new X`, `$this`) but, when no indexed member is found, that
knowledge is dropped.

## Rule (normative; add it to spec §11.5 and ADDING-A-LANGUAGE)

In `--mode references`, an **unresolved** use whose name matches the target is
excluded when evidence shows it cannot refer to the target. There are two kinds
of evidence, and only these two:

1. **Use form incompatible with target kind.** Derive the table from the
   extractor's actual `RefKind`s and receiver presence, write it into the spec,
   and implement exactly that table. Its intent:
   - A method target can be referenced only by a call with a member (`->`,
     `?->`) or scoped (`::`) receiver.
   - A function target can be referenced only by a call without a receiver, or
     by a function import.
   - A property target can be referenced only by a member or scoped property
     access, a read or a write.
   - A class-like target can be referenced only by a type use, an import, or a
     `new`.
   - A constant follows the same pattern.
   - `RefKind::Unknown` is never excluded by this rule.
   - Check the table against the extractor rather than trusting this list. If
     the extractor records a form this list misses (for example a callable
     string or array), keep that form, and report it.
2. **Receiver class known and unrelated.** This applies to a member or scoped
   use whose receiver class was determined by an existing receiver rule, with
   the same trust conditions that rule applies before binding.
   - **The rule:** exclude the use when the determined class R and the
     target's containing class-like T are *unrelated*. They are related if R
     is T, T is among R's ancestors, or R is among T's ancestors, all using
     `Hierarchy::ancestors`.
   - **Never exclude when:**
     - T is a trait. Trait use is not tracked.
     - The receiver rule would have refused to determine a class, for example
       because of conflicting hints or a reassignment the rebinding rule
       rejects.
   - **Unindexed classes:** an unindexed (vendor) class is not assumed to
     extend an indexed project class. State this assumption in the spec.
   - **Unresolved names:** when R's name cannot be resolved to a qualified
     name, do not exclude. The same applies when an ancestor chain has an
     unresolved link that could be T. Only a qualified name known to differ
     from T's may be treated as not T.

Exclusion never changes a tier and never binds anything. It is not
resolution: an excluded use stays an unresolved use, and `--mode candidates`
lists it exactly as before, byte-identical. Uses bound to the target are never
excluded. `context` uses reference mode and so inherits the rule.

## Build

- Persist the determined receiver class for unbound member/scoped uses. For
  example, add a nullable qualified class name to a per-use resolution record, or
  add a row kind to `bindings`. Choose the smallest schema change, bump the store
  schema version following the existing rule, and bump `RESOLVER_FINGERPRINT`
  to `php-rules-v3` with a history note.
- Apply the rule in `collect_matches` for `Selection::Query(Mode::References)`
  only. `Selection::Contained` (`symbol.calls`) is unchanged.
- **Honesty in output.** Reference mode reports how many name-matching uses it
  excluded:
  - add a `by_exclusion` object to the `refs` JSON, for example
    `{"incompatible_form": n, "unrelated_receiver": n}`, always present with
    zeros. It is additive, so document it in OUTPUT-CONTRACT with its
    ordering and determinism rules;
  - add one text line when either count is non-zero, for example
    `12 name matches excluded by evidence (see --mode candidates)`;
  - counts are computed before pagination and are independent of the page.
- Update spec §11.5, the §11.4 rule list if the inheritance wording there now
  contradicts, spec line ~433 on inheritance ("deferred": say hierarchy facts
  are stored and used only for exclusion, not for resolution), and the
  ADDING-A-LANGUAGE matrix. Inheritance *traversal for binding* stays
  unsupported.

## Tests (adversarial cases are required)

Fixture-style integration tests (temp repos, like `tests/hierarchy.rs`):

- A `->save()` on a `Request $r` parameter is excluded from `refs App\M::save`;
  it is present in `--mode candidates` with the same bytes as before this task.
- A receiver of a subclass of T, a receiver of T's parent class, and a
  receiver of an interface T implements are all kept.
- `$this->m()` in an unrelated class is excluded. In a subclass of T it is kept.
  When the target is a trait method, `$this->m()` in any class is kept.
- An untyped `$x->m()` is kept, as is a receiver typed by a name that does not
  resolve.
- Conflicting hints: the receiver rule refuses, so the use is kept.
- A bare `m()` function call against a method target is excluded, and a
  `$x->f()` call against a function target is excluded. A `->items()` call
  against property `$items`, and a `$x->items` read against method `items`,
  are both excluded.
- A use bound to the target is never excluded, even with weird receivers.
- `by_exclusion` counts are correct and page-independent (`--limit 1
  --offset 1`).
- Determinism across `--force` refresh.
- A `rivet context` on a method omits the excluded uses. Check its budget
  accounting stays consistent.

The authored fixture's gold must not change. Existing goldens may change only
through the new `by_exclusion` field, the snapshot digest (fingerprint bump),
and genuinely excluded uses; list each one and why.

## Checks

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 tests/gold/check_gold.py
```

Then add and tick an LR2 row in `docs/TASKS.md` and update the checkpoint table.

## Report

- the rule table as implemented;
- the schema change;
- every golden changed;
- the check results;
- any case where the documents and the code disagree, or where the rule above
  is ambiguous or wrong. Report these; do not silently resolve them.
