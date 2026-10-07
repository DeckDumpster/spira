#!/usr/bin/env bash
#
# test-destroy-branch.sh — spira_destroy_branch refuses to delete a branch
# whose content has not landed on the base, and succeeds once it has.
#
#   ./test-destroy-branch.sh
#
# THE PROPERTY UNDER TEST (sp-w6bw / sp-hl72 / sp-mqsl). Before the content fence was
# added, spira_destroy_branch called git branch -D unconditionally once two
# lightweight checks passed (no live holder, not checked out in a worktree).
# A branch reclaimed or slain before its commits reached origin/main could be
# garbage-collected within minutes, with no error and no log line from landing.
# sp-w6bw filed the assertion requirement; sp-kq8l implemented it; sp-hl72 wrote this test.
#
# The fence uses spira-lc content-landed (diff-based), not merge-base --is-ancestor
# (ancestry-based). The distinction matters for squash repositories: a squash
# merge replays the branch's diff as one new commit that is NOT an ancestor of
# the branch tip, so ancestry alone says "not landed" about work that is
# demonstrably on the base. This test covers both cases:
#
#   (a) unlanded real-file branch -> refused
#   (b) squash-landed branch -> approved (ancestry would refuse this)
#
# A POSITIVE CONTROL precedes each absence assertion: bypass the fence with a
# non-empty caller arg and confirm the branch IS deleted, proving the control
# path reaches the deletion. Without it, a version that refused everything
# would pass both absence checks (law-absence-needs-a-positive-control).
#
# A REAL GIT REPOSITORY with a bare remote, because the claims are about what
# git says about refs and trees (law-prefer-the-real-dependency). No real
# beads database: the status seam stands in for the holder witness, keeping
# this suite off the database and its 6-second init cost.
#
# defect: sp-mqsl
# tier: T2
# covers: spira/lib.sh UC-landed-audit-reaping-15
# scar: spira_destroy_branch called git branch -D unconditionally; a branch reclaimed before its commits reached origin/main was silently garbage-collected with no error.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"
SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REAPLOG="$SPIRA_RUN/reap.log"
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_REPO_MAP="$TMP/no-such-repo-map"   # not in the map; landref falls to origin/HEAD

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" commit -q --allow-empty -m base
timeout 5 git -C "$REPO" push -q origin main
git -C "$REPO" remote set-head origin main

# shellcheck disable=SC1090
. "$HERE/lib.sh"

# Use the status seam so no beads database is needed.
spira_status_seam - <<'SEAM'
sp-db1	open
sp-db2	open
sp-db3	open
SEAM

# THE LIFECYCLE STATE IS SEEDED, NOT SWITCHED OFF (sp-v62vn: the machine is the only mode).
# sending reads each bead's claim from its lifecycle row (`spira-lc show`) and its queue
# state (`spira-lc state`); this stand-in answers both from one file per bead under
# $LCSTATE, holding the bead's state, and hands every other verb (content-landed, the
# fence's own diff check) to the tree's real spira-lc. A bead with no file has no row.
REAL_LC="$(command -v spira-lc)" || bail "spira-lc is not on PATH"
LCSTATE="$TMP/lc-state"; mkdir -p "$LCSTATE" "$TMP/lc-bin"
cat > "$TMP/lc-bin/spira-lc" <<LCSTUB
#!/bin/sh
case "\$1" in
    show)  [ -f "$LCSTATE/\$2" ] || exit 1
           printf '{"bead":{"bead_id":"%s","state":"%s","holder":null,"lease_until":null,"holds":[]},"delivery":null}\n' "\$2" "\$(cat "$LCSTATE/\$2")" ;;
    state) [ -f "$LCSTATE/\$2" ] && cat "$LCSTATE/\$2" ;;
    list)  printf '[' ; sep=''
           for f in "$LCSTATE"/*; do [ -f "\$f" ] || continue
               printf '%s{"bead_id":"%s","state":"%s","holds":[]}' "\$sep" "\$(basename "\$f")" "\$(cat "\$f")"; sep=','
           done
           printf ']\n' ;;
    *)     exec "$REAL_LC" "\$@" ;;
esac
LCSTUB
chmod +x "$TMP/lc-bin/spira-lc"
PATH="$TMP/lc-bin:$PATH"
for id in sp-db1 sp-db2 sp-db3 sp-db6 sp-db8; do printf 'READY\n' > "$LCSTATE/$id"; done

# ---- helpers -----------------------------------------------------------------------

branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/$1" 2>/dev/null; }

# make_branch <id>  — branch off main, add a unique file, commit, leave no worktree
make_branch() {
    local id="$1"; local br="spira/$id"
    git -C "$REPO" checkout -q -b "$br" main
    printf '%s\n' "$id" > "$REPO/$id.txt"
    git -C "$REPO" add "$id.txt"
    git -C "$REPO" commit -q -m "sp-$id: real work"
    git -C "$REPO" checkout -q main
}

# squash_land <id>  — land <id>'s branch onto main by squash merge, push, fetch
squash_land() {
    local id="$1"; local br="spira/$id"
    git -C "$REPO" merge -q --squash "$br" >/dev/null 2>&1
    git -C "$REPO" commit -q -m "squash-land sp-$id"
    timeout 5 git -C "$REPO" push -q origin main
    timeout 5 git -C "$REPO" fetch -q origin
}

# ======================================================================================
# POSITIVE CONTROL — bypass the fence by passing a caller arg. Proves the code path
# reaches git branch -D. Without this, a version that refused every call (returning 1
# immediately) would pass the "branch still exists" assertions below.
# ======================================================================================
echo "positive control (caller bypass):"

make_branch sp-db1
if branch_exists spira/sp-db1; then ok "branch exists before destroy"; else bad "branch exists before destroy" "branch was not created"; fi

spira_destroy_branch sp-db1 spira/sp-db1 "$REPO" "test: caller bypass" sending >/dev/null 2>&1
rc=$?
is "caller-bypass destroy exits 0" 0 "$rc"
if branch_exists spira/sp-db1; then
    bad "branch gone after caller-bypass" "spira/sp-db1 still exists — git branch -D was never reached"
else
    ok "branch gone after caller-bypass"
fi

# ======================================================================================
# UNLANDED BRANCH — the fence must refuse. The branch carries a file that is not on
# origin/main. spira-lc content-landed returns false, destroy must return 1.
# ======================================================================================
echo
echo "unlanded branch (fence must refuse):"

make_branch sp-db2
if branch_exists spira/sp-db2; then ok "unlanded branch exists before destroy"; else bad "unlanded branch exists" "branch was not created"; fi

# Confirm spira-lc content-landed sees it as unlanded (positive control for spira-lc content-landed itself).
if spira-lc content-landed "$REPO" spira/sp-db2 origin/main; then
    bad "spira-lc content-landed sees unlanded branch as unlanded" "returned 0 — branch content appears landed already"
else
    ok "spira-lc content-landed correctly refuses the unlanded branch"
fi

out="$(spira_destroy_branch sp-db2 spira/sp-db2 "$REPO" "test: unlanded" 2>&1)"
rc=$?
is "destroy returns 1 for unlanded branch" 1 "$rc"
if branch_exists spira/sp-db2; then
    ok "branch still exists after refused destroy"
else
    bad "branch still exists after refused destroy" "spira/sp-db2 was deleted — unlanded work lost"
fi
want "reaplog records REFUSED" "REFUSED" "$(cat "$SPIRA_REAPLOG" 2>/dev/null)"
# Clean up for next case.
git -C "$REPO" branch -D spira/sp-db2 >/dev/null 2>&1 || true

# ======================================================================================
# SQUASH-LANDED BRANCH — ancestry check would refuse this, but spira-lc content-landed approves.
# The key property: after a squash merge, merge-base --is-ancestor returns non-zero for
# the original branch tip. spira-lc content-landed answers "yes, landed" because the trees match.
# destroy must succeed.
# ======================================================================================
echo
echo "squash-landed branch (spira-lc content-landed approves, ancestry would refuse):"

make_branch sp-db3
squash_land sp-db3

# Plant the exact failure that existed before sp-mqsl: ancestry alone refuses this branch.
if git -C "$REPO" merge-base --is-ancestor spira/sp-db3 origin/main 2>/dev/null; then
    bad "ancestry alone refuses squash-landed branch" "branch IS an ancestor — fixture is wrong, squash did not land"
else
    ok "ancestry alone refuses the squash-landed branch (this is the defect spira-lc content-landed fixes)"
fi

# spira-lc content-landed must approve it.
if spira-lc content-landed "$REPO" spira/sp-db3 origin/main; then
    ok "spira-lc content-landed approves squash-landed branch"
else
    bad "spira-lc content-landed should approve squash-landed branch" "returned non-zero — fence would incorrectly refuse"
fi

out2="$(spira_destroy_branch sp-db3 spira/sp-db3 "$REPO" "test: squash-landed" 2>&1)"
rc=$?
is "destroy exits 0 for squash-landed branch" 0 "$rc"
if branch_exists spira/sp-db3; then
    bad "branch gone after squash-landed destroy" "spira/sp-db3 still exists — fence incorrectly refused squash-landed content"
else
    ok "branch gone after squash-landed destroy"
fi

# ======================================================================================
# CERTIFIED / IN_DELIVERY LIFECYCLE STATE — a branch in the merge queue is never destroyed, caller
# bypass or not: batch.sh selects by ref, and deleting it drops the branch from the next
# batch with no log line. Gap: UC-landed-audit-reaping-15.
# ======================================================================================
echo
echo "queued lifecycle state (fence must refuse, no bypass possible):"

make_branch sp-db4
printf 'CERTIFIED\n' > "$LCSTATE/sp-db4"
out4="$(spira_destroy_branch sp-db4 spira/sp-db4 "$REPO" "test: certified" caller-bypass 2>&1)"
rc4=$?
is "CERTIFIED refuses even with a caller bypass" 1 "$rc4"
if branch_exists spira/sp-db4; then ok "CERTIFIED branch still exists"; else bad "CERTIFIED branch still exists" "spira/sp-db4 was deleted"; fi
want "reaplog names the lifecycle state" "lifecycle state is CERTIFIED" "$(cat "$SPIRA_REAPLOG" 2>/dev/null)"

make_branch sp-db5
printf 'IN_DELIVERY\n' > "$LCSTATE/sp-db5"
out5="$(spira_destroy_branch sp-db5 spira/sp-db5 "$REPO" "test: in delivery" caller-bypass 2>&1)"
rc5=$?
is "IN_DELIVERY refuses even with a caller bypass" 1 "$rc5"
if branch_exists spira/sp-db5; then ok "IN_DELIVERY branch still exists"; else bad "IN_DELIVERY branch still exists" "spira/sp-db5 was deleted"; fi

# ======================================================================================
# LIVE HOLDER WITNESS — a live process (a hold, or a WORKING lifecycle row) refuses the destroy regardless of content. Gap: UC-landed-audit-reaping-15.
# ======================================================================================
echo
echo "live holder witness (fence must refuse):"

make_branch sp-db6
printf '%s\n' "$$" > "$SPIRA_RUN/hold-sp-db6.pid"
out6="$(spira_destroy_branch sp-db6 spira/sp-db6 "$REPO" "test: held" 2>&1)"
rc6=$?
is "a live hold pid refuses the destroy" 1 "$rc6"
if branch_exists spira/sp-db6; then ok "held branch still exists"; else bad "held branch still exists" "spira/sp-db6 was deleted while held"; fi
want "reaplog records the holder" "a live process holds it" "$(cat "$SPIRA_REAPLOG" 2>/dev/null)"
rm -f "$SPIRA_RUN/hold-sp-db6.pid"

make_branch sp-db7
# The lease is the lifecycle row's WORKING holder, never bd's in_progress (sp-mve9i).
printf 'WORKING\n' > "$LCSTATE/sp-db7"
out7="$(spira_destroy_branch sp-db7 spira/sp-db7 "$REPO" "test: in_progress" 2>&1)"
rc7=$?
is "an in_progress lease refuses the destroy" 1 "$rc7"
if branch_exists spira/sp-db7; then ok "in_progress branch still exists"; else bad "in_progress branch still exists" "spira/sp-db7 was deleted mid-lease"; fi
want "reaplog records the lease" "the lease has not been released" "$(cat "$SPIRA_REAPLOG" 2>/dev/null)"

# ======================================================================================
# CHECKED-OUT-IN-A-WORKTREE — a branch a live worktree has attached is not deletable;
# pruning the registration out from under it turns a live tree into an orphan. Gap:
# UC-landed-audit-reaping-15.
# ======================================================================================
echo
echo "checked out in a worktree (fence must refuse):"

make_branch sp-db8
WT="$TMP/wt-sp-db8"
git -C "$REPO" worktree add -q "$WT" spira/sp-db8 >/dev/null 2>&1
out8="$(spira_destroy_branch sp-db8 spira/sp-db8 "$REPO" "test: checked out" 2>&1)"
rc8=$?
is "a checked-out branch refuses the destroy" 1 "$rc8"
if branch_exists spira/sp-db8; then ok "checked-out branch still exists"; else bad "checked-out branch still exists" "spira/sp-db8 was deleted while checked out"; fi
want "reaplog names the worktree path" "checked out at" "$(cat "$SPIRA_REAPLOG" 2>/dev/null)"
git -C "$REPO" worktree remove -q --force "$WT" >/dev/null 2>&1 || true

# ======================================================================================
tl_summary
