# T37: local measurements on the pinned corpus

rivet measured on itself, with no model calls and no agent: the benchmark's
first, free stage. Raw numbers are in [results.json](results.json); the
measurement script is [benchmark/local/measure.sh](../../local/measure.sh).

The corpus is two private Laravel applications, `fluent` and `timesheet`. This
report records only names, commits, counts, sizes and times. Query targets are
chosen from a fixed list of generic method names such as `handle`, `get`,
`create` and `update`.

## Environment

| Field | Value |
|---|---|
| Machine | Mac15,6, Apple M3 Pro, 12 cores, 18 GiB |
| OS | macOS 27.0 (26A428) |
| Rust | rustc 1.98.1 (48a229cea 2026-09-01), `cargo build --release` |
| rivet commit measured | `fa97020`, sources clean |
| Corpus pins | fluent `fc96ad75ea658e60df52dbbb8543077b0b931d14`, timesheet `17c26bb26099ef24bf750da7cdec8a3445081a04` |
| OS file cache | Warm. Each project was indexed once, untimed, before any timed run. "Cold index" means no `.rivet/`, not a cold file cache. |
| Load | 1-minute load average 2.2 at the start and 2.6 at the end; an unrelated process used about one core. rivet is single-threaded. |
| Runs | 7 per measurement. Times are median [min–max] in ms. |
| Timing | Wall time of the whole `rivet` process, including start-up, around `/usr/bin/time -l`. Peak memory is its maximum resident set size. |

## What rivet indexed

| | fluent | timesheet |
|---|---|---|
| Files seen / indexed | 2,815 / 1,219 | 691 / 250 |
| Skipped | 1,595 unsupported, 1 parse error | 441 unsupported |
| Symbols / uses / bindings | 11,336 / 125,299 / 30,751 | 1,177 / 15,610 / 2,448 |
| PHP read and hashed on a content-mode refresh | 1,220 files, 11,084,083 bytes | 250 files, 950,116 bytes |
| Index size on disk | 107,065,344 bytes (9.7× the PHP) | 9,748,480 bytes (10.3×) |

## Index and refresh

| Measurement | fluent | fluent RSS | timesheet | timesheet RSS |
|---|---|---|---|---|
| Cold index (`.rivet/` deleted first) | 3,167 [3,069–3,295] | 218 MB | 298 [295–312] | 36 MB |
| No-change `index`, content mode | 87 [87–224] | 36 MB | 23 [23–27] | 13 MB |
| No-change `index`, metadata mode | 55 [54–55] | 27 MB | 18 [17–18] | 12 MB |
| `index --force`, no change | 3,627 [3,555–3,731] | 222 MB | 325 [323–329] | 36 MB |
| Edit one file, then `index` (content) | 1,208 [1,175–1,222] | 210 MB | 91 [88–95] | 32 MB |
| Edit one file, then `index` (metadata) | 1,175 [1,146–1,204] | 206 MB | 84 [83–85] | 32 MB |
| Edit one file, then `refs` (content) | 1,389 [1,356–1,403] | 209 MB | 113 [113–114] | 36 MB |
| Edit one file, then `refs` (metadata) | 1,339 [1,326–1,358] | 206 MB | 108 [107–110] | 35 MB |

The edit appends a PHP line comment to the query target's own file, different
on every run. Each refresh updated exactly one file.

## Queries with no change

The target is the common-name declaration chosen below.

| Query | Freshness | fluent | fluent RSS | timesheet | timesheet RSS |
|---|---|---|---|---|---|
| `symbol` | content (default) | 275 [273–277] | 103 MB | 48 [48–51] | 22 MB |
| `symbol` | metadata | 237 [234–241] | 94 MB | 43 [42–43] | 21 MB |
| `symbol` | `--no-refresh` | 378 [374–381] | 91 MB | 55 [55–56] | 19 MB |
| `refs` | content | 272 [267–273] | 102 MB | 48 [47–50] | 22 MB |
| `refs` | metadata | 239 [234–240] | 94 MB | 43 [42–45] | 21 MB |
| `refs` | `--no-refresh` | 373 [370–378] | 91 MB | 55 [55–56] | 19 MB |
| `context` | content | 325 [321–328] | 113 MB | 53 [52–56] | 23 MB |
| `context` | metadata | 295 [290–298] | 105 MB | 48 [47–48] | 22 MB |
| `context` | `--no-refresh` | 432 [428–444] | 103 MB | 59 [58–60] | 21 MB |

Every query printed identical output on all 7 runs, and every `--json` run left
stderr empty.

## Hashing versus resolution

Derived by subtracting medians; no phase timing was added, so output is
unchanged.

| Cost | Derived as | fluent | timesheet |
|---|---|---|---|
| Hashing all PHP content | no-change content `index` − no-change metadata `index` | 32 ms | 5 ms |
| Reparse one file + full re-resolution | one-file-edit content `index` − no-change content `index` | 1,121 ms | 68 ms |
| Same, metadata mode | one-file-edit metadata `index` − no-change metadata `index` | 1,120 ms | 66 ms |
| `--force` over an existing index | `--force` − cold index | 460 ms | 27 ms |
| Query work beyond a no-change refresh | metadata `refs` − no-change metadata `index` | 184 ms | 25 ms |

Hashing is cheap, about 350 MB/s including reads. Subtraction cannot split the
1.12 s edit cost between reparsing and re-resolution; that needs phase timing.

## Common-name `refs`, with `rg` beside it

From a fixed list of generic names, take the one with at least 5 declarations
whose best single declaration has the most references.

| | fluent | timesheet |
|---|---|---|
| Name | `create`, 30 declarations | `update`, 6 declarations |
| `refs` total, references mode | 922 (exact 0, scoped 2, name_match 920) | 69 (name_match 69) |
| `refs` total, `--mode candidates` | 933 (exact 0, scoped 2, name_match 931) | 69 (name_match 69) |
| `refs` time, content mode | 268 [265–283] ms | 48 [47–49] ms |
| `rg -n -w <name> .` matched lines | 2,262 in 47 [45–57] ms | 276 in 19 [16–25] ms |
| `rg -n -w <name> -t php .` matched lines | 1,602 in 27 [26–29] ms | 151 in 11 [10–12] ms |

The `rg` rows are context, not a like-for-like comparison. `rg` also matches
declarations, comments, strings, unrelated same-name uses and non-PHP files,
and skips hidden files (spec §11.2). Only counts were kept.

## High fanout

| Query | fluent | timesheet |
|---|---|---|
| Ambiguous `symbol <name> --limit 1000` (exit 5) | `handle`: 110 candidates, 90 [89–94] ms | `handle`: 13 candidates, 23 [23–23] ms |
| `context` exploring the most candidates | `get`: 11 segments, 734 omitted for budget, 745 explored, `candidate_limit_reached: false`, 327 [325–330] ms | `update`: 11 segments, 56 omitted, 67 explored, `false`, 53 [52–54] ms |
| `symbol` on that target | 1,511 callers, 272 [270–272] ms | 69 callers, 48 [48–49] ms |

## Output size: JSON against text

"Source" is the source text a response carries. "Envelope" is the share of
JSON bytes that is not source. Estimates are rivet's own `ceil(bytes / 3)`,
not a real tokenizer's count; a real tokenizer comparison is T47.

### `context` by budget

| Project, target, `--tokens` | Segments | rivet's source estimate | Source bytes | JSON bytes | Text bytes | JSON envelope | Text non-source |
|---|---|---|---|---|---|---|---|
| fluent common 1k | 4 | 999 | 2,995 | 6,323 | 3,806 | 53% | 21% |
| fluent common 4k | 14 | 4,000 | 11,983 | 21,463 | 14,258 | 44% | 16% |
| fluent common 16k | 34 | 15,998 | 47,962 | 70,230 | 53,071 | 32% | 10% |
| fluent common 64k | 50 | 22,103 | 66,261 | 98,248 | 73,585 | 33% | 10% |
| fluent fanout 1k | 9 | 1,000 | 2,995 | 9,854 | 4,738 | 70% | 37% |
| fluent fanout 4k | 11 | 3,997 | 11,983 | 20,370 | 14,022 | 41% | 15% |
| fluent fanout 16k and 64k | 50 | 13,530 | 40,538 | 76,518 | 49,548 | 47% | 18% |
| timesheet 1k | 10 | 991 | 2,962 | 16,923 | 4,588 | 83% | 35% |
| timesheet 4k | 11 | 3,995 | 11,974 | 26,656 | 13,669 | 55% | 12% |
| timesheet 16k | 28 | 15,993 | 47,959 | 72,783 | 51,818 | 34% | 7% |
| timesheet 64k | 50 | 20,965 | 62,856 | 99,548 | 69,295 | 37% | 9% |

### Same query in both forms

| Project, target | Query | JSON bytes | Text bytes | Text / JSON | Source bytes | JSON envelope | rivet's estimate, JSON / text | Hashes in JSON |
|---|---|---|---|---|---|---|---|---|
| fluent common | `symbol` | 54,383 | 12,629 | 23% | 0 | 100% | 18,128 / 4,210 | 146 |
| fluent common | `symbol --source` | 60,781 | 18,891 | 31% | 6,252 | 90% | 20,261 / 6,297 | 146 |
| fluent common | `refs`, page of 50 | 38,180 | 8,325 | 22% | 0 | 100% | 12,727 / 2,775 | 102 |
| fluent common | `refs --limit 1000` (922) | 710,010 | 224,370 | 32% | 0 | 100% | 236,670 / 74,790 | 1,770 |
| fluent common | `context --tokens 4000` | 21,463 | 14,258 | 66% | 11,983 | 44% | 7,155 / 4,753 | 16 |
| fluent fanout | `symbol` | 41,897 | 9,014 | 22% | 0 | 100% | 13,966 / 3,005 | 102 |
| fluent fanout | `symbol --source` | 42,013 | 9,125 | 22% | 101 | 100% | 14,005 / 3,042 | 102 |
| fluent fanout | `refs`, page of 50 | 41,720 | 8,988 | 22% | 0 | 100% | 13,907 / 2,996 | 102 |
| fluent fanout | `refs --limit 1000` (1,000 of 1,511) | 726,779 | 187,350 | 26% | 0 | 100% | 242,260 / 62,450 | 1,799 |
| fluent fanout | `context --tokens 4000` | 20,370 | 14,022 | 69% | 11,983 | 41% | 6,790 / 4,674 | 13 |
| timesheet | `symbol` | 57,292 | 10,947 | 19% | 0 | 100% | 19,098 / 3,649 | 134 |
| timesheet | `symbol --source` | 59,013 | 12,623 | 21% | 1,666 | 97% | 19,671 / 4,208 | 134 |
| timesheet | `refs`, page of 50 | 45,127 | 8,204 | 18% | 0 | 100% | 15,043 / 2,735 | 102 |
| timesheet | `refs --limit 1000` (69) | 56,121 | 12,874 | 23% | 0 | 100% | 18,707 / 4,292 | 131 |
| timesheet | `context --tokens 4000` | 26,656 | 13,669 | 51% | 11,974 | 55% | 8,886 / 4,557 | 13 |

On the authored fixture the `context` envelope was 88%. On these real projects
it is 41–55% at the default 4,000 tokens, 32–47% at 16,000, and 53–83% at
1,000. The text form carries the same source in 51–69% of the JSON bytes for
`context`, and in 18–32% for `symbol` and `refs`.

## Against spec §24's aspirational targets

Spec §24's targets are for a 5,000-file repository and are explicitly
aspirations, not release claims. fluent has 2,815 files.

| Target | Aspiration | fluent |
|---|---|---|
| Metadata-mode freshness, no changes | < 50 ms | 55 ms |
| Cached symbol query, plus freshness | < 20 ms | ~180 ms beyond the refresh |
| Cached reference query, plus freshness | < 50 ms | ~185 ms beyond the refresh |
| Context query, plus freshness | < 150 ms | ~240 ms beyond the refresh |
| Incremental reindex, one file changed | < 100 ms | 1,208 ms |

## Follow-ups (recorded, not optimised)

1. **`--no-refresh` is slower than refreshing.** On fluent a cached `refs`
   takes 373 ms against 272 ms with a refresh. From reading the code, not from
   profiling, the likely cause is that `cached_report` loads every stored use
   row, 125,299 of them, just to count them.
2. **Query work is far above spec §24's targets.** About 180 ms per `symbol`
   or `refs` on fluent after the refresh. The ambiguous-symbol answer costs
   only about 3 ms beyond its refresh, so the time goes into loading
   references and calls. Opt-in phase timing, printed to stderr only, would
   locate it.
3. **A one-file edit costs a third of a cold index** (1.21 s against 3.17 s).
   This is the documented full re-resolution trade-off and should be split by
   phase before any selective-invalidation proposal.
4. **`--force` is 460 ms slower than a cold index** on fluent.
5. **The index is about 10× the PHP source.** Cold-index peak memory is
   218 MB, and a query's peak memory, 91–113 MB, is close to the whole index.
6. **One outlier:** a no-change content refresh took 224 ms against a median
   of 87 ms, the first refresh after the untimed target selection.
7. **`candidate_limit_reached` never fired.** The most explored was 745
   candidates, under the cap of 1,000.
8. **The default `--limit 50` stops `context` before large budgets are used.**
   At 64k tokens a response returns only 13–22k estimated tokens, with no
   error. This is per contract, but relevant to the snippet and to T38's tasks.
9. **Hashes are a large share of list output.** A 50-item `refs` page carries
   102 `blake3:` hashes, about 7 KB of 38–45 KB.
10. **Almost every common-name reference is `name_match`** (920 of 922; 69 of
    69). The labelling is honest, but it means rivet's references for common
    Laravel method names are mostly unresolved. T38's tasks should expect it.

## Independent verification by the orchestrator

Re-measured on the same corpus copy after the run, release build:

| Check | T37 | Re-measured |
|---|---|---|
| No-change `index`, content | 87 ms | 79 ms |
| No-change `index`, metadata | 55 ms | 49 ms |
| `handle` declarations on fluent | 110 | 110 |
| `create` declarations on fluent | 30 | 30 |
| Best `create` declaration's refs | 922 (exact 0, scoped 2, name_match 920) | 922 (exact 0, scoped 2, name_match 920) |
| `rg -n -w create -t php .` lines | 1,602 | 1,602 |
| Index size on disk | 107,065,344 bytes | 107,065,344 bytes |

The timing differences are within normal variation, on a quieter machine. A
privacy audit checked every committed file against all class, method,
namespace and path names from both projects' indexes. The only distinctive
match was `toArray`, which is in the script's fixed list of generic Laravel
method names rather than taken from either project.
