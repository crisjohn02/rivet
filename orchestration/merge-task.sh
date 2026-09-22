#!/usr/bin/env bash
# Merge a reviewed task branch into main, then verify main.
#
#   orchestration/merge-task.sh T20
#
# On conflict it stops with the conflicted paths listed; resolve, `git add`,
# `git commit`, then re-run the verification below by hand.
set -u
T="${1:?usage: merge-task.sh <task-id>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1
command -v cargo >/dev/null || export PATH="$HOME/.cargo/bin:$PATH"
git diff --quiet && git diff --cached --quiet || { echo "main has uncommitted changes"; exit 1; }
if git merge --no-ff --no-edit "task/$T" -m "Merge task/$T"; then
  echo "merged task/$T: $(git log --oneline -1)"
else
  echo "CONFLICTS:"; git diff --name-only --diff-filter=U; exit 2
fi
echo "== verifying merged main =="
cargo fmt --all -- --check && echo "fmt ok"
cargo clippy --workspace --all-targets --all-features -- -D warnings || exit 1
cargo test --workspace || exit 1
python3 tests/gold/check_gold.py | tail -1
echo "Now: git push, then git worktree remove ../rivet-wt/$T && git branch -d task/$T"
