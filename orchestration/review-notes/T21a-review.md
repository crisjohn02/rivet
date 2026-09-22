# T21a review: passed, with disclosed gaps carried to T21b

## Verified by hand against the T21a binary

Every rebinding form from the original defect note is now unbound, and the safe
single-assignment case still binds `scoped`:

| Form | Before | After |
|---|---|---|
| `foreach ($xs as $s)` | bound | unbound |
| `[$a, $s] = ...` | bound | unbound |
| `list($a, $s) = ...` | bound | unbound |
| `$s = &$o` | bound | unbound |
| `catch (\Throwable $s)` | bound | unbound |
| by-ref closure capture | bound | unbound |
| `$s ??= mk()` | bound | unbound |
| `$s = mk()` control | unbound | unbound |
| safe single `new` | bound | bound |

Checks pass in a private target directory: fmt, clippy with warnings denied, the
full workspace tests, and 25 gold entries. The authored fixture is unchanged at
13 bindings, which is the signal the prompt asked for that nothing else moved.

The implementor bumped `fact-schema` from 1 to 2 because `new_bindings` changed
meaning. That is exactly the rule T22 introduced, applied without being asked.

## Gaps it disclosed, and I confirmed are real

These still produce a wrong `scoped` binding. Each was stated in the report
rather than hidden, which is why T21a is accepted rather than rejected.

- A callee-declared by-reference parameter, `function f(&$x)` called as `f($s)`.
  Confirmed bound.
- A by-reference builtin, such as `preg_match('/x/', $subject, $s)`. Confirmed
  bound.
- Dynamic variable writes, `$$name = ...` and `${$name} = ...`. Confirmed bound.
- `$GLOBALS['x'] = ...`, `extract($arr)`, and `eval()`.

## Direction for T21b

Most of these do not need real analysis, only conservative suppression. If a
scope contains a construct the walker cannot account for, such as a dynamic
variable write, `extract`, `eval`, or a by-reference argument to a callee whose
signature is unknown, then no `new`-receiver binding in that scope should be
recorded at all. The by-reference parameter case is resolvable for an indexed
callee by reading its declared parameters; an unindexed or builtin callee falls
back to suppression.
