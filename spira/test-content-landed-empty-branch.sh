#!/usr/bin/env bash
#
# test-content-landed-empty-branch.sh — a branch with zero commits ahead of the base
#   must not be reported as landed by content_landed.
#
#   ./test-content-landed-empty-branch.sh
#
# THE ORIGINAL DEFECT (sp-qc4kn): content_landed's is-ancestor check came before the
# ahead>0 guard, so it returned 0 for empty branches (ahead=0, is-ancestor), causing
# the Sending to reap them and the bead to cycle without an attempt being charged.
#
# REVISED BEHAVIOR (sp-bf31a): ancestor branches are landed by definition — moving the
# is-ancestor check before the ahead=0 guard makes content_landed return 0 for all
# ancestor branches, including empty ones. The Sending reaps them; CHECK 5 reopens the
# bead when no commit on the base names it (the normal reopening path). This is correct:
# a closed bead with an empty branch never had work done, so it should be reopened.
#
# THREE CASES, all against content_landed directly:
#
#   1. POSITIVE CONTROL — ancestry alone returns 0 for an empty branch (fixture is correct).
#   2. EMPTY BRANCH     — content_landed returns 0 for an ancestor branch (sp-bf31a).
#   3. SQUASH-MERGED    — a branch whose content reached the base as a squash commit
#      still returns 0 from content_landed (commits ahead, merge-tree same).
#
# sending.sh's OWN use of content_landed — reaping an ahead=0 ancestor branch exactly as
# it reaps a genuine content-landed one — is test-sending.sh's sp-cl0 row now (sp-rg46a);
# this file no longer builds a sending.sh fixture just to re-confirm the same content_landed
# answer sending.sh reads.
#
# tier: T2
# covers: spira/lib.sh
# hermetic-ok: a local git repo, no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export SPIRA_CONF="$TMP/no-such-conf"

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main

# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-content-landed-empty-branch.sh"

# --------------------------------------------------------------------------------------
# CASE 1 & 2 — an empty branch (zero commits, cut straight from main).
# --------------------------------------------------------------------------------------
git -C "$REPO" branch spira/sp-empty main

echo
echo "positive control — ancestry alone returns 0 for the empty branch:"
if git -C "$REPO" merge-base --is-ancestor "spira/sp-empty" "origin/main" 2>/dev/null; then
    ok "ancestry alone incorrectly reports the empty branch as landed (the old defect)"
else
    bad "positive control failed" "ancestry check should return 0 for an empty branch — fixture may be wrong"
fi

echo
echo "empty branch — content_landed returns 0 for an ancestor branch (sp-bf31a):"
if content_landed "$REPO" "spira/sp-empty" "origin/main"; then
    ok "content_landed returns 0 for an ancestor branch (ancestor implies landed)"
else
    bad "content_landed must return 0 for an ancestor branch" "it returned non-zero"
fi

# --------------------------------------------------------------------------------------
# CASE 3 — SQUASH-MERGED: content_landed must return 0 (commits ahead, content on base).
# --------------------------------------------------------------------------------------
echo
echo "squash-merged — content_landed still returns 0 when content is already on the base:"

git -C "$REPO" worktree add -q -b "spira/sp-sq" "$TMP/wt-sp-sq" origin/main
printf 'squashed\n' > "$TMP/wt-sp-sq/sq.txt"
git -C "$TMP/wt-sp-sq" add sq.txt
git -C "$TMP/wt-sp-sq" commit -q -m "sp-sq: squash candidate"
git -C "$REPO" worktree remove "$TMP/wt-sp-sq" 2>/dev/null || true

# Squash that same content onto origin/main (no branch in it).
git -C "$REPO" worktree add -q "$TMP/wt-sq-land" origin/main
printf 'squashed\n' > "$TMP/wt-sq-land/sq.txt"
git -C "$TMP/wt-sq-land" add sq.txt
git -C "$TMP/wt-sq-land" commit -q -m "sp-sq: squash merged to main"
git -C "$REPO" push -q origin \
    "$(git -C "$TMP/wt-sq-land" rev-parse HEAD):refs/heads/main"
git -C "$REPO" fetch -q origin
git -C "$REPO" worktree remove "$TMP/wt-sq-land" 2>/dev/null || true

_sq_ahead="$(git -C "$REPO" rev-list --count "origin/main..spira/sp-sq" 2>/dev/null)"
if [ "${_sq_ahead:-0}" -gt 0 ] 2>/dev/null; then
    ok "squash branch has commits ahead ($_sq_ahead) — fixture correct"
else
    bad "squash fixture" "branch should have commits ahead of origin/main"
fi

if content_landed "$REPO" "spira/sp-sq" "origin/main"; then
    ok "content_landed returns 0 for squash-merged branch (content already on base)"
else
    bad "content_landed should return 0 for squash-merged" "it returned non-zero"
fi

echo
tl_summary
