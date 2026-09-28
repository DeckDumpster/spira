#!/usr/bin/env bash
#
# rebase-stale.sh <bead-id> [repo-name] — rebase a submitted branch that has gone stale
# against its landing ref, mechanically, so a bead only reaches an aeon for a genuine
# content conflict or a red gate (law-a-hand-fix-names-its-root-cause: most "returned for
# rebase" branches were never actually contested, they had simply waited for their round
# while the base moved).
#
#   1. refs/heads/spira/<id> already an ancestor-of-nothing new -> nothing to do, exit 0.
#   2. attempts `git rebase` in a scratch worktree under the sanctioned root, resolving any
#      conflict with mech-resolve.sh at each stop; a hunk it cannot resolve aborts the
#      rebase and reopens the bead with the conflicting hunks quoted (exit 1).
#   3. on a clean/mechanical rebase, moves the real branch to the new tip and re-certifies
#      it through queue.sh submit (fences + touched-suite gate). A red gate restores the
#      branch to its pre-rebase tip and reopens the bead with the gate's own output quoted
#      (exit 2) — the mechanical rebase is not repeated, only the gate failure is handed on.
#      A green gate leaves the bead exactly as queue.sh submit leaves it (CERTIFIED, or
#      landed/closed for push mode) and notes the bead (exit 0).
#
# Every call is one line in SPIRA_REBASE_STALE_LOG — outcome plus reason — which is the
# measure this exists to produce: how much of the rework rate a mechanical resolver removes.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

id="${1:?rebase-stale.sh: bead id required}"
name="${2:-$(spira_home_repo)}"
repo="$(repo_root "$name" 2>/dev/null)" || { echo "rebase-stale.sh: cannot resolve repo $name" >&2; exit 3; }
br="spira/$id"

_record() {   # _record <outcome> [reason]
    local log="${SPIRA_REBASE_STALE_LOG:-$SPIRA_RUN/rebase-stale.log}"
    mkdir -p "$(dirname "$log")" 2>/dev/null
    printf 'REBASE_STALE %s id=%s repo=%s outcome=%s reason=%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$id" "$name" "$1" "${2:-}" >> "$log" 2>/dev/null || true
}

git -C "$repo" show-ref --verify -q "refs/heads/$br" || {
    echo "rebase-stale.sh: no such branch $br in $name" >&2; exit 3
}
landref="$(spira_landref "$name" 2>/dev/null)" || {
    echo "rebase-stale.sh: cannot resolve the land ref for $name" >&2; exit 3
}
git -C "$repo" rev-parse --verify -q "$landref" >/dev/null 2>&1 || {
    echo "rebase-stale.sh: land ref $landref does not resolve in $name" >&2; exit 3
}

if git -C "$repo" merge-base --is-ancestor "$landref" "refs/heads/$br" 2>/dev/null; then
    _record current ""
    printf 'rebase-stale: %s already contains %s — nothing to do\n' "$br" "$landref"
    exit 0
fi

if [ -n "$(worktree_of "$br" "$repo" 2>/dev/null)" ]; then
    _record busy "checked out in a live worktree"
    echo "rebase-stale.sh: $br is checked out in a live worktree — leaving it" >&2
    exit 3
fi

# ONE SCRATCH TREE PER REPOSITORY, under the sanctioned root, distinct from rebase_branch's
# own .rebase.<repo> (landing.sh) so the two never contend for the same worktree.
scratch="$SPIRA_RUN/worktree/.rebase-stale.$(basename "$repo")"
lockfile="$SPIRA_RUN/rebase-stale.$(basename "$repo").lock"
mkdir -p "$(dirname "$scratch")" 2>/dev/null
exec 9>"$lockfile" || { echo "rebase-stale.sh: cannot open $lockfile" >&2; exit 3; }
flock -w "${SPIRA_REBASE_LOCK_WAIT:-60}" 9 || {
    echo "rebase-stale.sh: another rebase-stale.sh holds $lockfile" >&2; exit 3
}

if [ ! -e "$scratch/.git" ]; then
    spira_prune_worktrees "$repo" >/dev/null 2>&1
    git -C "$repo" worktree add -q --detach "$scratch" "$landref" >/dev/null 2>&1 \
        || { _record error "cannot create scratch worktree"; exit 3; }
fi
git -C "$scratch" checkout -q --detach >/dev/null 2>&1
git -C "$scratch" reset -q --hard "$landref" 2>/dev/null
git -C "$scratch" clean -qfd 2>/dev/null
git -C "$scratch" checkout -q -B "$br" "refs/heads/$br" >/dev/null 2>&1 || {
    _record error "cannot check out $br in the scratch worktree"
    exit 3
}

old_tip="$(git -C "$repo" rev-parse "$br")"
export GIT_EDITOR=true EDITOR=true
git -C "$scratch" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" \
    rebase -q "$landref" >/dev/null 2>&1

# `$scratch/.git` IS A GITFILE, NOT A DIRECTORY — a worktree's own rebase-merge/rebase-apply
# state lives under the MAIN repo's git-dir (.git/worktrees/<name>/...), so it must be
# resolved with `rev-parse --git-path` rather than assumed relative to $scratch/.git.
_rs_mid_rebase() {
    local d
    d="$(git -C "$scratch" rev-parse --git-path rebase-merge 2>/dev/null)" && [ -n "$d" ] && [ -d "$d" ] && return 0
    d="$(git -C "$scratch" rev-parse --git-path rebase-apply 2>/dev/null)" && [ -n "$d" ] && [ -d "$d" ] && return 0
    return 1
}

mechanical=0
failed=0
while _rs_mid_rebase; do
    if [ -n "${REBASE_STALE_DEBUG:-}" ]; then
        echo "DEBUG mid-rebase status:" >&2
        git -C "$scratch" status --short >&2
        echo "DEBUG git-path rebase-merge: $(git -C "$scratch" rev-parse --git-path rebase-merge 2>&1)" >&2
    fi
    conflicted="$(git -C "$scratch" diff --name-only --diff-filter=U 2>/dev/null)"
    if [ -z "$conflicted" ] || ! bash "$HERE/mech-resolve.sh" "$scratch"; then
        failed=1
        break
    fi
    mechanical=1
    git -C "$scratch" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" \
        rebase --continue -q >/dev/null 2>&1
done
if [ "$failed" = 0 ] && ! git -C "$scratch" merge-base --is-ancestor "$landref" HEAD 2>/dev/null; then
    failed=1   # git refused the rebase outright, with nothing left conflicted to name
fi

if [ "$failed" = 1 ]; then
    conflicted="$(git -C "$scratch" diff --name-only --diff-filter=U 2>/dev/null | tr '\n' ' ')"
    conflicted="${conflicted% }"
    hunk=""
    for f in $conflicted; do
        hunk="$hunk
--- $f ---
$(sed -n '/^<<<<<<< /,/^>>>>>>> /p' "$scratch/$f" 2>/dev/null | head -60)"
    done
    git -C "$scratch" rebase --abort >/dev/null 2>&1
    git -C "$scratch" checkout -q --detach >/dev/null 2>&1
    _record conflict "files: ${conflicted:-refused}"
    bump_requeue "$id" "merge-conflict" 2>/dev/null || true
    bead_reopen "$id" rebase-conflict "$(printf \
        'rebase-stale.sh: %s does not rebase mechanically onto %s.\n\nConflicting file(s): %s\n%s' \
        "$br" "$landref" "${conflicted:-unknown}" "${hunk:-<no hunk captured>}")"
    land_mark "$id" RED "$old_tip" "no-rebase@$(git -C "$repo" rev-parse "$landref")"
    echo "rebase-stale.sh: $br has a real conflict — returned to an aeon" >&2
    exec 9>&-
    exit 1
fi

new_tip="$(git -C "$scratch" rev-parse HEAD)"
git -C "$scratch" checkout -q --detach >/dev/null 2>&1
git -C "$repo" branch -f "$br" "$new_tip" >/dev/null 2>&1

gate_out="$(bash "$HERE/queue.sh" submit "$br" "$name" 2>&1)"
gate_rc=$?
if [ "$gate_rc" -ne 0 ]; then
    git -C "$repo" branch -f "$br" "$old_tip" >/dev/null 2>&1
    _record gate-red "tip=$new_tip"
    bead_reopen "$id" rebase-gate-red "$(printf \
        'rebase-stale.sh: %s rebased mechanically onto %s (new tip %s) but failed its gate there. Left at its pre-rebase tip %s — the rebase itself was mechanical and does not need repeating, only the gate failure below.\n\n%s' \
        "$br" "$landref" "$new_tip" "$old_tip" "$gate_out")"
    land_mark "$id" RED "$old_tip" "no-rebase@$(git -C "$repo" rev-parse "$landref")"
    echo "rebase-stale.sh: $br rebased but failed its gate at $new_tip — returned to an aeon" >&2
    exec 9>&-
    exit 2
fi

kind="clean"; [ "$mechanical" = 1 ] && kind="mechanical"
_record "$kind" "tip=$new_tip"
bdq note "$id" "rebase-stale.sh: rebased $br onto $landref ($kind) and re-certified at $new_tip. No aeon session used." \
    >/dev/null 2>&1 || true
printf 'rebase-stale: %s rebased (%s) and certified at %s\n' "$br" "$kind" "$new_tip"
exec 9>&-
exit 0
