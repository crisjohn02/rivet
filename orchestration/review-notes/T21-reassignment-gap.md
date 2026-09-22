# Review defect: T21 condition (a) catches only one assignment form

Found while reading the full T21 diff after the fact. The four-condition hand
probe missed it because the probe used the same syntax the prompt named.

## What is wrong

T21's `new_expr` rule refuses to bind when a receiver variable is assigned more
than once in its scope. It counts assignments from `ScopeFacts::new_bindings`,
which the PHP walker fills only from `bind_variable`, and `bind_variable` is
reached only from a simple `$x = ...` assignment expression. Every other way of
rebinding a variable is invisible, so the variable still looks singly assigned
and the call binds to the class of the stale `new`.

## Reproduction

Built from `0cce03a` (current main). Each function assigns `new Alpha()`, then
rebinds `$s` by some other means, then calls `$s->go()`.

| Form | Result | Correct? |
|---|---|---|
| `foreach ($xs as $s)` | binds `Alpha::go` scoped | no |
| `[$a, $s] = [1, 2]` | binds `Alpha::go` scoped | no |
| `$s = &$o` | binds `Alpha::go` scoped | no |
| `catch (\Throwable $s)` | binds `Alpha::go` scoped | no |
| `function () use (&$s) { $s = mk(); }` | binds `Alpha::go` scoped | no |
| `$s ??= mk()` | binds `Alpha::go` scoped | not proven wrong; `??=` does not fire on a non-null value, so treat as unverified rather than as a counterexample |
| `$s = mk();` (control) | unbound | yes |

The control line proves the rule works for the one form the walker records, so
this is a recording gap and not a resolver gap.

## Why it matters

Five of these produce a scoped binding with no rule justifying it. That breaks
the honesty non-negotiable: a receiver whose class cannot be established must
record nothing. It is worse than a missing binding because a wrong target is
indistinguishable from a right one in output.

## Suggested direction

Enumerating PHP's rebinding forms one by one is fragile and unbounded. Prefer
inverting the test: bind only when the walker can account for every mention of
the variable in the scope. Any occurrence of the variable in a position the
walker does not classify as a read should suppress the binding. Whatever the
approach, the safe default on an unrecognised construct is no binding.
