#!/usr/bin/env bash
# mech-resolve.sh <worktree> — resolve every conflicted path left by an in-progress `git
# rebase`/`merge` ONLY when each one is mechanical: an append-only line-set file (union,
# ours first: .gitignore, spira-config/schema/spira-key-history.txt), a config key-list
# region (spira/conf.sh, keylist-union.py), a derived file regenerated from source rather
# than merged (spira-config/schema/spira.schema.json via `spira-config schema`, Cargo.lock
# via `cargo generate-lockfile`), or a hunk where every line on both sides is a comment
# (comment-union.py). Anything else: exit 1 and leave every conflict marker in place for
# the caller to abort.
#
# STAGE NUMBERS DURING A REBASE ARE THE SAME AS DURING A MERGE (1=ancestor, 2=ours,
# 3=theirs) — only the semantic labels swap, which the union resolvers do not care about.
set -uo pipefail
w="$1"; here="$(cd "$(dirname "$0")" && pwd)"
paths="$(git -C "$w" diff --name-only --diff-filter=U)"
[ -n "$paths" ] || exit 0
for p in $paths; do
    case "$p" in
        .gitignore|spira-config/schema/spira-key-history.txt)
            { git -C "$w" show ":2:$p"; git -C "$w" show ":3:$p"; } 2>/dev/null \
                | awk '!seen[$0]++' > "$w/$p.u" && mv "$w/$p.u" "$w/$p" || exit 1 ;;
        spira/conf.sh)
            python3 "$here/keylist-union.py" "$w/$p" >/dev/null 2>&1 || exit 1
            bash -n "$w/$p" || exit 1 ;;
        Cargo.lock)
            ( cd "$w" && cargo generate-lockfile ) >/dev/null 2>&1 || exit 1 ;;
        spira-config/schema/spira.schema.json)
            ( cd "$w" && cargo run -q --bin spira-config -- schema > "$p.new" ) 2>/dev/null \
                && mv "$w/$p.new" "$w/$p" || exit 1 ;;
        *)
            python3 "$here/comment-union.py" "$w/$p" >/dev/null 2>&1 || {
                echo "not mechanical: $p"; exit 1
            } ;;
    esac
    git -C "$w" add "$p" || exit 1
done
exit 0
