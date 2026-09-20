#!/usr/bin/env bash
# wiki-commit.sh <wiki-dir> <message>
# Reads filenames (one per line) from stdin. Acquires an exclusive lock on the
# wiki checkout, stages each file, commits, and pushes. Serialises concurrent
# callers so no writer can commit paths staged by another.
# Exits 0 on success or nothing-to-commit, 1 on lock or commit error.
set -uo pipefail
wiki="${1:?wiki-commit.sh: wiki dir required}"
msg="${2:?wiki-commit.sh: commit message required}"
[ -d "$wiki/.git" ] || { printf 'wiki-commit.sh: %s: not a git checkout\n' "$wiki" >&2; exit 1; }

files=""
while IFS= read -r f; do
    [ -n "$f" ] && files="${files:+$files$'\n'}$f"
done
[ -n "$files" ] || exit 0

{ exec {_wc_lockfd}>"$wiki/.git/spira-commit.lock"; } 2>/dev/null \
    || { printf 'wiki-commit.sh: cannot open lock for %s\n' "$wiki" >&2; exit 1; }
flock "${_wc_lockfd}"

while IFS= read -r f; do
    [ -n "$f" ] || continue
    git -C "$wiki" add -- "$f" 2>/dev/null || true
done <<< "$files"

if git -C "$wiki" diff --cached --quiet 2>/dev/null; then
    exec {_wc_lockfd}>&-
    exit 0
fi

rc=0
git -C "$wiki" commit -m "$msg" 2>/dev/null || rc=$?
if [ "$rc" -eq 0 ]; then
    git -C "$wiki" push 2>/dev/null || true
fi
exec {_wc_lockfd}>&-
exit "$rc"
