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
# parent shadowed by a child that inherited its name before groomer stopped copying it)
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
# tier: T2
# covers: spira/lib.sh sentinel/src/* aeon/src/*
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
# sp-mve9i: the collision detector's candidates are the beads the lifecycle machine says wait
# for a builder (READY/REWORK), never bd's open; the fixture's open beads are told to it in
# lifecycle terms by a stand-in lifecycle service (testlib.sh lc_socket_mirror).
lc_socket_mirror "$TMP/lcsock"

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
echo "case 1b — detect_branch_collisions reads branch: off the list it already fetched, not with a bd state call per bead (sp-nsxhd)"
# ==========================================================================================
REAL_BD="$(command -v bd)"
STATE_LOG="$TMP/state-calls.log"
BD_STUB="$TMP/bd-stub.sh"
cat > "$BD_STUB" <<STUBEOF
#!/usr/bin/env bash
for a in "\$@"; do
    if [ "\$a" = state ]; then
        printf '%s\n' "\$*" >> "$STATE_LOG"
        break
    fi
done
exec "$REAL_BD" "\$@"
STUBEOF
chmod +x "$BD_STUB"
: > "$STATE_LOG"

out1b="$(SPIRA_BD="$BD_STUB" detect_branch_collisions 2>/dev/null)"
is   "case 1b: no bd state call while detecting a real collision" "0" "$(wc -l < "$STATE_LOG" | tr -d ' ')"
want "case 1b: output is identical to the unstubbed pass" "COLLISION sp-root fixture spira/sp-root sp-hold" "$out1b"

# ==========================================================================================
echo
echo "case 2 — park_branch_collisions parks a real collision, but cuts an inherited label (sp-ln4ke)"
# ==========================================================================================
park_out2="$(park_branch_collisions "$out")"

labels_root="$(bdq label list sp-root 2>/dev/null)"
want "case 2: sp-root (own branch squatted) is labeled $SPIRA_ASK_LABEL" "$SPIRA_ASK_LABEL" "$labels_root"
want "case 2: sp-root (own branch squatted) is labeled overseer"          "overseer"        "$labels_root"

labels_child="$(bdq label list sp-child 2>/dev/null)"
nowant "case 2: sp-child (inherited its parent's branch label) is never labeled $SPIRA_ASK_LABEL" "$SPIRA_ASK_LABEL" "$labels_child"
nowant "case 2: sp-child (inherited its parent's branch label) is never labeled overseer"          "overseer"        "$labels_child"
nowant "case 2: sp-child's inherited branch: label is gone"                                        "branch:spira/sp-root" "$labels_child"
want   "case 2: park_branch_collisions reports sp-child UNLABELED, naming sp-root" \
       "UNLABELED sp-child fixture spira/sp-root sp-root" "$park_out2"

notes_root="$(bd -C "$SPIRA_DB" show sp-root --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"
want "case 2: parked note names the true holder" "sp-hold" "$notes_root"

notes_child="$(bd -C "$SPIRA_DB" show sp-child --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"
want "case 2: sp-child's note names the parent it inherited from, not needs-ryan prose" "inherited branch:spira/sp-root from sp-root" "$notes_child"
nowant "case 2: sp-child's note never tells Ryan to free a worktree" "free $SPIRA_RUN" "$notes_child"

for id in sp-hold sp-fine; do
    labels="$(bdq label list "$id" 2>/dev/null)"
    nowant "case 2: $id (no collision) is not parked" "$SPIRA_ASK_LABEL" "$labels"
done

# ==========================================================================================
echo
echo "case 2b — an inherited label with the child's OWN commits already on it is noted, not stranded"
# ==========================================================================================
git -C "$REPO" worktree add -q -b spira/sp-root2 "$SPIRA_RUN/worktree/sp-hold2" main
printf 'sp-child2 was here\n' > "$SPIRA_RUN/worktree/sp-hold2/child2.txt"
git -C "$SPIRA_RUN/worktree/sp-hold2" add child2.txt
git -C "$SPIRA_RUN/worktree/sp-hold2" commit -qm "sp-child2: work committed onto the inherited branch"

seed sp-root2                       # default branch spira/sp-root2, squatted by sp-hold2
seed sp-child2 spira/sp-root2       # inherited affinity, but it already committed onto it

out2b="$(detect_branch_collisions 2>/dev/null)"
want "case 2b: sp-child2 detected as a collision before correction" "COLLISION sp-child2 fixture spira/sp-root2 sp-hold2" "$out2b"

park_out2b="$(park_branch_collisions "$out2b")"
want "case 2b: park_branch_collisions reports sp-child2 UNLABELED" "UNLABELED sp-child2 fixture spira/sp-root2 sp-root2" "$park_out2b"

labels_child2="$(bdq label list sp-child2 2>/dev/null)"
nowant "case 2b: sp-child2 is never labeled $SPIRA_ASK_LABEL" "$SPIRA_ASK_LABEL" "$labels_child2"

notes_child2="$(bd -C "$SPIRA_DB" show sp-child2 --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"
want "case 2b: sp-child2's own commit is named so it is not stranded" "sp-child2: work committed onto the inherited branch" "$notes_child2"

# ==========================================================================================
echo
echo "case 3 — a second sweep neither re-detects nor re-notes an already-parked or already-cut bead"
# ==========================================================================================
out2="$(detect_branch_collisions 2>/dev/null)"
nowant "already-parked bead excluded from a repeat detect pass" "sp-root" "$out2"
nowant "already-cut bead excluded from a repeat detect pass"    "sp-child" "$out2"

# park_branch_collisions itself is idempotent even fed stale output directly (defense in
# depth: detect already excludes parked/cut beads, but park must not re-note if it is ever
# handed a line for a bead already resolved since the output was produced).
park_out_repeat="$(park_branch_collisions "$out")"
notes_root2="$(bd -C "$SPIRA_DB" show sp-root --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"
n_notes="$(grep -c "Parked by detect_branch_collisions" <<< "$notes_root2" || true)"
is "case 3: exactly one park note on sp-root, not re-appended" "1" "$n_notes"

notes_child2b="$(bd -C "$SPIRA_DB" show sp-child --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("notes","") if d else "")' 2>/dev/null)"
n_child_notes="$(grep -c "Corrected by detect_branch_collisions" <<< "$notes_child2b" || true)"
is "case 3: exactly one correction note on sp-child, not re-appended" "1" "$n_child_notes"
nowant "case 3: a stale re-fed sp-child line is not re-reported as UNLABELED" "UNLABELED sp-child " "$park_out_repeat"

# ==========================================================================================
echo
echo "case 4 — a squatter whose owning bead is CLOSED and CLEAN is freed, not parked (sp-vcxmz)"
# ==========================================================================================
git -C "$REPO" worktree add -q -b spira/sp-root3 "$SPIRA_RUN/worktree/sp-hold3" main
seed sp-root3                      # default branch spira/sp-root3, squatted by sp-hold3
seed sp-hold3
testdb_restate sp-hold3 closed     # the holder's closed row is fixture data (sp-voip5)

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
testdb_restate sp-hold4 closed     # the holder's closed row is fixture data (sp-voip5)

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
testdb_restate sp-hold5 closed     # the holder's closed row is fixture data (sp-voip5)
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
