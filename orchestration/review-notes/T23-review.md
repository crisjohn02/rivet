# T23 review: passed

Reviewed against the built binary, not the implementor's report.

## Checks

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, `cargo test --workspace`, and
`python3 tests/gold/check_gold.py` all pass in a private target directory.

## Hand probes beyond the task's tests

- **Key order** matches `docs/OUTPUT-CONTRACT.md` exactly, both the nine
  top-level keys and the eleven reference-object keys, in order.
  `by_resolution` always carries exactly `exact`, `scoped`, `name_match`.
- **Reference mode** on the survey method returns six references and correctly
  EXCLUDES the `boot.php` call bound to the same-name free function.
- **Candidate mode** adds exactly that call as `name_match` while retaining its
  real `resolved_target`, which is the contract's query-relative rule.
- **An unresolved same-name use** appears in both modes as `name_match` with a
  null target.
- **Pagination** at limit 2 over seven matches: the four pages reconstruct the
  full list exactly, with no overlap and no gaps, `next_offset` is correct and
  null on the last page, and `by_resolution` is byte-identical on every page,
  proving counts are taken before pagination.
- **Offset past the end** yields an empty page with `truncated: true` and
  unchanged counts, as the contract requires, rather than an error.
- **`--min-resolution scoped`** drops name matches and zeroes that count.
- **Argument rejection**: `--limit 0`, `--limit 1001`, an unknown `--mode`, an
  unknown `--kind`, and `--no-refresh` with `--freshness` each exit 2 with a
  four-field error envelope on stderr and an empty stdout.
- **Gold negatives**: neither mode emits the `[[not_a_use]]` spans.
- **Determinism**: the same query twice is byte-identical.

## Disclosed limitation, accepted

The implementor reported that name matching uses the persisted `lookup_name`,
and PHP property declarations keep a leading `$` while member uses drop it, so
an unresolved `$label` property use would not name-match the `$label`
declaration. Bound uses are unaffected because they arrive through their
binding. This is a pre-existing store normalization detail. It is not exercised
by the fixture and is worth closing when property references matter.
