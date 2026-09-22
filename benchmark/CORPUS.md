# Benchmark corpus

Selected by T35, which ran no benchmark. The machine-readable pins are in
[corpus.toml](corpus.toml). T37 measures rivet on this corpus.

## What was chosen, and why

The corpus is **two private Laravel applications**, pinned to exact commits:

| Name | Role in the corpus | Licence |
|---|---|---|
| `fluent` | The PHP project T37 measures: a large Laravel application with substantial first-party code | proprietary, not published |
| `timesheet` | A smaller Laravel + Vue 3 application: the only TypeScript in the corpus, and a second PHP project | proprietary, not published |

The user chose them, replacing the public candidates below, for these reasons:

- **They are real Laravel applications**, which is the kind of code the user
  works with. Their first-party code stays realistic and sizeable after
  rivet's mandatory `vendor/` exclusion. `fluent` has about 1,220 indexable
  PHP files and many methods that share a name across classes, so it
  exercises ambiguity and name-only matching.
- **They are unlikely to have been memorized.** Neither repository has ever
  been public, so no model could have trained on it. docs/BENCHMARK.md warns
  that low popularity alone does not establish this. Never having been
  published does.

This file does not describe the projects' contents, because they are
proprietary and this repository is public. Only the name, the commit,
per-extension file counts, and rivet's index results are recorded.

### Known limitations

- **TypeScript is thin.** `timesheet` has 48 `.ts` files, 2 `.d.ts` files, and
  no `.tsx`. Its front end is mostly `.vue` single-file components, which
  rivet's TypeScript plan does not support (docs/ADDING-A-LANGUAGE.md "MVP
  support boundary"). TypeScript extraction does not exist yet: it is
  deferred behind Gate G, so the TypeScript side cannot be measured until T41
  onward. T41 should decide whether the benchmark needs a further TS/TSX
  project to get balanced language coverage.
- **The licences are not permissive.** docs/BENCHMARK.md asks for
  "permissively licensed projects" so that results, corpus pins, and tasks
  can be published. With a private corpus, the pins stay reproducible only
  for someone with access to the source repositories, and task text and gold
  data cannot be published verbatim. Publishing results therefore needs a
  redaction policy, decided before pre-registration.

## Pilot and held-out separation

A pilot / held-out partition exists. It is recorded privately in
`partition.md` in the private gold directory (`$RIVET_PRIVATE_GOLD_DIR`),
because naming its parts would reveal internal structure. Pilot tasks (T38)
may draw only on the pilot partition. The split is by repository, which gives
the strongest separation: tuning on the pilot cannot see the held-out code.

## Public candidates considered first

Before the user redirected T35 to the private projects, these public projects
were cloned read-only and indexed with rivet at commit `360c13b`. Nothing in
them was executed. They are recorded so the selection can be audited.

| Candidate | Licence (read from the licence file) | PHP / TS files | Result |
|---|---|---|---|
| koel/koel | MIT (`LICENSE.md`) | 1,465 PHP indexed of 2,828 seen | Best public Laravel application; superseded by the private corpus |
| BookStackApp/BookStack | MIT (`LICENSE`) | 1,834 PHP indexed, 1 parse error | Laravel application, but 689 of its PHP files are translations and 325 are Blade templates, and its tests need MySQL; superseded |
| bagisto/bagisto | MIT (`LICENSE`) | 2,759 PHP indexed | Rejected: only 3 PHP files in `app/`; the code lives in modular packages, not the Laravel application layout |
| flarum/framework | MIT (`LICENSE.md`) | 1,667 PHP indexed, 2 parse errors | Rejected: not a Laravel application (a framework monorepo) |
| thephpleague/commonmark | BSD-3-Clause (`LICENSE`) | 498 PHP indexed | Rejected: a library, not a Laravel application |
| wallabag/wallabag | MIT (`COPYING.md`) | 382 PHP indexed | Rejected: Symfony, and small |
| kimai/kimai | AGPL-3.0 (`LICENSE`) | not indexed | Rejected: copyleft licence |
| firefly-iii, monica, snipe-it, pixelfed, linkstack | AGPL-3.0 (GitHub metadata; not cloned) | not indexed | Rejected: copyleft licence |
| invoiceninja, akaunting | not a recognized open-source licence (GitHub metadata; not cloned) | not indexed | Rejected: not permissive |
| excalidraw/excalidraw | MIT (`LICENSE`, matches the badge) | 659 TS/TSX | Public TS candidate; superseded |
| actualbudget/actual | MIT (`LICENSE.txt`) | 1,986 TS/TSX | Public TS candidate; superseded |
| umami-software/umami | MIT (`LICENSE`) | 1,375 TS/TSX | Public TS candidate; superseded |

## How to recreate the copies

The projects have no public URL. `benchmark/fetch-corpus.sh` makes a clean
copy from a local source repository that you supply. For each project it
clones without hardlinks, checks out the pinned commit detached, removes the
`origin` remote, and deletes every tracked file whose name starts with
`.env`. It then verifies the copy, and runs nothing from the repository.

```bash
export RIVET_CORPUS_DIR=/path/outside/this/repo/corpus          # required, no default
export RIVET_CORPUS_SOURCE_FLUENT=/path/to/fluent/source         # needed only for a new copy
export RIVET_CORPUS_SOURCE_TIMESHEET=/path/to/timesheet/source
benchmark/fetch-corpus.sh              # or: benchmark/fetch-corpus.sh fluent
```

The script refuses a destination inside this repository. Rerunning it is safe:
an existing copy is re-verified and left untouched. It fails if HEAD is not
the pin, if a remote remains, or if any tracked file differs other than the
deleted env files. Running rivet over a copy creates `.rivet/` inside it,
which is untracked and allowed.

## Private gold sample

The hand-verified PHP gold sample (T35) lives outside this repository, in
`$RIVET_PRIVATE_GOLD_DIR/fluent.toml`. It uses the field format of
`tests/gold/php-authored.toml`. `python3 tests/gold/check_gold.py` verifies it
against `$RIVET_CORPUS_DIR/fluent/` when both variables are set and the copy
exists. Otherwise it skips the sample with a message. The 51 authored entries
are always checked.
