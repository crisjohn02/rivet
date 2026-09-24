#!/usr/bin/env bash
# Regenerate (or verify) the committed parser C of rivet's patched
# tree-sitter-typescript from its grammar sources. See RIVET-PATCHES.md.
#
# Nothing in the rivet build runs this: the generated C is committed. It is
# for changing the grammar, or for checking that the committed C matches the
# grammar sources.
#
# Usage:
#   vendor/tree-sitter-typescript/regenerate.sh           # regenerate, test, copy src/ back
#   vendor/tree-sitter-typescript/regenerate.sh --check   # regenerate, test, compare only
#
# Needs node/npm and a C compiler. The network is used only by npm, to install
# the two pinned packages below into a scratch directory (default
# target/grammar-regen/ under the repository root, which git ignores). The CLI
# runs with HOME pointed into the scratch directory so its cache and lock
# files stay there.
set -euo pipefail

TREE_SITTER_CLI_VERSION=0.24.4       # the CLI upstream used for v0.23.2 (see RIVET-PATCHES.md)
TREE_SITTER_JAVASCRIPT_VERSION=0.23.1 # the base grammar define-grammar.js requires

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
SCRATCH="${RIVET_GRAMMAR_SCRATCH:-$REPO_ROOT/target/grammar-regen}"
MODE="${1:-write}"

case "$MODE" in
    write | --check) ;;
    *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

rm -rf "$SCRATCH/grammar"
mkdir -p "$SCRATCH/grammar" "$SCRATCH/home" "$SCRATCH/lib"
cp -R "$HERE/common" "$HERE/typescript" "$HERE/tsx" "$HERE/queries" "$HERE/test" \
    "$HERE/tree-sitter.json" "$SCRATCH/grammar/"

npm install --prefix "$SCRATCH/grammar" --no-save --no-audit --no-fund --ignore-scripts \
    "tree-sitter-javascript@$TREE_SITTER_JAVASCRIPT_VERSION" >/dev/null
# The CLI package downloads its prebuilt binary in its install script.
npm install --prefix "$SCRATCH/cli" --no-save --no-audit --no-fund \
    "tree-sitter-cli@$TREE_SITTER_CLI_VERSION" >/dev/null

ts() {
    HOME="$SCRATCH/home" TREE_SITTER_LIBDIR="$SCRATCH/lib" \
        "$SCRATCH/cli/node_modules/.bin/tree-sitter" "$@"
}

ts --version
(cd "$SCRATCH/grammar/typescript" && ts generate)
(cd "$SCRATCH/grammar/tsx" && ts generate)
(cd "$SCRATCH/grammar" && ts test)

status=0
for dialect in typescript tsx; do
    if [ "$MODE" = "--check" ]; then
        if ! diff -r "$HERE/$dialect/src" "$SCRATCH/grammar/$dialect/src" >/dev/null; then
            echo "regenerate: $dialect/src differs from the committed C" >&2
            status=1
        fi
    else
        rm -rf "$HERE/$dialect/src"
        cp -R "$SCRATCH/grammar/$dialect/src" "$HERE/$dialect/src"
    fi
done
if [ "$MODE" = "--check" ] && [ "$status" -eq 0 ]; then
    echo "regenerate: committed C matches the grammar sources"
fi
exit "$status"
