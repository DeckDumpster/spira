#!/usr/bin/env bash
# timeout: 150
#
# test-groomer-state.sh — groomer sweep reads bead STATE across the whole graph, not
#   just its own trigger partition (sp-0qp7s).
#
# THE ROOT CAUSE. Before this, chamber/groomer.md told the groomer to "read all open beads
# in the partition you own" and its per-bead checklist was text-only (title, duplicates,
# premise, lane) — never STATE. A bead landed-but-open, closed-but-never-landed, or blocked
# by a closed-but-unlanded bead sat that way forever because nothing ever looked.
#
# FOUR CASES, each a pair with its negative control (law-absence-needs-a-positive-control):
#
#   1. LANDED-BUT-OPEN — an open bead whose repo already carries a commit naming it
#      ("<id>: ...") is closed, citing the commit. PAIRED with an open bead with no such
#      commit, left alone.
#   2. CLOSED-NO-BRANCH — a closed bead with no branch: label is labeled spira-dropped.
#      PAIRED with a closed, superseded bead (a recognised landing signal), left alone.
#   3. CLOSED-NEVER-LANDED — a closed bead whose branch: label names a real branch ahead of
#      the base is REOPENED: conflict (does not merge) or batch-ready (merges cleanly), each
#      noting which. PAIRED with a closed, content-landed bead, left alone. A batch-ready
#      bead that still carries its closing aeon's assignee is left closed instead — that is
#      aeon.sh's own close -> work-close-converted teardown window, not a stranded bead,
#      PAIRED with the same shape once the assignee has been released.
#   4. FALSE BLOCKER — an open bead that depends (type=blocks) on the conflict-case bead
#      above is noted once that blocker is reopened.
#
# REAL GIT REPO, REAL TESTDB (law-prefer-the-real-dependency): content-on-base and the
# merge-tree clean check read git ancestry and merge-tree, which a stub cannot stand in for
# without becoming a second implementation of git. Only `spira-lc state` is stubbed.
#
# tier: T2
# defect: sp-0qp7s
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

# "LANDED" IS THE LIFECYCLE RECORD'S STATE (sp-oqf8c): strand asks `spira-lc state <id>`, not a
# commit subject. The stub answers `state` from $LCSTATE/<id> and hands every other verb to
# the real spira-lc, so only the landed-ness question is pinned.
LCSTATE="$TMP/lcstate"; mkdir -p "$LCSTATE" "$TMP/lcbin"
REAL_LC="$(command -v spira-lc)"
printf '#!/usr/bin/env bash\nif [ "${1:-}" = state ]; then [ -s "%s/${2:-}" ] && cat "%s/${2:-}"; exit 0; fi\nexec "%s" "$@"\n' \
    "$LCSTATE" "$LCSTATE" "$REAL_LC" > "$TMP/lcbin/spira-lc"
chmod +x "$TMP/lcbin/spira-lc"
PATH="$TMP/lcbin:$PATH"

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

# Case 1: LANDED-BUT-OPEN — sp-st-lbo's record is LANDED and its own commit is on the base.
git -C "$REPO" commit -q --allow-empty -m "sp-st-lbo: implement the thing"
echo LANDED > "$LCSTATE/sp-st-lbo"
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
echo "REAL RUN — groomer sweep applies the whole-graph STATE remedies"
# ==========================================================================================
: > "$RUN/groom.log"
out="$(run_sweep)"
is "sweep exits 0" 0 "$?"

# ---- Case 1: LANDED-BUT-OPEN ----
is    "sp-st-lbo landed-but-open is closed"     closed  "$(status_of sp-st-lbo)"
want  "close reason cites landed-but-open"      "landed-but-open" "$(bd -C "$SPIRA_DB" show sp-st-lbo --json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("close_reason") or "")')"
is    "sp-st-openok (no commit anywhere) stays open" open "$(status_of sp-st-openok)"

# ---- Case 2: CLOSED-NO-BRANCH ----
want  "sp-st-nobranch gets spira-dropped"        "spira-dropped" "$(labels_of sp-st-nobranch)"
nowant "sp-st-supr (superseded) is NOT dropped"  "spira-dropped" "$(labels_of sp-st-supr)"

# ---- Case 3: CLOSED-NEVER-LANDED ----
is    "sp-st-conflict is reopened"    open  "$(status_of sp-st-conflict)"
is    "sp-st-ready is reopened"       open  "$(status_of sp-st-ready)"
want  "sp-st-conflict note says rebase" "rebase" "$(notes_of sp-st-conflict)"
want  "sp-st-ready note says batch-ready" "batch-ready" "$(notes_of sp-st-ready)"
is    "sp-st-cl (content-landed) stays closed" closed "$(status_of sp-st-cl)"
is    "sp-st-livehold (live claim) stays closed" closed "$(status_of sp-st-livehold)"
is    "sp-st-releasedhold (released claim) is reopened" open "$(status_of sp-st-releasedhold)"

# ---- Case 4: FALSE BLOCKER ----
want  "sp-st-blocked is noted about the false blocker" "blocked-by-unlanded" "$(notes_of sp-st-blocked)"
is    "sp-st-blocked itself is left open (not force-unblocked)" open "$(status_of sp-st-blocked)"

# ---- groom.log carries the actions ----
groom_log="$(cat "$RUN/groom.log" 2>/dev/null)"
want "groom.log names the landed-but-open close" "CLOSED sp-st-lbo" "$groom_log"
want "groom.log names the dropped bead"          "DROPPED sp-st-nobranch" "$groom_log"
want "groom.log names both reopens"              "REOPENED sp-st-conflict" "$groom_log"
want "groom.log names both reopens (ready)"       "REOPENED sp-st-ready" "$groom_log"

echo
tl_summary
