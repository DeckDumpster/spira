#!/usr/bin/env bash
# wiki-commit.sh <wiki-dir> <message>
# Reads filenames (one per line) from stdin. Acquires an exclusive lock on the
# wiki checkout, stages each file, commits, and pushes. Serialises concurrent
# callers; commits only the named paths (--only), leaving anything else staged untouched.
# Exits 0 on success or nothing-to-commit, 1 on lock, stage or commit error.
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

paths=()
while IFS= read -r f; do
    [ -n "$f" ] || continue
    # A directory path stages everything beneath it — every file a caller never named,
    # indistinguishable from `git add -A` for that subtree. Callers name files; refuse
    # anything that isn't one rather than silently sweeping it in.
    if [ -d "$wiki/$f" ]; then
        printf 'wiki-commit.sh: refusing directory path %s — name the files, not the directory\n' "$f" >&2
        continue
    fi
    add_deadline=$((SECONDS + ${WIKI_COMMIT_LOCK_WAIT:-10}))
    while ! add_err=$(git -C "$wiki" add -- "$f" 2>&1); do
        if [ -e "$wiki/.git/index.lock" ] && [ "$SECONDS" -lt "$add_deadline" ]; then
            sleep 0.2; continue
        fi
        printf 'wiki-commit.sh: git add %s failed:\n%s\n' "$f" "$add_err" >&2
        if [ -e "$wiki/.git/index.lock" ]; then
            age=$(( $(date +%s) - $(stat -c %Y "$wiki/.git/index.lock") ))
            holder="holder unknown"; if command -v fuser >/dev/null; then holder=held; fuser "$wiki/.git/index.lock" >/dev/null 2>&1 || holder="held by no process"; fi
            printf 'wiki-commit.sh: %s/.git/index.lock is %ss old, %s; clear it yourself if stale (this tool never removes another process lock)\n' "$wiki" "$age" "$holder" >&2
        fi
        exec {_wc_lockfd}>&-
        exit 1
    done
    if ! git -C "$wiki" ls-files --error-unmatch -- "$f" >/dev/null 2>&1 \
        && ! git -C "$wiki" cat-file -e "HEAD:$f" 2>/dev/null; then
        printf 'wiki-commit.sh: %s is not in the index after add — nothing was staged\n' "$f" >&2
        exec {_wc_lockfd}>&-
        exit 1
    fi
    paths+=("$f")
done <<< "$files"

if [ "${#paths[@]}" -eq 0 ] || git -C "$wiki" diff --cached --quiet -- "${paths[@]}" 2>/dev/null; then
    exec {_wc_lockfd}>&-
    exit 0
fi

rc=0
git -C "$wiki" commit --only -m "$msg" -- "${paths[@]}" 2>/dev/null || rc=$?
if [ "$rc" -eq 0 ]; then
    committed="$(git -C "$wiki" show --name-only --format= HEAD 2>/dev/null | sort)"
    named="$(git -C "$wiki" diff --name-only HEAD~1 HEAD -- "${paths[@]}" 2>/dev/null | sort)"
    if [ "$committed" != "$named" ]; then
        printf 'wiki-commit.sh: HEAD names paths beyond those given:\n%s\n' "$committed" >&2
        rc=1
    fi
fi
if [ "$rc" -eq 0 ]; then
    if ! perr=$(timeout 30 git -C "$wiki" push 2>&1) && grep -qi 'rejected' <<<"$perr"; then  # batch-job: network push to the brain remote
        printf 'wiki-commit.sh: push refused:\n%s\n' "$perr" >&2
        rc=1
    fi
fi
exec {_wc_lockfd}>&-
exit "$rc"
