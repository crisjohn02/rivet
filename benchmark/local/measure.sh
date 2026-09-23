#!/usr/bin/env bash
# T37: measure rivet itself on the pinned private corpus (no model, no agent).
#
#   RIVET_CORPUS_DIR=/path/to/corpus benchmark/local/measure.sh
#
# RIVET_CORPUS_DIR is required and has no default; it must hold the clean
# copies made by benchmark/fetch-corpus.sh. Each project is measured in a
# scratch copy under a temporary directory, which is deleted afterwards, so the
# corpus copies are never edited. Optional: RIVET_RUNS (default 7, at least 5),
# RIVET_PROJECTS (default fluent,timesheet), RIVET_RESULTS_DIR (default
# benchmark/results/T37-local), CARGO_TARGET_DIR.
#
# Writes results.json only: counts, sizes, times, commits, and the generic
# query names chosen from a fixed list. See measure.py for what is recorded.
set -euo pipefail

if [[ -z "${RIVET_CORPUS_DIR:-}" ]]; then
  echo "measure.sh: set RIVET_CORPUS_DIR to the corpus directory (no default)" >&2
  exit 2
fi
if [[ ! -d "$RIVET_CORPUS_DIR" ]]; then
  echo "measure.sh: RIVET_CORPUS_DIR is not a directory" >&2
  exit 2
fi

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
export PATH="$HOME/.cargo/bin:$PATH"
(cd "$repo" && cargo build --release --quiet)
target_dir="${CARGO_TARGET_DIR:-$repo/target}"
export RIVET_BIN="$target_dir/release/rivet"
if [[ ! -x "$RIVET_BIN" ]]; then
  echo "measure.sh: no release binary at $RIVET_BIN" >&2
  exit 1
fi

exec python3 "$repo/benchmark/local/measure.py"
