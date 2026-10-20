#!/usr/bin/env bash
#
# test-no-python.sh — no tracked *.py exists anywhere in the repository: the harness is Rust
# and shell, and a Python script is a second language runtime to install and keep in step.
#
# The scan has a git branch and a disk-walking fallback for a container that bind-mounts only
# a linked worktree (its .git names a gitdir the container cannot see). Both are proven on
# throwaway trees first, because a scan that finds nothing means nothing until it has found
# something (law-absence-needs-a-positive-control).
#
# tier: T0
# covers: *.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-no-python.sh"

scan() {   # scan <repo> -> every *.py path, one per line
    local repo="$1"
    if git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git -C "$repo" ls-files -z
    else
        find "$repo" \( -name .git -o -name target -o -path "$repo/bin" \
                -o -name .runtime -o -name .beads -o -name .dolt \) -prune \
            -o -type f -print0 | sed -z "s#^$repo/##"
    fi | tr '\0' '\n' | grep -E '\.py$' || true
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/g/tools" "$TMP/d/tools"
for t in g d; do
    printf 'print(1)\n' > "$TMP/$t/tools/planted.py"
    printf 'echo ok\n' > "$TMP/$t/tools/fine.sh"
    printf 'pyc\n' > "$TMP/$t/tools/cached.pyc"
done
git -C "$TMP/g" init -q -b main
git -C "$TMP/g" add -A

is "git branch: a planted .py is found, and only it" "tools/planted.py" "$(scan "$TMP/g")"
is "fallback branch: a planted .py is found, and only it" "tools/planted.py" "$(scan "$TMP/d")"

rm -f "$TMP/g/tools/planted.py" "$TMP/d/tools/planted.py"
git -C "$TMP/g" add -A
is "git branch: a tree with none reports none" "" "$(scan "$TMP/g")"
is "fallback branch: a tree with none reports none" "" "$(scan "$TMP/d")"

found="$(scan "$ROOT")"
is "the repository tracks no *.py file" "" "$found"

tl_summary
