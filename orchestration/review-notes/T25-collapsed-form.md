# Review defect: the collapsed container form is redundant and misleading

Found by reading `signature_summary`'s own test expectations in T25. All three
faults are asserted as correct by that test, so they are deliberate, and two of
the three were not disclosed in the report.

The summary produced for the new `Members.php` fixture, quoted from the test:

```text
abstract class AbstractThing
{
    public const FIRST = 1, SECOND = 2;
    public const FIRST = 1, SECOND = 2;
    public static int $count = 0;
    public readonly string $title;
    public int $left, $right = 2;
    public int $left, $right = 2;
    abstract public function describe(): string { … }
    public function __construct(private int $seed, public string $tag = 'x') { … }
    private int $seed;
    public string $tag = 'x';
}
```

## 1. Multi-name declarations repeat the whole declaration, once per name

`FIRST, SECOND` and `$left, $right` each appear twice, because every element is
its own symbol and every symbol renders the shared declaration header. A reader
of `$right`'s signature also sees `$left` named first. Not disclosed.

## 2. Bodyless methods render with a body

`abstract public function describe(): string { … }` claims a body the method
does not have. The same applies to interface methods. Disclosed as an
interpretation, and it follows the stated method rule, but the rule is wrong for
bodyless declarations.

## 3. Promoted constructor properties appear three times

Once inside the constructor's own signature, then again as two standalone
members. Not disclosed.

## Why it matters

Spec §16.2 makes the purpose explicit: the collapsed form exists because it
tells an agent what exists and how to call it "at a fraction of the cost of its
body". A summary that repeats declarations inflates exactly the cost the form
was created to cut, and a body on an abstract method is simply false.

No user-facing impact yet. `signature_summary` still has no callers outside its
own unit tests. It must be corrected before the context builder consumes it.

## Direction

Render each element's own signature rather than the shared header, so `$right`
renders `public int $right = 2`. Omit ` { … }` when the declaration has no body.
Decide whether a promoted property is listed separately or left implied by the
constructor signature, and state the rule; listing it in both places is the one
option to reject.
