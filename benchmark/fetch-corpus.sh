#!/usr/bin/env bash
# Recreate the private benchmark corpus copies listed in benchmark/corpus.toml.
#
# The corpus projects are private, so there is no public URL to fetch from.
# For each project this script makes a clean copy from a local repository the
# user supplies, reproducing exactly these steps:
#
#   1. clone the user-supplied local repository (no checkout, no hardlinks);
#   2. check out the pinned commit, detached;
#   3. remove the `origin` remote;
#   4. delete every tracked file whose name starts with `.env`.
#
# It then verifies that HEAD equals the pin, that no remote remains, and that
# the only differences from the pinned tree are those deleted env files.
# Anything else fails loudly.
#
# Environment (no defaults; nothing is guessed):
#   RIVET_CORPUS_DIR               destination; copies go to $RIVET_CORPUS_DIR/<name>/.
#                                  Keep it OUTSIDE this public repository.
#   RIVET_CORPUS_SOURCE_<NAME>     local path of the source repository for project
#                                  <name>, upper-cased with `-`/`.` as `_`
#                                  (for example RIVET_CORPUS_SOURCE_FLUENT).
#                                  Needed only when the copy does not exist yet.
#
# Safety: nothing from a repository is executed. Git hooks are disabled
# (core.hooksPath=/dev/null), templates are not copied, submodules are not
# touched, LFS smudging is skipped, and global/system Git config is ignored.
# No install, build, script, or test step runs.
#
# Idempotent: an existing, correct copy is re-verified and left as it is.
#
# Usage: benchmark/fetch-corpus.sh [name ...]   (default: every project)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$HERE/corpus.toml"
REPO_ROOT="$(cd "$HERE/.." && pwd)"

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
    echo "fetch-corpus: ERROR: $*" >&2
    exit 1
}

command -v git >/dev/null || die "git is required"
command -v python3 >/dev/null || die "python3 (3.11+, for tomllib) is required"
[ -f "$MANIFEST" ] || die "manifest not found: $MANIFEST"

[ -n "${RIVET_CORPUS_DIR:-}" ] || die "set RIVET_CORPUS_DIR to the destination directory (outside this repository)"
CORPUS="$(python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$RIVET_CORPUS_DIR")"
case "$CORPUS/" in
    "$(cd "$REPO_ROOT" && pwd -P)/"*)
        die "RIVET_CORPUS_DIR ($CORPUS) is inside the rivet repository; the corpus is private and must live outside it" ;;
esac
mkdir -p "$CORPUS"

# name<TAB>commit, one line per project, in manifest order.
PROJECTS="$(python3 - "$MANIFEST" <<'PY'
import re, sys, tomllib
with open(sys.argv[1], "rb") as fh:
    data = tomllib.load(fh)
names = set()
for p in data.get("project", []):
    name, commit = p["name"], p["commit"]
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]*", name) or name in names:
        sys.exit("bad or duplicate project name: %r" % name)
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        sys.exit("commit for %s is not a full 40-hex SHA: %r" % (name, commit))
    names.add(name)
    print("%s\t%s" % (name, commit))
PY
)" || die "could not read $MANIFEST"

[ -n "$PROJECTS" ] || die "no [[project]] entries in $MANIFEST"

if [ "$#" -gt 0 ]; then
    for want in "$@"; do
        printf '%s\n' "$PROJECTS" | cut -f1 | grep -qxF "$want" \
            || die "unknown project: $want"
    done
fi

# Tracked paths whose file name starts with `.env`, NUL-separated.
env_files() {
    g -C "$1" ls-files -z | python3 -c '
import sys
for p in sys.stdin.buffer.read().split(b"\0"):
    if p and p.rsplit(b"/", 1)[-1].startswith(b".env"):
        sys.stdout.buffer.write(p + b"\0")
'
}

verify() {
    local name="$1" commit="$2" dir="$3"
    local head remotes status expected

    head="$(g -C "$dir" rev-parse -q --verify HEAD 2>/dev/null || true)"
    [ "$head" = "$commit" ] || die "$name: HEAD is '${head:-none}', expected $commit"

    remotes="$(g -C "$dir" remote)"
    [ -z "$remotes" ] || die "$name: copy still has remote(s): $remotes"

    # The only tracked differences allowed are the deleted env files.
    status="$(g -C "$dir" status --porcelain --untracked-files=no | LC_ALL=C sort)"
    expected="$(env_files "$dir" | tr '\0' '\n' | sed '/^$/d; s/^/ D /' | LC_ALL=C sort)"
    [ "$status" = "$expected" ] || die "$name: tracked state differs from $commit minus env files:
--- got
$status
--- expected
$expected"
}

fetch_one() {
    local name="$1" commit="$2"
    local dir="$CORPUS/$name"

    if [ -e "$dir" ]; then
        [ -d "$dir/.git" ] || die "$dir exists but is not a git checkout; remove it and rerun"
        verify "$name" "$commit" "$dir"
        echo "fetch-corpus: $name: ok at $commit (existing copy)"
        return
    fi

    local var src
    var="RIVET_CORPUS_SOURCE_$(printf '%s' "$name" | tr 'a-z.-' 'A-Z__')"
    src="${!var:-}"
    [ -n "$src" ] || die "$name: no copy at $dir and $var is not set"
    [ -d "$src" ] || die "$name: $var=$src is not a directory"

    echo "fetch-corpus: $name: cloning into $dir"
    g clone -q --no-checkout --no-hardlinks --no-recurse-submodules "$src" "$dir" \
        || die "$name: clone failed"
    g -C "$dir" cat-file -e "${commit}^{commit}" 2>/dev/null \
        || { rm -rf "$dir"; die "$name: commit $commit is not in $src"; }
    g -C "$dir" checkout -q --detach "$commit" || die "$name: checkout of $commit failed"
    g -C "$dir" remote remove origin || die "$name: could not remove origin"
    (cd "$dir" && env_files . | xargs -0 rm -f --)

    verify "$name" "$commit" "$dir"
    echo "fetch-corpus: $name: ok at $commit"
}

while IFS=$'\t' read -r name commit; do
    if [ "$#" -gt 0 ]; then
        skip=1
        for want in "$@"; do [ "$want" = "$name" ] && skip=0; done
        [ "$skip" -eq 0 ] || continue
    fi
    fetch_one "$name" "$commit"
done <<< "$PROJECTS"
