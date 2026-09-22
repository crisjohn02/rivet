#!/usr/bin/env bash
# Independent review checks for one task. Never trusts the implementor's report.
#
#   orchestration/review.sh T21
#
# Builds into a private CARGO_TARGET_DIR so a stale shared binary cannot make a
# broken build look green, runs the four mandated checks, and leaves a freshly
# built binary at $REVIEW_TARGET/debug/rivet for hand probing.
set -u
NAME="${1:?usage: review.sh <task-id>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${REVIEW_TARGET:-/tmp/rivet-review-$NAME}"
cd "$ROOT" || exit 1
fail=0
step() { echo; echo "===== $* ====="; }

step "diff stat vs HEAD"
git --no-pager diff --stat HEAD
git status --short

step "cargo fmt --all -- --check"
cargo fmt --all -- --check && echo "PASS fmt" || { echo "FAIL fmt"; fail=1; }

step "cargo clippy --workspace --all-targets --all-features -- -D warnings"
if cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -20; then
  echo "PASS clippy"; else echo "FAIL clippy"; fail=1; fi

step "cargo test --workspace --no-fail-fast"
cargo test --workspace --no-fail-fast 2>&1 | grep -E '^test result|FAILED|^error|panicked at' || true
cargo test --workspace --no-fail-fast >/dev/null 2>&1 && echo "PASS tests" || { echo "FAIL tests"; fail=1; }

step "python3 tests/gold/check_gold.py"
python3 tests/gold/check_gold.py 2>&1 | tail -3 && echo "PASS gold" || { echo "FAIL gold"; fail=1; }

step "feature builds"
cargo check --workspace --no-default-features 2>&1 | tail -2
cargo check --workspace --no-default-features --features lang-php 2>&1 | tail -2

step "binary for hand probing"
cargo build -q 2>&1 | tail -3
echo "binary: $CARGO_TARGET_DIR/debug/rivet"
ls -la "$CARGO_TARGET_DIR/debug/rivet" 2>/dev/null || echo "NO BINARY BUILT"

echo; echo "===== REVIEW RESULT: $([ $fail = 0 ] && echo ALL-CHECKS-PASS || echo CHECKS-FAILED) ====="
exit $fail
