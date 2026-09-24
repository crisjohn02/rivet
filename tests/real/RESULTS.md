# Real-code results (T46)

The last recorded run of `tests/real/check_real.py` on the pinned Hono
checkout and the authored fixtures, plus the honesty audit read by hand.
Everything here is reproducible offline once `tests/real/fetch.sh` has run:

```sh
cargo build --release
tests/real/fetch.sh                      # network: the pinned commit only
python3 tests/real/check_real.py         # every check; exits 1 on any FAIL
python3 tests/real/check_real.py --audit # the seeded audit sample (seed 46)
python3 tests/real/test_check_real.py    # offline unit tests of the helpers
```

`benchmark/runner/test_runner.py` collects only its own directory's modules,
so the helper tests are run by the last command above, not by that runner.

## Pin and environment

| Field | Value |
|---|---|
| Project | Hono, <https://github.com/honojs/hono>, MIT |
| Tag / commit | `v4.13.9` (annotated tag object `96e00d99`), peeled commit `7c3b0df96dbf3e767968ff5afe4c0d999257b8ee`, verified by `fetch.sh` with `git ls-remote` and `git rev-parse HEAD` |
| Tracked files | 488: 329 `.ts` (not `.d.ts`), 32 `.tsx`, 1 `.d.ts`, 27 `.mts`, 1 `.js`, 2 `.mjs`, 96 others |
| Left out by the walk | 5 `.ts` under `build/` (spec §26 default exclusion) and 2 tracked `.gitkeep` files the project's `.gitignore` matches, so 481 files are seen |
| rivet | commit `fca8f07` (task/T46 base; T46 changes no Rust), `cargo build --release` |
| Machine | Apple Silicon macOS laptop, 12 cores; `--jobs 12` query processes in parallel |
| Seed | 46 (`--seed`); samples use `random.Random` seeded from strings, so they do not depend on hash seeds |
| Python | recorded run under 3.14.7; the final script re-run under macOS's system 3.9.6 printed the same PASS lines (apart from added parentheses in the coverage lines), step plans, and counts, and `--audit` drew the same samples |
| Wall time | 13 min 56 s (3.14.7) and 13 min 59 s (3.9.6) for the full `cargo build --release && tests/real/fetch.sh && python3 tests/real/check_real.py` |

PHP has no public real project in this repository. PHP's real-project
coverage stays with the private benchmark corpus (`benchmark/corpus.toml`);
here PHP is exercised through the authored fixture, alone and mixed with the
authored TypeScript fixture.

## Check results

All checks passed: `check_real: PASS (0 failed check(s))`, exit 0, in both runs.

Every compared answer is a `rivet` CLI answer. The script also opens each
store (`.rivet/index.db`) read-only, between rivet runs, to enumerate
symbols and bindings for sampling and for the coverage cross-checks below.

### Coverage

| Tree | Seen | Indexed | Skipped | Symbols | Uses | Bindings `exact` | Bindings `scoped` |
|---|---:|---:|---|---:|---:|---:|---:|
| hono | 481 | 349 | 124 unsupported, 8 parse_error | 2,468 | 92,303 | 8,215 | 1,070 |
| php fixture | 10 | 9 | 1 unsupported | 50 | 14 | 6 | 7 |
| typescript fixture | 17 | 12 | 4 unsupported, 1 parse_error | 82 | 138 | 51 | 16 |
| mixed (both fixtures) | 26 | 21 | 4 unsupported, 1 parse_error | 132 (50 PHP, 82 TS) | 152 (14 PHP, 138 TS) | 57 (6 PHP, 51 TS) | 23 (7 PHP, 16 TS) |

Hono's 124 unsupported files are 27 `.mts` and every non-TypeScript file
(JSON, Markdown, YAML, `.js`, `.mjs`, ...). Its 8 parse failures, by path:
`src/context.ts`, `src/helper/factory/index.ts`,
`src/helper/ssg/middleware.ts`, `src/jsx/dom/index.ts`,
`src/jsx/hooks/index.ts`, `src/jsx/index.ts`, `src/types.ts`,
`src/utils/body.ts` (causes under "Known limitations" below). No file hit a
resource limit. In every tree `files_seen = files_indexed + sum(skipped)`,
`complete` agrees with the skip counts, the JSON counts of files, symbols,
uses, and bindings equal the store's rows, and the `parse_error` diagnostics
name exactly the store's parse-failed files.

### Determinism

Each tree was copied to two different directories and indexed clean in each.

| Tree | `index --json` (timing dropped) | Queries compared | Result |
|---|---|---:|---|
| hono | identical | 800 (200 of 2,468 symbols, seeded) | byte-identical |
| php fixture | identical | 200 (all 50 symbols) | byte-identical |
| typescript fixture | identical | 328 (all 82 symbols) | byte-identical |
| mixed | identical | 528 (all 132 symbols) | byte-identical |

Each symbol is queried four ways, all `--json --no-refresh`: `symbol`,
`refs --limit 1000`, `refs --mode candidates --limit 1000`, and `context`.
Exit code, stdout, and stderr are compared byte for byte, snapshot digest
included. The only field dropped from `index --json` is `elapsed_ms`.

### Full versus incremental

Two identical copies of a tree are indexed clean. Each scripted step is
planned (seeded) against the incremental copy's current store, applied to both
copies (their trees are then checked equal, `.rivet/` aside), refreshed with
`rivet index` in one copy and `rivet index --force` in the other, and
compared: snapshot digest, `coverage`, `diagnostics`, and the symbol, use, and
binding counts from both `index` outputs, then the four queries above for
every symbol of the edited files and of their dependents (files with a use
bound into an edited file, before or after the step, in either store) plus a
seeded sample of 200 others. Any byte difference, digest included, is a FAIL.

| Tree | Steps | Queries per store | Result |
|---|---:|---:|---|
| hono | 8 | 9,904 | all identical |
| php fixture | 8 | 1,620 | all identical |
| typescript fixture | 8 | 2,344 | all identical |
| mixed (PHP script, then TypeScript script) | 16 | 8,436 | all identical |

The Hono steps:

| Step | Edit | Edited / dependent files | Symbols compared (from edited or dependent files) |
|---|---|---|---|
| 1 modify a function body | a call inserted before the closing brace of `src/middleware/bearer-auth/index.ts#bearerAuth` | 1 / 0 | 207 (7) |
| 2 rename a method its callers use | `src/router/reg-exp-router/prepared-router.ts#PreparedRegExpRouter.add` renamed with its 3 bound uses | 2 / 0 | 214 (14) |
| 3 remove an export others import | `export` dropped from `src/jsx/intrinsic-elements.ts#JSX` (imported by 2 files) | 1 / 4 | 920 (720) |
| 4 add a file importing an existing one | `src/jsx/dom/rivet-t46-new.ts` imports `build` from `./render` | 2 / 7 | 285 (85) |
| 5 delete a file | `src/utils/jwt/index.ts` (3 dependents) | 1 / 3 | 211 (11) |
| 6 rename a file | `benchmarks/jsx/src/react-jsx/hono.ts` to `hono-rivet-t46.ts` (1 dependent) | 2 / 1 | 203 (3) |
| 7 introduce a syntax error | a broken declaration appended to `src/utils/compress.ts` (2 dependents); the file became `parse_error` in both stores | 1 / 2 | 218 (18) |
| 8 fix it | `src/utils/compress.ts` restored | 1 / 2 | 218 (18) |

PHP has no exports, so the PHP script's step 3 renames the declaration of a
class another file imports with `use` (`SurveyService`), which breaks the
import the same way. Where no file had dependents left (late PHP steps), the
planner fell back to any indexed file and says so (`tier 1`) in its output.

**The check can fail.** Run once, not committed, with the incremental
refresh skipped at step 2 (a copy of the script with that one line changed),
on the TypeScript fixture:

```text
FAIL incremental[typescript] typescript/2-rename_method: rename method src/services/survey.ts#SurveyService.launch to launchRivetT46 and its 6 bound uses (tier 0): 338 difference(s)
    snapshot digest differs: incremental STALE: refresh skipped vs force blake3:9b3318d7...
    ...
    symbol src/anonymous.ts#mapAll: stdout at byte 48 (lengths 1591/1591): ... "snapshot":"blake3:fb1c62b4... vs ... "snapshot":"blake3:9b3318d7...
PASS incremental[typescript] typescript/3-remove_export: ...
FAIL incremental[typescript]: 338 problem(s) over 8 steps
check_real: FAIL (2 failed check(s))
```

With the `index` object also dropped from the comparison, so that the stale
digest alone cannot fail it, the same run still reported 142 differences at
step 2: the 6 `index`-output ones above and 136 query answers whose content
differs (for example `refs src/models.ts#Box.value` differs at
byte 3130, in a reference row's `content_hash` for the edited
`src/services/survey.ts`). Step 3 passes again because its refresh catches up.

### Two languages in one repository

The authored TypeScript fixture and the authored PHP fixture were copied into
one tree, each keeping its own relative paths (their only shared path is the
unsupported `README.md`; the TypeScript one is kept).

- PHP: all 50 symbols x 4 queries (200) in the mixed tree equal the PHP-only
  tree's, with the top-level `index` object removed from both.
- TypeScript: all 82 symbols x 4 queries (328) equal the TypeScript-only
  tree's, likewise.
- 0 of the mixed tree's 80 bindings join a use and a declaration of different
  languages, and 0 of the 255 rows that `refs --mode references` and `refs
  --mode candidates` return over all 132 symbols list a use in another
  language, although 13 names are declared in both languages (`Active`,
  `ReportService`, `Status`, `SurveyService`, `describe`, `first`, `greet`,
  `launch`, `name`, `relaunch`, `runTyped`, `runUnknown`, `status`).

The comparison drops the whole `index` object, not only the snapshot digest:
the mixed tree's `coverage` counts both fixtures' files (26 seen, not 10 or
17) and its `diagnostics` list `src/broken.ts`, so those fields necessarily
differ from a single-language tree's.

### Budget honesty

100 Hono symbols (seeded) x `--tokens 1000` and `--tokens 4000`, `context
--json --no-refresh`: 200 queries.

| Outcome | Count |
|---|---:|
| fitted (exit 0) | 199, with 600 segments |
| fitted answers that left candidates out for budget (`omitted.budget > 0`) | 33 |
| `budget_too_small` (exit 8) | 1 |

In every fitted answer `budget_tokens` is the requested budget,
`estimated_tokens` is at most it and equals the sum of the segment estimates,
each segment's `estimated_tokens` is `ceil(UTF-8 bytes / 3)` of its `source`,
every `full` segment's `source` is exactly the stored bytes of that symbol's
span (no partial body), the first segment is the target, and `tokenizer`/
`budget_scope` are `utf8-bytes-v1`/`source`. The exit-8 answer reports
`required_tokens` above the budget and no larger than the target's full
estimate. The median fitted answer uses 1.5% of its budget: most Hono targets
are small and have few related candidates. The budget scope is source only;
rendered JSON size is T47's.

## Timings (indicative)

benchmark/corpus.toml's protocol on Hono, from `elapsed_ms`, release build:

| Measure | Value |
|---|---:|
| cold index (median of 5, `.rivet/` deleted first) | 776 ms (745, 776, 782, 770, 827) |
| no-change refresh (median of 3) | 46 ms (47, 43, 46) |
| `index --force` (one run) | 1,156 ms |

The 3.9.6 re-run measured 763 ms (743, 879, 763, 874, 731), 39 ms (39, 39,
40), and 860 ms, which gives a feel for the run-to-run spread.

Query latency, wall clock of one process, median of 5, measured separately
after the run: `symbol`, `refs`, and `context` take 178-199 ms with the
default content refresh and 252-284 ms with `--no-refresh`, so a cached read
is slower than a verified one on this tree. Reported as a follow-up, not
investigated here.
PF2 fixed this; see "PF2 addendum" at the end.

## Honesty audit

Seeded samples from a clean Hono index (`check_real.py --audit`, seed 46),
each read against the source: 40 of the 8,215 `exact` bindings, 40 of the
1,070 `scoped` bindings, and 40 of the 19,784 unresolved member calls
(a `call` use with a receiver and no binding).

**Wrong bindings: none.** Every sampled `exact` and `scoped` binding names the
declaration the use refers to. With 0 errors in 40, the one-sided 95% upper
bound on each tier's error rate is 7.2%. Checked for each: the import
statement and its specifier's module for cross-file bindings, the absence of a
shadowing local or type parameter for same-file ones, and for `scoped` ones the
receiver's annotation or `new` (for example `let fsMock: FileSystemModule`,
`const req = new HonoRequest(...)`, `let router: Router<string>`, `this`
inside the declaring class). An interface receiver binds the interface's own
member (S04, S19-S22), as the T45 rule states; it is not the implementation
that runs.

### `exact` sample

| # | Use | Kind | Name | Target | Verdict |
|---|---|---|---|---|---|
| E01 | `benchmarks/jsx/src/react-jsx/preact.ts:2:10` | import | `buildPage` | `benchmarks/jsx/src/react-jsx/page-preact.tsx#buildPage` (function) | correct |
| E02 | `benchmarks/jsx/src/react.ts:5:44` | call | `buildPage` | `benchmarks/jsx/src/page-react.tsx#buildPage` (function) | correct |
| E03 | `runtime-tests/lambda/index.test.ts:954:23` | unknown | `testApiGatewayRequestContextV2` | `runtime-tests/lambda/index.test.ts#testApiGatewayRequestContextV2` (const) | correct |
| E04 | `src/adapter/aws-lambda/handler.ts:253:9` | type | `WithHeaders` | `src/adapter/aws-lambda/handler.ts#WithHeaders` (type_alias) | correct |
| E05 | `src/adapter/bun/websocket.test.ts:69:15` | type | `BunWebSocketData` | `src/adapter/bun/websocket.ts#BunWebSocketData` (interface) | correct |
| E06 | `src/adapter/bun/websocket.ts:25:11` | type | `WSEvents` | `src/helper/websocket/index.ts#WSEvents` (interface) | correct |
| E07 | `src/client/client.test.ts:806:22` | type | `Hono` | `src/hono.ts#Hono` (class) | correct |
| E08 | `src/client/client.test.ts:1119:26` | type | `Equal` | `src/utils/types.ts#Equal` (type_alias) | correct |
| E09 | `src/client/utils.test.ts:381:29` | type | `Equal` | `src/utils/types.ts#Equal` (type_alias) | correct |
| E10 | `src/helper/css/common.ts:34:6` | unknown | `CSS_ESCAPED` | `src/helper/css/common.ts#CSS_ESCAPED` (const) | correct |
| E11 | `src/helper/css/common.ts:118:54` | type | `CssVariableAsyncType` | `src/helper/css/common.ts#CssVariableAsyncType` (type_alias) | correct |
| E12 | `src/helper/css/index.test.tsx:140:12` | call | `Style` | `src/helper/css/index.ts#Style` (const) | correct |
| E13 | `src/helper/css/index.test.tsx:303:54` | call | `createCssContext` | `src/helper/css/index.ts#createCssContext` (function) | correct |
| E14 | `src/helper/html/index.test.ts:64:20` | call | `resolveCallback` | `src/utils/html.ts#resolveCallback` (function) | correct |
| E15 | `src/helper/ssg/ssg.test.tsx:389:30` | type | `AfterResponseHook` | `src/helper/ssg/ssg.ts#AfterResponseHook` (type_alias) | correct |
| E16 | `src/jsx/base.ts:204:10` | type | `Props` | `src/jsx/base.ts#Props` (type_alias) | correct |
| E17 | `src/jsx/dom/server.ts:6:15` | import | `HtmlEscapedString` | `src/utils/html.ts#HtmlEscapedString` (type_alias) | correct |
| E18 | `src/jsx/intrinsic-element/components.ts:181:54` | type | `Child` | `src/jsx/base.ts#Child` (type_alias) | correct |
| E19 | `src/jsx/streaming.test.tsx:764:18` | call | `Suspense` | `src/jsx/streaming.ts#Suspense` (function) | correct |
| E20 | `src/middleware/combine/index.ts:152:20` | unknown | `METHOD_NAME_ALL` | `src/router.ts#METHOD_NAME_ALL` (const) | correct |
| E21 | `src/middleware/csrf/index.test.ts:431:11` | call | `buildSimplePostRequestData` | `src/middleware/csrf/index.test.ts#buildSimplePostRequestData` (function) | correct |
| E22 | `src/middleware/csrf/index.test.ts:512:11` | call | `buildSimplePostRequestData` | `src/middleware/csrf/index.test.ts#buildSimplePostRequestData` (function) | correct |
| E23 | `src/middleware/jwt/jwt.ts:187:31` | unknown | `Jwt` | `src/utils/jwt/index.ts#Jwt` (const) | correct |
| E24 | `src/middleware/serve-static/index.ts:52:32` | unknown | `defaultJoin` | `src/middleware/serve-static/path.ts#defaultJoin` (function) | correct |
| E25 | `src/middleware/timeout/index.test.ts:19:9` | type | `HTTPException` | `src/http-exception.ts#HTTPException` (class) | correct |
| E26 | `src/middleware/timeout/index.ts:40:14` | type | `HTTPExceptionFunction` | `src/middleware/timeout/index.ts#HTTPExceptionFunction` (type_alias) | correct |
| E27 | `src/preset/tiny.ts:7:15` | import | `HonoOptions` | `src/hono-base.ts#HonoOptions` (type_alias) | correct |
| E28 | `src/router/trie-router/node.test.ts:53:22` | type | `Node` | `src/router/trie-router/node.ts#Node` (class) | correct |
| E29 | `src/types.test.ts:105:21` | type | `Expect` | `src/utils/types.ts#Expect` (type_alias) | correct |
| E30 | `src/types.test.ts:458:16` | type | `StatusCode` | `src/utils/http-status.ts#StatusCode` (type_alias) | correct |
| E31 | `src/types.test.ts:3318:21` | type | `Expect` | `src/utils/types.ts#Expect` (type_alias) | correct |
| E32 | `src/utils/buffer.test.ts:14:12` | call | `equal` | `src/utils/buffer.ts#equal` (function) | correct |
| E33 | `src/utils/cookie.ts:166:30` | call | `verifySignature` | `src/utils/cookie.ts#verifySignature` (function) | correct |
| E34 | `src/utils/jwt/jwt.test.ts:535:30` | call | `verify` | `src/utils/jwt/jwt.ts#verify` (function) | correct |
| E35 | `src/utils/jwt/jwt.ts:23:3` | import | `JwtTokenIssuedAt` | `src/utils/jwt/types.ts#JwtTokenIssuedAt` (class) | correct |
| E36 | `src/utils/jwt/jwt.ts:268:15` | type | `JwtTokenInvalid` | `src/utils/jwt/types.ts#JwtTokenInvalid` (class) | correct |
| E37 | `src/utils/types.ts:24:26` | type | `JSONPrimitive` | `src/utils/types.ts#JSONPrimitive` (type_alias) | correct |
| E38 | `src/utils/types.ts:26:46` | type | `JSONObject` | `src/utils/types.ts#JSONObject` (type_alias) | correct |
| E39 | `src/validator/validator.test.ts:984:5` | call | `validator` | `src/validator/validator.ts#validator` (function) | correct |
| E40 | `src/validator/validator.test.ts:1337:19` | type | `ContentfulStatusCode` | `src/utils/http-status.ts#ContentfulStatusCode` (type_alias) | correct |

### `scoped` sample

| # | Use | Kind | Name | Target | Verdict |
|---|---|---|---|---|---|
| S01 | `src/adapter/aws-lambda/handler.ts:449:18` | read | `path` | `src/adapter/aws-lambda/handler.ts#APIGatewayProxyEvent.path` (property) | correct |
| S02 | `src/adapter/aws-lambda/handler.ts:521:55` | read | `headers` | `src/adapter/aws-lambda/handler.ts#ALBProxyEvent.headers` (property) | correct |
| S03 | `src/client/fetch-result-please.ts:73:10` | write | `statusCode` | `src/client/fetch-result-please.ts#DetailedError.statusCode` (property) | correct |
| S04 | `src/helper/ssg/plugins.test.tsx:45:21` | read | `writeFile` | `src/helper/ssg/ssg.ts#FileSystemModule.writeFile` (method) | correct |
| S05 | `src/helper/ssg/ssg.test.tsx:981:14` | write | `files` | `src/helper/ssg/ssg.ts#ToSSGResult.files` (property) | correct |
| S06 | `src/hono-base.ts:134:16` | write | `#path` | `src/hono-base.ts#Hono.%23path` (property) | correct |
| S07 | `src/hono-base.ts:185:25` | read | `routes` | `src/hono-base.ts#Hono.routes` (property) | correct |
| S08 | `src/hono-base.ts:398:10` | read | `routes` | `src/hono-base.ts#Hono.routes` (property) | correct |
| S09 | `src/hono-base.ts:542:30` | call | `#dispatch` | `src/hono-base.ts#Hono.%23dispatch` (method) | correct |
| S10 | `src/jsx/index.test.tsx:51:21` | read | `children` | `src/jsx/index.test.tsx#SiteData.children` (property) | correct |
| S11 | `src/middleware/language/language.ts:302:26` | read | `cookieOptions` | `src/middleware/language/language.ts#DetectorOptions.cookieOptions` (property) | correct |
| S12 | `src/request.test.ts:125:24` | call | `param` | `src/request.ts#HonoRequest.param` (method) | correct |
| S13 | `src/request.test.ts:288:22` | call | `blob` | `src/request.ts#HonoRequest.blob` (method) | correct |
| S14 | `src/request.test.ts:428:24` | call | `arrayBuffer` | `src/request.ts#HonoRequest.arrayBuffer` (method) | correct |
| S15 | `src/request.test.ts:687:9` | read | `bodyCache` | `src/request.ts#HonoRequest.bodyCache` (property) | correct |
| S16 | `src/request.ts:191:10` | read | `raw` | `src/request.ts#HonoRequest.raw` (property) | correct |
| S17 | `src/request.ts:483:16` | read | `raw` | `src/request.ts#HonoRequest.raw` (property) | correct |
| S18 | `src/request.ts:512:15` | read | `raw` | `src/request.ts#HonoRequest.raw` (property) | correct |
| S19 | `src/router/common.case.test.ts:196:16` | call | `add` | `src/router.ts#Router.add` (method) | correct |
| S20 | `src/router/common.case.test.ts:291:16` | call | `add` | `src/router.ts#Router.add` (method) | correct |
| S21 | `src/router/common.case.test.ts:414:16` | call | `add` | `src/router.ts#Router.add` (method) | correct |
| S22 | `src/router/common.case.test.ts:997:16` | call | `add` | `src/router.ts#Router.add` (method) | correct |
| S23 | `src/router/pattern-router/router.test.ts:33:14` | call | `add` | `src/router/pattern-router/router.ts#PatternRouter.add` (method) | correct |
| S24 | `src/router/reg-exp-router/prepared-router.ts:15:10` | write | `#matchers` | `src/router/reg-exp-router/prepared-router.ts#PreparedRegExpRouter.%23matchers` (property) | correct |
| S25 | `src/router/reg-exp-router/router.test.ts:71:14` | call | `add` | `src/router/reg-exp-router/router.ts#RegExpRouter.add` (method) | correct |
| S26 | `src/router/reg-exp-router/router.test.ts:115:16` | call | `add` | `src/router/reg-exp-router/router.ts#RegExpRouter.add` (method) | correct |
| S27 | `src/router/reg-exp-router/router.test.ts:208:18` | call | `add` | `src/router/reg-exp-router/router.ts#RegExpRouter.add` (method) | correct |
| S28 | `src/router/reg-exp-router/router.ts:77:12` | read | `#tries` | `src/router/reg-exp-router/router.ts#RegExpRouter.%23tries` (property) | correct |
| S29 | `src/router/smart-router/router.ts:18:10` | read | `#routes` | `src/router/smart-router/router.ts#SmartRouter.%23routes` (property) | correct |
| S30 | `src/router/trie-router/node.test.ts:33:8` | call | `insert` | `src/router/trie-router/node.ts#Node.insert` (method) | correct |
| S31 | `src/router/trie-router/node.test.ts:167:8` | call | `insert` | `src/router/trie-router/node.ts#Node.insert` (method) | correct |
| S32 | `src/router/trie-router/node.test.ts:258:24` | call | `search` | `src/router/trie-router/node.ts#Node.search` (method) | correct |
| S33 | `src/router/trie-router/node.test.ts:345:26` | call | `search` | `src/router/trie-router/node.ts#Node.search` (method) | correct |
| S34 | `src/router/trie-router/node.test.ts:371:10` | call | `insert` | `src/router/trie-router/node.ts#Node.insert` (method) | correct |
| S35 | `src/router/trie-router/node.test.ts:562:10` | call | `insert` | `src/router/trie-router/node.ts#Node.insert` (method) | correct |
| S36 | `src/router/trie-router/node.test.ts:601:26` | call | `search` | `src/router/trie-router/node.ts#Node.search` (method) | correct |
| S37 | `src/router/trie-router/node.test.ts:666:10` | call | `insert` | `src/router/trie-router/node.ts#Node.insert` (method) | correct |
| S38 | `src/utils/stream.test.ts:27:24` | read | `responseReadable` | `src/utils/stream.ts#StreamingApi.responseReadable` (property) | correct |
| S39 | `src/utils/stream.ts:72:18` | read | `writer` | `src/utils/stream.ts#StreamingApi.writer` (property) | correct |
| S40 | `src/utils/stream.ts:83:12` | write | `writer` | `src/utils/stream.ts#StreamingApi.writer` (property) | correct |

### Unresolved member calls

| # | Use | Member | Receiver | Why unresolved | Note |
|---|---|---|---|---|---|
| U01 | `runtime-tests/bun/index.test.tsx:37:27` | `request` | `app` | re-export | `Hono` imported from `../../src/index`, which exports its import binding of `Hono` |
| U02 | `runtime-tests/bun/index.test.tsx:235:22` | `text` | `res` | untyped receiver | `res = await app.request(...)` |
| U03 | `runtime-tests/deno/hono.test.ts:14:7` | `get` | `app` | inherited member | `Hono` from `../../src/hono.ts`; `get` is declared on the base class (`src/hono-base.ts`) |
| U04 | `runtime-tests/lambda/index.test.ts:253:40` | `toBeUndefined` | `expect(response.multiValueHeaders)` | other: call result |  |
| U05 | `src/adapter/cloudflare-workers/websocket.test.ts:61:9` | `get` | `app` | re-export | `Hono` imported from `../..`, which names `src/index.ts`, an exported import binding |
| U06 | `src/context.test.ts:11:24` | `set` | `Reflect` | class-name receiver | global `Reflect` |
| U07 | `src/helper/cookie/index.test.ts:238:16` | `text` | `c` | untyped receiver | callback parameter `c` |
| U08 | `src/hono.test.ts:486:30` | `toBe` | `expect(await res.text())` | other: call result |  |
| U09 | `src/hono.test.ts:1454:26` | `toBe` | `expect(res.status)` | other: call result |  |
| U10 | `src/hono.test.ts:1809:26` | `text` | `c` | untyped receiver | callback parameter `c` |
| U11 | `src/hono.test.ts:2678:32` | `toBe` | `expect(await res.text())` | other: call result |  |
| U12 | `src/hono.test.ts:2947:27` | `header` | `c.req` | chained property | `c.req` |
| U13 | `src/jsx/base.test.tsx:21:38` | `toBe` | `expect(clonedElement.toString())` | other: call result |  |
| U14 | `src/jsx/dom/client.test.tsx:49:10` | `render` | `root` | untyped receiver | `root` from a factory call |
| U15 | `src/jsx/dom/intrinsic-element/components.test.tsx:618:41` | `toBe` | `expect(document.head.innerHTML)` | other: call result |  |
| U16 | `src/jsx/index.test.tsx:307:88` | `toString` | `(<span dangerouslySetInnerHTML={{ __h...` | other: expression (JSX element) |  |
| U17 | `src/jsx/index.test.tsx:762:35` | `toBe` | `expect(template.toString())` | other: call result |  |
| U18 | `src/jsx/streaming.test.tsx:583:74` | `mockImplementation` | `vi.spyOn(ReadableStreamDefaultControl...` | other: call result |  |
| U19 | `src/middleware/basic-auth/index.test.ts:345:34` | `get` | `c` | untyped receiver | callback parameter `c` |
| U20 | `src/middleware/basic-auth/index.test.ts:347:31` | `from` | `Buffer` | class-name receiver | global `Buffer` |
| U21 | `src/middleware/cache/index.test.ts:144:7` | `use` | `app` | inherited member | `use` declared on the base class |
| U22 | `src/middleware/cache/index.test.ts:396:46` | `toBe` | `expect(res.headers.get('cache-control'))` | other: call result |  |
| U23 | `src/middleware/cache/index.test.ts:990:44` | `text` | `(await request('globex'))` | other: call result |  |
| U24 | `src/middleware/cors/index.test.ts:356:37` | `toBeNull` | `expect(res.headers.get('Vary'))` | other: call result |  |
| U25 | `src/middleware/jwk/index.test.ts:503:16` | `json` | `c` | untyped receiver | callback parameter `c` |
| U26 | `src/middleware/language/index.test.ts:221:29` | `request` | `app` | untyped receiver | `app = createTestApp(...)` |
| U27 | `src/middleware/logger/index.test.ts:97:27` | `request` | `app` | inherited member | typed `Hono`; `request` declared on the base class |
| U28 | `src/middleware/logger/index.ts:18:28` | `map` | `times` | other: non-class annotation | `times: string[]` |
| U29 | `src/middleware/method-not-allowed/index.test.ts:90:24` | `has` | `res.headers` | chained property | `res.headers` |
| U30 | `src/middleware/secure-headers/index.test.ts:29:49` | `toEqual` | `expect(res.headers.get('X-XSS-Protect...` | other: call result |  |
| U31 | `src/middleware/trailing-slash/index.test.ts:36:28` | `toBe` | `expect(loc.pathname)` | other: call result |  |
| U32 | `src/router/common.case.test.ts:137:28` | `toBe` | `expect(res.length)` | other: call result |  |
| U33 | `src/router/common.case.test.ts:626:34` | `toEqual` | `expect(res[3].handler)` | other: call result |  |
| U34 | `src/router/common.case.test.ts:853:28` | `toBe` | `expect(res.length)` | other: call result |  |
| U35 | `src/router/reg-exp-router/router.test.ts:185:72` | `toBe` | `expect((stash as ParamStash)[(res[0][...` | other: call result |  |
| U36 | `src/router/trie-router/node.test.ts:671:25` | `toEqual` | `expect(res[0][0])` | other: call result |  |
| U37 | `src/router/trie-router/node.test.ts:778:23` | `toEqual` | `expect(res[1][1])` | other: call result |  |
| U38 | `src/types.test.ts:1874:34` | `toEqualTypeOf` | `expectTypeOf(c.var.foo5)` | other: call result |  |
| U39 | `src/utils/url.ts:126:24` | `slice` | `url` | untyped receiver | `url = request.url` |
| U40 | `src/validator/validator.test.ts:280:27` | `request` | `app` | inherited member | `request` declared on the base class |

| Why unresolved (sample of 40) | Count | Share |
|---|---:|---:|
| other: call or expression result as receiver (`expect(x).toBe()`, a JSX element) | 21 | 52.5% |
| untyped receiver | 8 | 20.0% |
| inherited member | 4 | 10.0% |
| re-export | 2 | 5.0% |
| class-name receiver | 2 | 5.0% |
| chained property | 2 | 5.0% |
| other: non-class annotation (`string[]`) | 1 | 2.5% |
| qualified type annotation | 0 | 0% |
| package import | 0 | 0% |
| path alias | 0 | 0% |

Over all 19,784, by receiver shape (mechanical; `--audit` prints it): call or
other expression 8,490 (42.9%); identifier with no hint 5,060 (25.6%); `new`
hint with no own member bound 2,506 (12.7%); property chain 1,909 (9.6%);
capitalized identifier with no hint 960 (4.9%); annotation hint with no own
member bound 853 (4.3%); annotation with a qualified type name 5 (0.03%);
`this` with no own member 1. For scale, of 20,508 member calls through a
receiver, 607 (3.0%) bind `scoped` and 117 (0.6%) bind `exact` (namespace-import
members).

The five qualified annotations are all `NodeJS.WritableStream`
(`src/adapter/aws-lambda/handler.ts` and `runtime-tests/lambda/mock.ts`), a
global namespace from `@types/node`. The two conservative gaps the T45 review
found, a receiver typed `ns.Inner.Foo` (deeper than one namespace-import
member) and one typed `Local.Foo` through a same-file `namespace Local`, occur
0 times in Hono.

## Known limitations on real code

The TypeScript adapter README ("Known limitations on real code (T46)") has
the tables; in short:

1. **Grammar failures (8 of 357 eligible files, 2.2%).** The pinned
   `tree-sitter-typescript` 0.23.2 rejects consecutive generic call
   signatures separated only by a newline (6 files) and `export type * from`
   (2 files). Minimal forms:

   ```ts
   interface G {
     <K extends string>(key: K): number
     <K extends number>(key: K): string
   }
   ```

   (a `;` after the first signature parses) and `export type * from './x'`.
   `src/context.ts` (the `Context` class) and `src/types.ts` are among them,
   so the common `c: Context` receiver never binds, and 299 of 1,634 named and
   default imports (18.3%) point into a failed module.
2. **Imports (1,634 named and default):** 971 bound (59.4%); 299 into a
   parse-failed module; 140 bare `.` or `..` specifiers (8.6%), which are not
   `./` or `../` and so are not relative specifiers under the module rule; 124
   packages or built-ins (7.6%); 76 re-exported (4.7%) and 20 exported import
   bindings (1.2%); 3 other; 1 with no candidate module; no path aliases.
3. **Member calls:** 96.5% stay `name_match`, mostly because the receiver is
   a call result or an untyped name; inheritance (`new Hono()` whose methods
   live on the base class) is the largest share among hinted receivers.

## PF2 addendum (2026-09-24): `--no-refresh` speed and cached diagnostics

Wall clock of one process on the pinned Hono checkout (a copy, warm cache,
release build), median of 5 runs, query
`src/request.ts#HonoRequest.queries --json`. The two binaries were measured
back to back, alternating three times; the third pair is shown, and the other
two agree within about 10 ms.

| Command | Before PF2 | After PF2 |
|---|---:|---:|
| `index` (no change) | 45 ms | 45 ms |
| `symbol` | 171 ms | 170 ms |
| `symbol --no-refresh` | 242 ms | 131 ms |
| `refs` | 168 ms | 169 ms |
| `refs --no-refresh` | 241 ms | 128 ms |
| `context` | 180 ms | 182 ms |
| `context --no-refresh` | 248 ms | 138 ms |

Cause: building the `cached` report decoded every stored use (one query per
file, 92,303 rows), every symbol, every binding, and every file's source blob
only to count them, about 112 ms. It now uses `COUNT(*)` and a file-status
query that reads no source, so a cached query costs a refreshed one minus the
refresh (about 40 ms).

Where a refreshed query's ~170 ms goes (instrumented locally, not committed):

| Phase | Time |
|---|---:|
| process start, root discovery, config, store open | ~2 ms |
| refresh: load inventory 1 ms, walk 5 ms, read and hash 9 ms | ~15 ms |
| refresh: reparse the 8 files that failed to parse (`src/types.ts` alone 12.5 ms) | ~19 ms |
| refresh: stage 1 ms, recheck walk 5 ms, commit | ~7 ms |
| query: load every use (`references::all_uses`) | ~100 ms |
| query: bindings map 2 ms, evidence 5 ms, matching and JSON ~10 ms | ~17 ms |

The use load is the largest share: SQLite's ordered scan over the uses index
takes about 35 ms and decoding 92,303 rows into `UseRow` (columns looked up by
name, seven heap strings per row) about 65 ms. Every `symbol`, `refs`, and
`context` query does it, refreshed or not. Content-mode refreshes also reparse
every failed file each time, because only an `ok` file's facts are reused.

Cached diagnostics: a refresh now persists each failed file's parser
diagnostic in the existing `diagnostics` table (no schema change), so a
`--no-refresh` answer and a metadata-mode refresh report `} at byte 2885`
rather than `stored parse error`, and size, binary, and encoding skips report
the same detail a content refresh does. Cached answers on Hono now differ from
refreshed ones only in `freshness`.
