#!/usr/bin/env bash
# Run one task in its own git worktree, for tasks that are independent of other
# in-flight tasks. Creates <repo>/../rivet-wt/<task> on branch task/<task>.
#
#   orchestration/run-task-worktree.sh T20
#
# Each worktree builds into its own target/ so concurrent sessions never hand
# each other a stale binary. Merge with orchestration/merge-task.sh.
set -u
NAME="${1:?usage: run-task-worktree.sh <task-id>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROMPT="${2:-$ROOT/orchestration/prompts/$NAME.md}"
WT="$(dirname "$ROOT")/rivet-wt/$NAME"
[ -f "$PROMPT" ] || { echo "no prompt file: $PROMPT"; exit 1; }
mkdir -p "$(dirname "$WT")"
[ -d "$WT" ] || git -C "$ROOT" worktree add -q -b "task/$NAME" "$WT" main || exit 1
mkdir -p "$ROOT/orchestration/runs"
STARTUP_SECS="${STARTUP_SECS:-150}" \
  "$ROOT/orchestration/run-task.sh" "$NAME" "$PROMPT" 2>&1 | sed "s|^|[$NAME] |"
