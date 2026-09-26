#!/usr/bin/env bash
#
# test-branch-collision-park.sh — detect_branch_collisions/park_branch_collisions (lib.sh)
#   park a bead whose recorded branch is checked out in a DIFFERENT bead's worktree, so
#   dispatch never spends another summon reaching aeon.sh's law-one-aeon-one-worktree
#   refusal at claim time.
#
# THE DEFECT (sp-lyglx). aeon.sh's worktree-attach guard reacts correctly once a bead is
# claimed, but nothing about the input changes between claims (law-a-retry-must-change-an-
# input): a bead whose branch: label is squatted by another bead's canonical worktree died
# — or looped — on every summon. A bead whose own DEFAULT branch is the squatted one (a
# parent shadowed by a child that inherited its name before groomer.sh stopped copying it)
# can never self-correct at all, because renaming to its own default changes nothing.
#
# THE SECOND DEFECT (sp-vcxmz). Parking with $SPIRA_ASK_LABEL is right when a human has to
# free the squatter, but a squatter whose OWN bead is closed, whose tree is clean and whose
# owner has no live aeon needs no human at all — freeing it is mechanical, and escalating it
# anyway put a P0 test-plan bead behind a question the operator could not answer. Cases 4-6
# below cover the three-way gate: closed+clean+unheld is freed; open, dirty or live still
# parks.
#
# Driven through a REAL git repository and a real bd fixture (law-prefer-the-real-
# dependency): the assertion is about `git worktree list`'s actual output, not a model of it.
#
# defect: sp-lyglx sp-vcxmz
# covers: spira/lib.sh spira/sentinel.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

# Non-default ask label (law-gates-run-in-a-clean-environment): a hardcoded "needs-operator"
# in the detector would pass against the shipped default and fail here.
export SPIRA_HOME="$HERE"
export SPIRA_ASK_LABEL="needs-decision-bc"
export SPIRA_CONF=/tmp/.spira-test-noconf-$$
export SPIRA_HOME_REPO=spira

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-branch-collision-park
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"; git -C "$REPO" add f; git -C "$REPO" commit -qm seed
git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN/worktree"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

testdb_up branchcollide || { echo "test-branch-collision-park: could not build fixture database"; exit 1; }

acted=0; progressed=0
act()      { acted=$((acted+1)); }
progress() { progressed=$((progressed+1)); act "$@"; }
log()      { : ; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"
. "$HERE/testlib.sh"

echo "test-branch-collision-park.sh"

# sp-hold's OWN canonical worktree, but checked out on sp-root's default branch — the exact
# shape the incident evidence showed: a child claimed first, before a sibling or its own
# parent, and walked off with the parent's branch name inside its own worktree directory.
git -C "$REPO" worktree add -q -b spira/sp-root "$SPIRA_RUN/worktree/sp-hold" main

# sp-fine's own canonical worktree on its own default branch — a legitimate, unremarkable
# worktree that must never be mistaken for a collision.
git -C "$REPO" worktree add -q -b spira/sp-fine "$SPIRA_RUN/worktree/sp-fine" main

seed() { # seed <id> [branch-state]
    local id="$1" br="${2:-}"
    printf '{"id":"%s","title":"t %s","status":"open","issue_type":"task","labels":["repo:fixture","spira"]}\n' \
        "$id" "$id" | testdb_seed
    [ -n "$br" ] && bd -C "$SPIRA_DB" set-state "$id" "branch=$br" >/dev/null 2>&1
}

testdb_reset
seed sp-root                       # default branch spira/sp-root, squatted by sp-hold
seed sp-child spira/sp-root        # explicit affinity to the squatted branch (inherited)
seed sp-hold                       # holds sp-root's branch in ITS OWN worktree; not itself a collision
seed sp-fine                       # own worktree, own branch: no collision at all

# ==========================================================================================
echo
echo "case 1 — detect_branch_collisions: positive and negative controls together"
# ==========================================================================================
out="$(detect_branch_collisions 2>/dev/null)"

want   "root bead flagged: its own default branch is squatted"      "COLLISION sp-root fixture spira/sp-root sp-hold" "$out"
want   "child bead flagged: inherited affinity to the squatted branch" "COLLISION sp-child fixture spira/sp-root sp-hold" "$out"
nowant "holder bead itself is not flagged (its own branch is free)" "COLLISION sp-hold"  "$out"
nowant "unrelated bead with its own worktree is not flagged"        "COLLISION sp-fine"  "$out"

# ==========================================================================================
echo
echo "case 2 — park_branch_collisions labels and notes each collision, once"
# ==========================================================================================
park_branch_collisions "$out"

for id in sp-root sp-child; do
    labels="$(bdq label list "$id" 2>/dev/null)"
    want "case 2: $id labeled $SPIRA_ASK_LABEL" "$SPIRA_ASK_LABEL" "$labels"
    want "case 2: $id labeled overseer"          "overseer"        "$labels"
done

notes_root="$(bd -C "$SPIRA_DB" show sp-root --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"
want "case 2: parked note names the true holder" "sp-hold" "$notes_root"

for id in sp-hold sp-fine; do
    labels="$(bdq label list "$id" 2>/dev/null)"
    nowant "case 2: $id (no collision) is not parked" "$SPIRA_ASK_LABEL" "$labels"
done

# ==========================================================================================
echo
echo "case 3 — a second sweep neither re-detects nor re-notes an already-parked bead"
# ==========================================================================================
out2="$(detect_branch_collisions 2>/dev/null)"
nowant "already-parked bead excluded from a repeat detect pass" "sp-root" "$out2"
nowant "already-parked bead excluded from a repeat detect pass" "sp-child" "$out2"

# park_branch_collisions itself is idempotent even fed stale output directly (defense in
# depth: detect already excludes parked beads, but park must not re-note if it is ever
# handed a line for a bead parked since the output was produced).
park_branch_collisions "$out"
notes_root2="$(bd -C "$SPIRA_DB" show sp-root --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"
n_notes="$(grep -c "Parked by detect_branch_collisions" <<< "$notes_root2" || true)"
is "case 3: exactly one park note on sp-root, not re-appended" "1" "$n_notes"

# ==========================================================================================
echo
echo "case 4 — a squatter whose owning bead is CLOSED and CLEAN is freed, not parked (sp-vcxmz)"
# ==========================================================================================
git -C "$REPO" worktree add -q -b spira/sp-root3 "$SPIRA_RUN/worktree/sp-hold3" main
seed sp-root3                      # default branch spira/sp-root3, squatted by sp-hold3
seed sp-hold3
bd -C "$SPIRA_DB" close sp-hold3 --reason "done" >/dev/null 2>&1

out4="$(detect_branch_collisions 2>/dev/null)"
want "case 4: sp-root3 detected as a collision before freeing" "COLLISION sp-root3 fixture spira/sp-root3 sp-hold3" "$out4"

sha_before="$(git -C "$REPO" rev-parse refs/heads/spira/sp-root3)"
park_out4="$(park_branch_collisions "$out4")"

want "case 4: park_branch_collisions reports sp-root3 FREED" "FREED sp-root3 fixture spira/sp-root3 sp-hold3 $SPIRA_RUN/worktree/sp-hold3" "$park_out4"
wt3_state=present; [ -e "$SPIRA_RUN/worktree/sp-hold3" ] || wt3_state=gone
is   "case 4: sp-hold3's stale worktree directory is gone" "gone" "$wt3_state"
is   "case 4: spira/sp-root3 branch and its commit survive — only the worktree is removed" \
     "$sha_before" "$(git -C "$REPO" rev-parse refs/heads/spira/sp-root3 2>/dev/null)"

labels4="$(bdq label list sp-root3 2>/dev/null)"
nowant "case 4: freed bead is not parked with $SPIRA_ASK_LABEL — it stays claimable" "$SPIRA_ASK_LABEL" "$labels4"

out4b="$(detect_branch_collisions 2>/dev/null)"
nowant "case 4: a repeat detect pass sees no collision — the worktree is really gone" "sp-root3" "$out4b"

# ==========================================================================================
echo
echo "case 5 — a closed holder with an UNCOMMITTED change in its worktree still parks"
# ==========================================================================================
git -C "$REPO" worktree add -q -b spira/sp-root4 "$SPIRA_RUN/worktree/sp-hold4" main
printf 'uncommitted\n' > "$SPIRA_RUN/worktree/sp-hold4/dirty.txt"
seed sp-root4
seed sp-hold4
bd -C "$SPIRA_DB" close sp-hold4 --reason "done" >/dev/null 2>&1

out5="$(detect_branch_collisions 2>/dev/null)"
park_out5="$(park_branch_collisions "$out5")"

nowant "case 5: a dirty closed holder is never freed" "FREED sp-root4" "$park_out5"
wt4_state=gone; [ -e "$SPIRA_RUN/worktree/sp-hold4" ] && wt4_state=present
is     "case 5: sp-hold4's worktree survives — dirty holders are never touched" "present" "$wt4_state"
labels5="$(bdq label list sp-root4 2>/dev/null)"
want "case 5: sp-root4 parks with $SPIRA_ASK_LABEL like any other unfreeable collision" "$SPIRA_ASK_LABEL" "$labels5"

# ==========================================================================================
echo
echo "case 6 — a closed holder with a LIVE session on its worktree still parks"
# ==========================================================================================
git -C "$REPO" worktree add -q -b spira/sp-root5 "$SPIRA_RUN/worktree/sp-hold5" main
seed sp-root5
seed sp-hold5
bd -C "$SPIRA_DB" close sp-hold5 --reason "done" >/dev/null 2>&1
# holder_alive checks a hold pidfile by pid alone (law-prefer-the-real-dependency's own
# positive control, mirrored from test-hold.sh): our own pid is certainly alive.
echo $$ > "$SPIRA_RUN/hold-sp-hold5.pid"

out6="$(detect_branch_collisions 2>/dev/null)"
park_out6="$(park_branch_collisions "$out6")"

nowant "case 6: a closed holder with a live session is never freed" "FREED sp-root5" "$park_out6"
wt5_state=gone; [ -e "$SPIRA_RUN/worktree/sp-hold5" ] && wt5_state=present
is     "case 6: sp-hold5's worktree survives — a live holder is never touched" "present" "$wt5_state"
labels6="$(bdq label list sp-root5 2>/dev/null)"
want "case 6: sp-root5 parks with $SPIRA_ASK_LABEL like any other unfreeable collision" "$SPIRA_ASK_LABEL" "$labels6"
rm -f "$SPIRA_RUN/hold-sp-hold5.pid"

tl_summary
