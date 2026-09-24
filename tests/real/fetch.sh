#!/usr/bin/env bash
# Fetch the pinned public TypeScript project named in tests/real/manifest.toml
# (T46) for tests/real/check_real.py.
#
# The project's source is never vendored into this repository. This script
# makes a detached checkout of the pinned commit at $RIVET_REAL_DIR/<name>/:
#
#   1. if the checkout does not exist: `git init`, add the manifest's URL as
#      `origin`, fetch exactly the pinned commit (shallow), and check it out,
#      detached;
#   2. if it exists: refuse when it is not a Git checkout, when its `origin`
#      is not the manifest's URL, or when it is dirty (any tracked change or
#      untracked file); otherwise fetch the pin if HEAD is elsewhere;
#   3. verify that `git rev-parse HEAD` equals the pinned commit, and, when
#      the network was used, that the manifest's tag peels to that commit.
#
# Environment:
#   RIVET_REAL_DIR   destination root (default: target/real under the
#                    repository root, which .gitignore already ignores).
#
# Safety: nothing from the fetched repository is executed. Git hooks are
# disabled (core.hooksPath=/dev/null), templates are not copied, submodules
# are not touched, LFS smudging is skipped, and global/system Git config is
# ignored. No install, build, script, or test step runs. The network is used
# only to fetch the pinned commit; the rivet binary never needs it.
#
# Idempotent: a clean checkout already at the pin is verified and left as it is.
#
# Usage: tests/real/fetch.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$HERE/manifest.toml"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

export GIT_LFS_SKIP_SMUDGE=1
export GIT_TERMINAL_PROMPT=0
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL=/dev/null

g() {
    git -c core.hooksPath=/dev/null \
        -c advice.detachedHead=false \
        -c submodule.recurse=false \
        -c init.templateDir= \
        "$@"
}

die() {
    echo "fetch-real: ERROR: $*" >&2
    exit 1
}

command -v git >/dev/null || die "git is required"
[ -f "$MANIFEST" ] || die "manifest not found: $MANIFEST"

# Reads one top-level string key of the [project] table: key = "value".
field() {
    local value
    value="$(awk -v key="$1" '
        /^\[/ { table = $0; next }
        table == "[project]" && $1 == key && $2 == "=" {
            line = $0
            sub(/^[^=]*=[ \t]*"/, "", line)
            sub(/".*$/, "", line)
            print line
            exit
        }' "$MANIFEST")"
    [ -n "$value" ] || die "manifest has no [project] $1"
    printf '%s' "$value"
}

NAME="$(field name)"
URL="$(field url)"
TAG="$(field tag)"
COMMIT="$(field commit)"

[[ "$NAME" =~ ^[a-z0-9][a-z0-9._-]*$ ]] || die "bad project name: $NAME"
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || die "commit is not a full 40-hex SHA: $COMMIT"
[[ "$URL" =~ ^https:// ]] || die "url is not https: $URL"

ROOT="${RIVET_REAL_DIR:-$REPO_ROOT/target/real}"
mkdir -p "$ROOT"
DIR="$ROOT/$NAME"

verify_tag() {
    local peeled
    peeled="$(g ls-remote "$URL" "refs/tags/$TAG^{}" "refs/tags/$TAG" \
        | awk -v t="refs/tags/$TAG" '$2 == t "^{}" { p = $1 } $2 == t && p == "" { l = $1 } END { print (p != "" ? p : l) }')"
    [ "$peeled" = "$COMMIT" ] || die "tag $TAG at $URL peels to '${peeled:-nothing}', expected $COMMIT"
}

fetch_pin() {
    echo "fetch-real: $NAME: fetching $COMMIT from $URL"
    g -C "$DIR" fetch -q --depth 1 --no-tags --no-recurse-submodules origin "$COMMIT" \
        || die "$NAME: fetch of $COMMIT failed"
    g -C "$DIR" checkout -q --detach "$COMMIT" || die "$NAME: checkout of $COMMIT failed"
    verify_tag
}

if [ -e "$DIR" ]; then
    [ -d "$DIR/.git" ] || die "$DIR exists but is not a Git checkout; remove it and rerun"
    origin="$(g -C "$DIR" remote get-url origin 2>/dev/null || true)"
    [ "$origin" = "$URL" ] || die "$DIR has origin '${origin:-none}', expected $URL; remove it and rerun"
    head="$(g -C "$DIR" rev-parse -q --verify HEAD 2>/dev/null || true)"
    if [ -n "$head" ]; then
        dirty="$(g -C "$DIR" status --porcelain --untracked-files=all)"
        [ -z "$dirty" ] || die "$DIR is dirty; refusing to touch it:
$dirty"
    fi
    if [ "$head" != "$COMMIT" ]; then
        fetch_pin
    fi
else
    echo "fetch-real: $NAME: creating $DIR"
    mkdir -p "$DIR"
    g -C "$DIR" init -q || die "$NAME: git init failed"
    g -C "$DIR" remote add origin "$URL" || die "$NAME: could not add origin"
    fetch_pin
fi

head="$(g -C "$DIR" rev-parse -q --verify HEAD 2>/dev/null || true)"
[ "$head" = "$COMMIT" ] || die "$NAME: HEAD is '${head:-none}', expected $COMMIT"
dirty="$(g -C "$DIR" status --porcelain --untracked-files=all)"
[ -z "$dirty" ] || die "$NAME: checkout is dirty after fetch:
$dirty"
echo "fetch-real: $NAME: ok at $COMMIT ($TAG) in $DIR"
