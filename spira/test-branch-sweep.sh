#!/usr/bin/env bash
#
# test-branch-sweep.sh — branch-sweep.sh deletes a remote spira/<id> branch only when
# its tip is provably an ancestor of the base, reports every branch it cannot prove,
# and never touches spira/queue/*.
#
#   ./test-branch-sweep.sh
#
# THE PROPERTY UNDER TEST. A sweep of a 255-branch backlog runs unsupervised; the one
# thing it must never do is destroy a branch that still carries unlanded work. This
# suite plants exactly that offender — a branch with a real commit never merged
# anywhere — and requires the sweep to report it and leave it alone
# (law-a-check-that-finds-nothing-must-first-prove-it-could-have-found-something).
#
# A REAL GIT REPOSITORY with a bare remote (law-prefer-the-real-dependency): the
# claims are about what `git push --delete` and `merge-base --is-ancestor` do to
# actual refs, which a stub cannot stand in for.
#
# defect: sp-6hjh3
# covers: spira/branch-sweep.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/no-such-repo-map"   # not in the map; landref falls to origin/HEAD

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" push -q origin main
git -C "$REPO" remote set-head origin main

remote_has() { git -C "$REMOTE" show-ref --verify -q "refs/heads/$1" 2>/dev/null; }

# make_and_push <branch> — branch off main, commit a unique file, push to the remote
# WITHOUT landing it on main. This is the unmerged shape.
make_and_push() {
    local br="$1"
    git -C "$REPO" checkout -q -b "$br" main
    printf '%s\n' "$br" > "$REPO/${br//\//_}.txt"
    git -C "$REPO" add "${br//\//_}.txt"
    git -C "$REPO" commit -q -m "$br: work"
    git -C "$REPO" push -q origin "$br"
    git -C "$REPO" checkout -q main
}

# land_via_merge <branch> — fast-forward main to include the branch, then push both,
# so the branch tip becomes a real ancestor of main.
land_via_merge() {
    local br="$1"
    git -C "$REPO" merge -q --ff-only "$br"
    git -C "$REPO" push -q origin main "$br"
}

# ======================================================================================
# ANCESTOR — a branch whose tip already landed on main: swept.
# ======================================================================================
echo "ancestor branch (proven landed):"
make_and_push spira/sp-landed
land_via_merge spira/sp-landed
if remote_has spira/sp-landed; then ok "landed branch present before sweep"; else bad "landed branch present before sweep" "push to remote failed"; fi

# ======================================================================================
# NOT AN ANCESTOR — the offender. Real commit, pushed, never merged anywhere. This is
# the positive control: without it, a sweep that deleted everything would still pass
# every other assertion in this file.
# ======================================================================================
echo "unmerged branch (the offender — must survive):"
make_and_push spira/sp-orphan

# ======================================================================================
# QUEUE POPULATION — landed, but must never be touched by this tool regardless.
# ======================================================================================
echo "queue branch (landed, but excluded by name):"
make_and_push spira/queue/20260101T000000Z
land_via_merge spira/queue/20260101T000000Z

# ---- dry run first: nothing on the remote may move --------------------------------
out_dry="$("$HERE/branch-sweep.sh" "$REPO" --dry-run 2>&1)"
rc_dry=$?
is "dry run exits 0" 0 "$rc_dry"
want "dry run names the landed branch as would-delete" "WOULD-DELETE  spira/sp-landed" "$out_dry"
want "dry run names the orphan as not-ancestor" "NOT-ANCESTOR  spira/sp-orphan" "$out_dry"
want "dry run names the queue branch as excluded" "EXCLUDED      spira/queue/20260101T000000Z" "$out_dry"
if remote_has spira/sp-landed; then ok "dry run left the landed branch on the remote"; else bad "dry run left the landed branch on the remote" "dry run deleted despite --dry-run"; fi
if remote_has spira/sp-orphan; then ok "dry run left the orphan on the remote"; else bad "dry run left the orphan on the remote" "dry run deleted an unmerged branch"; fi
if remote_has spira/queue/20260101T000000Z; then ok "dry run left the queue branch on the remote"; else bad "dry run left the queue branch on the remote" "dry run deleted a queue branch"; fi

# ---- real sweep ---------------------------------------------------------------------
out_real="$("$HERE/branch-sweep.sh" "$REPO" 2>&1)"
rc_real=$?
is "real sweep exits 0" 0 "$rc_real"
want "real sweep reports the delete" "DELETED       spira/sp-landed" "$out_real"

if remote_has spira/sp-landed; then bad "landed branch deleted from remote" "spira/sp-landed still on the remote — sweep never reached push --delete"; else ok "landed branch deleted from remote"; fi

# THE OFFENDER MUST SURVIVE. If this assertion is ever the only thing keeping a
# non-ancestor branch alive, the check just proved it can fail loudly.
if remote_has spira/sp-orphan; then ok "unmerged branch survives the real sweep"; else bad "unmerged branch survives the real sweep" "spira/sp-orphan was deleted despite never being an ancestor of main"; fi
want "real sweep reports the orphan as not-ancestor" "NOT-ANCESTOR  spira/sp-orphan" "$out_real"

if remote_has spira/queue/20260101T000000Z; then ok "queue branch survives the real sweep"; else bad "queue branch survives the real sweep" "spira/queue/20260101T000000Z was deleted — queue population must never be touched here"; fi
want "real sweep reports the queue branch as excluded" "EXCLUDED      spira/queue/20260101T000000Z" "$out_real"

tl_summary
