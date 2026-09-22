# Authored PHP fixture

Three hand-written PHP files used as the declaration/use gold source for the
PHP extraction and resolution tasks. All are UTF-8, LF, begin with `<?php`,
declare `strict_types=1`, and use namespaces.

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

Expected spans live in `../gold/php-authored.toml`; run `python3
tests/gold/check_gold.py` from the repository root to verify them.
