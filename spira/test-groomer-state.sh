#!/usr/bin/env bash
# timeout: 150
#
# test-groomer-state.sh — groomer sweep no longer runs the whole-graph landstate STATE
#   remedies (sp-jnwbn).
#
# HISTORY. sp-0qp7s gave the groomer's sweep three STATE passes that read landing state
# straight from git/landstate: landed-but-open (close), closed-no-branch (spira-dropped),
# closed-never-landed conflict|batch-ready (reopen), then blocked-by-unlanded notes over
# whatever got reopened. The 2026-10-04 lifecycle cutover deleted them: lifecycle LANDED
# supersedes them, and the sentinel's CHECK5-LC reports the same three drifts from the
# spira-lc rows. The landed-but-open sweep had closed the cutover bead (sp-sa8pn) itself.
#
# THE CASE. The exact fixture the old sweep acted on — an open bead whose commit is on the
# base, a closed bead with no branch: label, two closed beads whose branches never landed
# (one conflicting, one clean), and an open bead blocked by the conflicting one — is left
# exactly as seeded, and groom.log records none of the four remedies. Against the pre-
# cutover groomer every assertion in the REAL RUN block fails (seen red, sp-jnwbn).
#
# REAL GIT REPO, REAL TESTDB (law-prefer-the-real-dependency): landed()/content_landed()
# read git ancestry and merge-tree, which a stub cannot stand in for without becoming a
# second implementation of git.
#
# tier: T2
# defect: sp-0qp7s sp-jnwbn
# covers: groomer/src/* spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-groomer-state
TMP="$(mktemp -d)"
testdb_up state || { echo "test-groomer-state: could not build fixture database"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'exit 143' INT TERM

. "$HERE/testlib.sh"

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; mkdir -p "$RUN"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main

# PINNED TO A NON-DEFAULT NAME so the suite cannot pass on an accidentally matching literal.
REPONAME=statetestrepo
printf '%s | %s | push | main | |\n' "$REPONAME" "$REPO" > "$TMP/repo-map"

run_sweep() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_BD="${SPIRA_BD:-bd}" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_RUN="$RUN" \
        SPIRA_REPO_MAP="$TMP/repo-map" \
        SPIRA_ASK_LABEL=needs-ryan \
        SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        SPIRA_SCOPE_LABEL=spira \
        groomer sweep "$@" 2>&1
}

status_of() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | python3 -c '
import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("status",""))' 2>/dev/null; }
labels_of() { bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }
notes_of()  { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | python3 -c '
import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("notes") or "")' 2>/dev/null; }

echo "test-groomer-state.sh"

# ==========================================================================================
echo
echo "FIXTURE"
# ==========================================================================================
testdb_reset

# Case 1: LANDED-BUT-OPEN — sp-st-lbo's own commit is already on the base.
git -C "$REPO" commit -q --allow-empty -m "sp-st-lbo: implement the thing"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# Case 3: CLOSED-NEVER-LANDED — two branches cut from the CURRENT base tip.
BASE_SHA="$(git -C "$REPO" rev-parse origin/main)"
git -C "$REPO" branch -q sp-st-conflict "$BASE_SHA"
git -C "$REPO" worktree add -q "$TMP/wt-conflict" sp-st-conflict
printf 'conflict\n' > "$TMP/wt-conflict/CONFLICT_FILE"
git -C "$TMP/wt-conflict" add CONFLICT_FILE
git -C "$TMP/wt-conflict" commit -q -m "sp-st-conflict: work in progress"
git -C "$REPO" worktree remove --force "$TMP/wt-conflict"
# Land a DIFFERENT change to the same path on main, so the branch conflicts.
printf 'main-side\n' > "$REPO/CONFLICT_FILE"
git -C "$REPO" add CONFLICT_FILE
git -C "$REPO" commit -q -m "unrelated: also touches CONFLICT_FILE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

git -C "$REPO" branch -q sp-st-ready "$BASE_SHA"
git -C "$REPO" worktree add -q "$TMP/wt-ready" sp-st-ready
printf 'ready\n' > "$TMP/wt-ready/READY_FILE"
git -C "$TMP/wt-ready" add READY_FILE
git -C "$TMP/wt-ready" commit -q -m "sp-st-ready: clean work"
git -C "$REPO" worktree remove --force "$TMP/wt-ready"

testdb_seed <<JSONL
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":["spira"]}
{"id":"sp-st-lbo","title":"landed but still open","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"dependencies":[{"issue_id":"sp-st-lbo","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-st-openok","title":"genuinely open, no commit anywhere","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"dependencies":[{"issue_id":"sp-st-openok","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-st-nobranch","title":"closed, no branch label","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"dependencies":[{"issue_id":"sp-st-nobranch","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-st-succ","title":"successor, still open","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"dependencies":[{"issue_id":"sp-st-succ","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-st-supr","title":"closed, superseded, no branch label","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"dependencies":[{"issue_id":"sp-st-supr","depends_on_id":"sp-epic","type":"parent-child"},{"issue_id":"sp-st-supr","depends_on_id":"sp-st-succ","type":"supersedes"}]}
{"id":"sp-st-conflict","title":"closed, branch conflicts with base","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME","branch:sp-st-conflict"],"dependencies":[{"issue_id":"sp-st-conflict","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-st-ready","title":"closed, branch merges cleanly","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME","branch:sp-st-ready"],"dependencies":[{"issue_id":"sp-st-ready","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-st-livehold","title":"closed, batch-ready, but claim still live","status":"closed","issue_type":"task","assignee":"aeon-live","labels":["spira","plan","repo:$REPONAME","branch:sp-st-ready"],"dependencies":[{"issue_id":"sp-st-livehold","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-st-releasedhold","title":"closed, batch-ready, claim already released","status":"closed","issue_type":"task","assignee":"","labels":["spira","plan","repo:$REPONAME","branch:sp-st-ready"],"dependencies":[{"issue_id":"sp-st-releasedhold","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-st-cl","title":"closed, content-landed label","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME","content-landed"],"dependencies":[{"issue_id":"sp-st-cl","depends_on_id":"sp-epic","type":"parent-child"}]}
{"id":"sp-st-blocked","title":"blocked by the conflicting closed bead","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"dependencies":[{"issue_id":"sp-st-blocked","depends_on_id":"sp-epic","type":"parent-child"},{"issue_id":"sp-st-blocked","depends_on_id":"sp-st-conflict","type":"blocks"}]}
JSONL

# ==========================================================================================
echo
echo "REAL RUN — groomer sweep applies none of the retired STATE remedies"
# ==========================================================================================
: > "$RUN/groom.log"
out="$(run_sweep)"
is "sweep exits 0" 0 "$?"

is     "sp-st-lbo (its commit is on the base) is left open"   open   "$(status_of sp-st-lbo)"
is     "sp-st-openok stays open"                              open   "$(status_of sp-st-openok)"
nowant "sp-st-nobranch is NOT labeled spira-dropped"          "spira-dropped" "$(labels_of sp-st-nobranch)"
is     "sp-st-conflict is NOT reopened"                       closed "$(status_of sp-st-conflict)"
is     "sp-st-ready is NOT reopened"                          closed "$(status_of sp-st-ready)"
is     "sp-st-releasedhold is NOT reopened"                   closed "$(status_of sp-st-releasedhold)"
nowant "sp-st-blocked gets no false-blocker note"             "blocked-by-unlanded" "$(notes_of sp-st-blocked)"

groom_log="$(cat "$RUN/groom.log" 2>/dev/null)"
for kind in landed-but-open closed-no-branch closed-never-landed blocked-by-unlanded; do
    nowant "groom.log carries no $kind remedy" "$kind" "$groom_log"
    nowant "sweep output carries no $kind remedy" "$kind" "$out"
done

echo
tl_summary
