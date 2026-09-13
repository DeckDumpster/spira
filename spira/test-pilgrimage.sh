#!/usr/bin/env bash
#
# test-pilgrimage.sh — a completed pilgrimage announces itself as an EVENT.
#
#   ./test-pilgrimage.sh
#
# The announcement leg had no suite at all, which is how it spent a day writing the wrong
# kind of bead: `PILGRIMAGE COMPLETE — <id>: <title>` went out as `ask.sh insight`, and two
# of the eleven beads in the insights queue were outcomes wearing an insight's label
# (sp-94h, hq-5enm). An insight is what an agent LEARNED and might become law; an outcome is
# what HAPPENED. They are read by different people for different reasons and they now have
# different bins.
#
# So this is not a test of pilgrimage detection — `bd epic status` already answers that. It
# is a test of what the notice IS, end to end through the real ask.sh onto a real bd, and of
# the two properties that make it safe to emit at all: it is created CLOSED, and it carries
# no labels, so it can never be claimed by an aeon as work.
#
# EVERY CASE HAS ITS NEGATIVE. An epic with an open child must emit NOTHING, and the suite
# proves the check could have seen something by running the positive first — an assertion of
# absence from a probe that was never pointed at anything is indistinguishable from a pass
# (law-absence-needs-a-positive-control).
# defect: sp-obd sp-1wzp
# covers: spira/pilgrimage.sh cockpit/ask.sh
# scar: a completed pilgrimage was filed as ask.sh insight rather than a closed event bead; outcomes and insights are read by different people for different reasons.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
eq()     { [ "$3" = "$2" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

. "$HERE/testdb.sh"
testdb_require test-pilgrimage
TMP="$(mktemp -d)"
testdb_up pilgrimage || { echo "testdb_up failed"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

DB="$SPIRA_DB"
BD="${TESTDB_BD:-bd}"
# COCKPIT_DB IS EXPORTED EXPLICITLY, never left to conf.sh's default. The operator's
# spira.conf may name COCKPIT_DB, and the environment is the only source that outranks it —
# without this line a suite on his box writes its fixtures into the live database.
export COCKPIT_DB="$DB"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"

child() {   # child <id> <status> <parent>
    printf '{"id":"%s","title":"child %s","description":"d","status":"%s","issue_type":"task","labels":["spira","plan"],"dependencies":[{"issue_id":"%s","depends_on_id":"%s","type":"parent-child"}]}\n' \
        "$1" "$1" "$2" "$1" "$3"
}
events() { "$BD" -C "$DB" list --all --limit 0 -t event --json 2>/dev/null | sed -n '/^[[{]/,$p'; }
field()  { events | python3 -c 'import json,sys;r=json.load(sys.stdin);print((r[0] if r else {}).get(sys.argv[1]) or "")' "$1"; }
n_events() { events | python3 -c 'import json,sys;print(len(json.load(sys.stdin)))'; }
status_of() { "$BD" -C "$DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
    | python3 -c 'import json,sys;d=json.load(sys.stdin);print((d[0] if isinstance(d,list) else d).get("status") or "")'; }

run() { SPIRA_DB="$DB" SPIRA_NOTIFY="$HERE/../cockpit/ask.sh" "$HERE/pilgrimage.sh" check 2>&1; }

# ======================================================================================
echo "a complete pilgrimage — the notice is an event"

testdb_seed <<JSONL
{"id":"sp-done","title":"a finished pilgrimage","description":"d","status":"open","issue_type":"epic","labels":["spira","plan"]}
$(child sp-d1 closed sp-done)
$(child sp-d2 closed sp-done)
JSONL

out="$(run)"
want "the run announces it"        "PILGRIMAGE COMPLETE" "$out"
eq   "and wrote exactly one event" "1" "$(n_events)"
eq   "of kind pilgrimage.complete" "pilgrimage.complete" "$(field event_kind)"
# The epic id lives in `event_target`, not only spelled into the title, so a reader can
# filter the stream on the thing the outcome happened TO.
eq   "targeted at the epic"        "sp-done" "$(field target)"
want "titled with what happened"   "PILGRIMAGE COMPLETE — sp-done" "$(field title)"
want "and carrying which children closed" "sp-d1" "$(field description)"

# THE TWO PROPERTIES THAT MAKE IT SAFE TO EMIT. An OPEN event carrying the plan's labels is
# claimable by an aeon, which would put a completion notice in front of a worker as work.
eq "it is created closed" "closed" "$(field status)"
eq "carrying no labels at all" "[]" \
   "$(events | python3 -c 'import json,sys;r=json.load(sys.stdin);print(json.dumps((r[0] if r else {}).get("labels") or []))')"
labels="$(events | python3 -c 'import json,sys;r=json.load(sys.stdin);print(",".join((r[0] if r else {}).get("labels") or []))')"
# Not a grep over the row: `created_by` is `overseer` on everything this harness writes.
nowant "never labelled insight — that queue is for what was LEARNED" "insight" ",$labels,"
nowant "never labelled overseer — that label is what DECISIONS matches" "overseer" ",$labels,"

ready="$("$BD" -C "$DB" ready --limit 0 --exclude-type epic --label spira,plan \
          --exclude-label spira-poison,needs-ryan --json 2>/dev/null | sed -n '/^[[{]/,$p')"
nowant "the sentinel's ready predicate cannot see it" "$(field id)" "${ready:-[]}"

eq "and the epic itself is closed once the notice is out" "closed" "$(status_of sp-done)"

out="$(run)"
nowant "a second pass announces nothing — the marker holds" "PILGRIMAGE COMPLETE" "$out"
eq    "and writes no second event" "1" "$(n_events)"

# ======================================================================================
echo
echo "an unfinished pilgrimage — silence, from a check that just proved it can speak"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-part","title":"still going","description":"d","status":"open","issue_type":"epic","labels":["spira","plan"]}
$(child sp-p1 closed sp-part)
$(child sp-p2 open   sp-part)
JSONL

out="$(run)"
nowant "no notice while a child is open" "PILGRIMAGE COMPLETE" "$out"
eq     "and no event"                    "0" "$(n_events)"
eq     "the epic stays open"             "open" "$(status_of sp-part)"

# ======================================================================================
echo
echo "an epic outside Spira's partition is not ours to announce"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-alien","title":"someone else's epic","description":"d","status":"open","issue_type":"epic","labels":["repo:town"]}
$(child sp-a1 closed sp-alien)
JSONL
out="$(run)"
nowant "an epic without the spira label is skipped" "PILGRIMAGE COMPLETE" "$out"
eq     "and nothing is written about it"            "0" "$(n_events)"
eq     "it is left open for whoever owns it"        "open" "$(status_of sp-alien)"

# ======================================================================================
echo
echo "landstate assertion — a live push-mode branch blocks the close until landing records it"
#
# The defect (sp-qj8n): sp-scbi's branch was present in the repo, its bead was closed,
# but landing.sh had never written LANDED to landstate — marker commits were lost.
# Pilgrimage closed the epic over this and left the branch permanently unlanded.
#
# The assertion: before closing an epic, every child with a live spira/* branch in a
# push-mode repo must appear in landstate as LANDED. If any is missing or non-LANDED,
# the close is deferred until the next landing pass records it.
#
# A REAL GIT REPO is used here — the assertion calls `git show-ref`, so a mock would only
# prove the check can read a file we wrote (law-prefer-the-real-dependency).

testdb_reset
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
LREPO="$TMP/lrepo"
git init -q --bare -b main "$LREPO"
git_work="$TMP/lrepo-work"
git init -q -b main "$git_work"
git -C "$git_work" commit -q --allow-empty -m base
git -C "$git_work" remote add origin "$LREPO"
git -C "$git_work" push -q origin main
git -C "$git_work" fetch -q origin

LMAP="$TMP/lrepo-map"
printf 'lrepo | %s | push | origin/main | |\n' "$git_work" > "$LMAP"

# pilgrimage.sh only READS landstate — landing.sh writes it in production.
# The test must create the directory or writes to it will silently fail.
export SPIRA_RUN="$TMP/lrun"; mkdir -p "$SPIRA_RUN/landstate"

run_repo() {
    SPIRA_DB="$DB" SPIRA_NOTIFY="$HERE/../cockpit/ask.sh" \
    SPIRA_HOME="$HERE" \
    SPIRA_HOME_REPO=lrepo \
    SPIRA_REPO="$git_work" \
    SPIRA_REPO_MAP="$LMAP" \
    SPIRA_RUN="$TMP/lrun" \
        "$HERE/pilgrimage.sh" check 2>&1
}

testdb_seed <<JSONL
{"id":"sp-assert-epic","title":"assertion epic","description":"d","status":"open","issue_type":"epic","labels":["spira","plan"]}
$(child sp-assert-c1 closed sp-assert-epic)
$(child sp-assert-c2 closed sp-assert-epic)
JSONL

# Create a branch for sp-assert-c1 in the push-mode repo but write NO landstate entry.
git -C "$git_work" worktree add -q -b "spira/sp-assert-c1" "$TMP/lrun/wt-c1" main
printf 'c1\n' > "$TMP/lrun/wt-c1/c1.txt"
git -C "$TMP/lrun/wt-c1" add -A
git -C "$TMP/lrun/wt-c1" commit -q -m "feat: sp-assert-c1 — work"

out="$(run_repo)"
want "a live branch with no landstate entry blocks the close" \
     "ASSERTION — spira/sp-assert-c1 in lrepo is live but landstate reads 'missing'" "$out"
want "and the epic is deferred"   "deferring — not all child branches are in landstate" "$out"
eq   "and stays open"             "open" "$(status_of sp-assert-epic)"

# A non-LANDED state (e.g. GATED) also blocks the close.
printf 'GATED abc123 1234567890 fixture\n' > "$SPIRA_RUN/landstate/sp-assert-c1"
out="$(run_repo)"
want "a GATED landstate entry also blocks the close" \
     "landstate reads 'GATED'" "$out"
eq   "and the epic still stays open" "open" "$(status_of sp-assert-epic)"

# Once the LANDED entry exists, the epic may close.
printf 'LANDED abc123 1234567890 lrepo\n' > "$SPIRA_RUN/landstate/sp-assert-c1"
out="$(run_repo)"
want "with a LANDED entry, the epic closes"  "PILGRIMAGE COMPLETE" "$out"
eq   "and the epic is now closed"            "closed" "$(status_of sp-assert-epic)"

# sp-assert-c2 had no branch and needed no landstate entry — the close still went through.
git -C "$git_work" worktree remove --force "$TMP/lrun/wt-c1" 2>/dev/null
git -C "$git_work" branch -q -D spira/sp-assert-c1 2>/dev/null

# A CHILD IN A PR-MODE REPO IS NOT CHECKED — landing.sh does not write landstate for pr.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-pr-epic","title":"pr-mode epic","description":"d","status":"open","issue_type":"epic","labels":["spira","plan"]}
$(child sp-pr-c1 closed sp-pr-epic)
JSONL
PRMAP="$TMP/prrepo-map"
printf 'lrepo | %s | pr | origin/main | |\n' "$git_work" > "$PRMAP"
git -C "$git_work" worktree add -q -b "spira/sp-pr-c1" "$TMP/lrun/wt-pr-c1" main
printf 'pr\n' > "$TMP/lrun/wt-pr-c1/pr.txt"
git -C "$TMP/lrun/wt-pr-c1" add -A
git -C "$TMP/lrun/wt-pr-c1" commit -q -m "feat: sp-pr-c1 — work"
# No landstate entry for sp-pr-c1.
rm -f "$SPIRA_RUN/landstate/sp-pr-c1"
out="$(SPIRA_DB="$DB" SPIRA_NOTIFY="$HERE/../cockpit/ask.sh" \
       SPIRA_HOME="$HERE" SPIRA_HOME_REPO=lrepo \
       SPIRA_REPO="$git_work" SPIRA_REPO_MAP="$PRMAP" SPIRA_RUN="$TMP/lrun" \
       "$HERE/pilgrimage.sh" check 2>&1)"
want "a pr-mode branch is not checked — the epic closes regardless" "PILGRIMAGE COMPLETE" "$out"
eq   "and the epic is closed" "closed" "$(status_of sp-pr-epic)"
git -C "$git_work" worktree remove --force "$TMP/lrun/wt-pr-c1" 2>/dev/null
git -C "$git_work" branch -q -D spira/sp-pr-c1 2>/dev/null

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
