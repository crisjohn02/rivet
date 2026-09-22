# Authored PHP fixture

Nine hand-written PHP files used as the declaration/use gold source for the
PHP extraction, resolution, and context tasks. All are UTF-8, LF, begin with
`<?php`, declare `strict_types=1`, and use namespaces. The original three
files (`SurveyService.php`, `ReportService.php`, `boot.php`) carry the
same-name, alias, top-level-call, receiver, and interpolation cases; T25 added
the remaining six files for the other MVP declaration kinds and collapsed
signatures.

## Original three (references and resolution)

Cases, each marked in the source with `// gold:` comments:

- **a** — `App\Services\SurveyService::launch` and
  `App\Reporting\ReportService::launch`: same short name in two namespaces.
- **b** — `use App\Services\SurveyService as SurveySvc;` and a call through
  the alias, whose spelling differs from the class target.
- **c** — top-level `launch();` call with no containing symbol (`boot.php`).
- **d** — `$this->launch()` inside `SurveyService`, scoped via `$this`.
- **e** — `$svc = new \App\Services\SurveyService(); $svc->launch();`, scoped
  via the preceding `new`.
- **f** — `runTyped(SurveyService $svc)` calling `$svc->launch()`, scoped via
  the explicit parameter type.
- **g** — `$x->launch()` with `$x` from an unknown source; unresolved by
  design in v0.1, so it remains `name_match` with no target.
- **h** — the word `launch` in a comment and in a string literal (not uses),
  and the interpolation `"{$svc->launch()}"` (its inner call is a use).
- **i** — extra kinds for later tasks: `function launch`, class constant
  `DEFAULT_LABEL`, and typed property `$label`.

## T25 declaration-kind files

- **`Interface.php`** — `interface Named` with a class constant and two
  bodyless method declarations.
- **`Trait.php`** — `trait Greets`, extracted as a class-like container, with a
  class constant and a method.
- **`Enums.php`** — a pure `enum Suit` and a backed `enum Status: string`,
  each with `case` declarations extracted as constants.
- **`Members.php`** — `abstract class AbstractThing` with a multi-name
  constant, `static`/`readonly`/typed and multi-name properties, an abstract
  method, a constructor with promoted properties, and an anonymous class whose
  method is deliberately not addressable.
- **`Documented.php`** — doc comments, including one separated from its method
  by a blank line.
- **`Constants.php`** — a top-level multi-name `const FIRST = 1, SECOND = 2;`
  declaration with no parent symbol.

Expected spans live in `../gold/php-authored.toml`; run `python3
tests/gold/check_gold.py` from the repository root to verify them.
