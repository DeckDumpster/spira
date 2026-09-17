#!/usr/bin/env bash
#
# test-slay.sh — slay.sh stops one aeon cleanly, removes its work, and makes
# the bead say what is true.
#
#   ./test-slay.sh
#
# THE PROPERTY UNDER TEST. An operator kills an aeon and the harness must
# reflect what actually happened: the bead is released, any uncommitted work is
# committed as a wip and left on the branch in refs/heads (mode=reopen), and no
# attempt is charged. The next aeon's brief names the slain attempt. Every
# absence assertion has a presence assertion beside it so an empty result cannot
# be mistaken for correct behaviour (law-absence-needs-a-positive-control).
#
# A REAL bd ON A THROWAWAY DATABASE, because every claim is about what bd does
# with a status, a label, a claim and an assignee. A git fixture with a bare
# remote for branches and worktrees, because every claim about the work is
# about what git says exists (law-prefer-the-real-dependency).
#
# defect: sp-ekio
# covers: spira/slay.sh spira/lib.sh
# scar: slay.sh left the bead in an inconsistent state after terminating the aeon and did not salvage uncommitted work from the worktree.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-slay
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up slay || { echo "test-slay: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN/worktree"
export SPIRA_REPO="$REPO"
export SPIRA_CONF="$TMP/no-such-conf"
export SPIRA_GOAL=sp-goal
export SPIRA_REPO_MAP="$TMP/repo-map"
printf '# fixture — empty\n' > "$TMP/repo-map"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main

# shellcheck disable=SC1090
. "$HERE/lib.sh"
SLAY="$HERE/slay.sh"

# ---- helpers -----------------------------------------------------------------------

status_of() { bdjson show "$1" | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("status","") if d else "")' 2>/dev/null; }

assignee_of() { bdjson show "$1" | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("assignee","") or "" if d else "")' 2>/dev/null; }

has_label() { bdq label list "$1" 2>/dev/null | grep -qF "$2"; }

seed() {
    local id="$1" st="${2:-in_progress}" as="${3:-aeon-test}"
    testdb_reset
    local line; line="{\"id\":\"$id\",\"title\":\"test bead\",\"status\":\"$st\",\"issue_type\":\"task\",\"labels\":[\"spira\",\"plan\"]"
    [ -n "$as" ] && line="$line,\"assignee\":\"$as\""
    line="$line,\"updated_at\":\"2026-09-06T00:00:00Z\"}"
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-06T00:00:00Z"}
$line
JSONL
}

make_work() {
    local id="$1"
    local br="spira/$id" wt="$SPIRA_RUN/worktree/$id"
    git -C "$REPO" branch -q "$br" main 2>/dev/null || true
    git -C "$REPO" worktree add -q "$wt" "$br" 2>/dev/null
    echo "work for $id" > "$wt/work.txt"
    git -C "$wt" add work.txt
    git -C "$wt" commit -q -m "sp-$id work"
}

teardown() {
    local id="$1"
    local wt="$SPIRA_RUN/worktree/$id"
    git -C "$REPO" worktree remove --force "$wt" 2>/dev/null || true
    rm -rf "$wt"
    git -C "$REPO" branch -D "spira/$id" 2>/dev/null || true
    git -C "$REPO" update-ref -d "refs/slain/$id" 2>/dev/null || true
    rm -rf "$SPIRA_RUN/reaped" "$SPIRA_RUN"/aeon-*-"$id".pid
}

# ======================================================================================
# STRUCTURAL: the slain marker is the first action.
#
# The marker must be written before anything is stopped, because aeon.sh's exit path
# reads it to decide whether to charge an attempt. If the kill arrives before the marker,
# the exit path sees a normal 143 and charges — and three charges poison the bead.
# ======================================================================================
echo "structural:"

first_action="$(sed -n '/^# ---- 1\. the marker/,/^# ---- 2\./p' "$SLAY" \
                | grep -vE '^\s*(#|$)' | head -1)"
want "the marker is the first action" 'slain' "$first_action"

# ======================================================================================
# DEFAULT SLAY (reopen) — bead goes from in_progress to open, unassigned, work removed.
# The bead, the worktree and the branch are all checked. A slayer that touches nothing
# would pass every absence check, so the presence check comes first.
# ======================================================================================
echo
echo "default slay (reopen):"

seed sp-s1
make_work sp-s1
is "bead starts in_progress"   in_progress "$(status_of sp-s1)"
is "bead starts assigned"      aeon-test   "$(assignee_of sp-s1)"

out="$(bash "$SLAY" --bead sp-s1 2>&1)"
rc=$?
is  "slay exits 0"             0    "$rc"
is  "bead is now open"         open "$(status_of sp-s1)"
is  "bead is unassigned"       ""   "$(assignee_of sp-s1)"
is  "worktree is gone"         no   "$([ -d "$SPIRA_RUN/worktree/sp-s1" ] && echo yes || echo no)"
is  "branch stays in refs/heads" 0  "$(git -C "$REPO" show-ref --verify -q refs/heads/spira/sp-s1 2>/dev/null; echo $?)"
is  "marker is cleaned up"     no   "$([ -f "$SPIRA_RUN/sp-s1.slain" ] && echo yes || echo no)"
want "reports slain"            "slain: sp-s1" "$out"
teardown sp-s1

# ======================================================================================
# SLAY --close — bead is closed with the reason and the spira-dropped label.
#
# The label is what keeps the sentinel from reopening it within two minutes: CHECK 5
# reopens any closed bead whose id does not appear on the base branch, and slain work
# has no commit naming it.
# ======================================================================================
echo
echo "slay --close:"

seed sp-s2
make_work sp-s2

out="$(bash "$SLAY" --bead sp-s2 --close "operator decided to drop this" 2>&1)"
rc=$?
is "slay --close exits 0"   0      "$rc"
is "bead is closed"          closed "$(status_of sp-s2)"
if has_label sp-s2 spira-dropped; then ok "bead has spira-dropped label"
else bad "bead has spira-dropped label" "label not found"; fi
teardown sp-s2

# ======================================================================================
# SLAY --keep-work — branch and worktree survive.
# ======================================================================================
echo
echo "slay --keep-work:"

seed sp-s3
make_work sp-s3

out="$(bash "$SLAY" --bead sp-s3 --keep-work 2>&1)"
rc=$?
is   "slay --keep-work exits 0"   0   "$rc"
is   "bead is open"               open "$(status_of sp-s3)"
is   "worktree is kept"           yes  "$([ -d "$SPIRA_RUN/worktree/sp-s3" ] && echo yes || echo no)"
is   "branch is kept"             0    "$(git -C "$REPO" show-ref --verify -q refs/heads/spira/sp-s3 2>/dev/null; echo $?)"
want "reports work kept"           "kept" "$out"
teardown sp-s3

# ======================================================================================
# SALVAGE — uncommitted changes are saved before the worktree goes.
# ======================================================================================
echo
echo "salvage:"

seed sp-s4
make_work sp-s4
echo "uncommitted work" > "$SPIRA_RUN/worktree/sp-s4/unsaved.txt"
git -C "$SPIRA_RUN/worktree/sp-s4" add unsaved.txt

out="$(bash "$SLAY" --bead sp-s4 2>&1)"
rc=$?
is "slay with dirty worktree exits 0" 0 "$rc"
salvaged="$(ls "$SPIRA_RUN/reaped"/sp-s4.*.patch 2>/dev/null | head -1)"
if [ -n "$salvaged" ]; then ok "uncommitted changes salvaged to a patch"
else bad "uncommitted changes salvaged to a patch" "no patch found in $SPIRA_RUN/reaped/"; fi
_wip_msg="$(git -C "$REPO" log --format='%s' -n 1 spira/sp-s4 2>/dev/null)"
want "dirty worktree produces wip commit" "wip — salvaged at slay" "$_wip_msg"
is   "branch stays in refs/heads after dirty slay" 0 \
     "$(git -C "$REPO" show-ref --verify -q refs/heads/spira/sp-s4 2>/dev/null; echo $?)"
teardown sp-s4

# ======================================================================================
# PARKING — a branch with unique work is parked at refs/slain/<id> so that a gc cannot
# collect it. The pair: a branch whose commits are already on main is NOT parked, because
# parking those would fill the namespace with refs nobody will ever read.
# ======================================================================================
echo
echo "parking:"

seed sp-s5
make_work sp-s5

out="$(bash "$SLAY" --bead sp-s5 2>&1)"
is   "branch with unique work stays in refs/heads" 0 \
     "$(git -C "$REPO" show-ref --verify -q refs/heads/spira/sp-s5 2>/dev/null; echo $?)"
is   "branch with unique work is not parked at refs/slain" 1 \
     "$(git -C "$REPO" show-ref --verify -q refs/slain/sp-s5 2>/dev/null; echo $?)"
want "reports branch kept"  "kept" "$out"
teardown sp-s5

# THE PAIR: a branch at main carries nothing worth keeping — it is deleted, not kept.
seed sp-s6
git -C "$REPO" branch -q spira/sp-s6 main 2>/dev/null
git -C "$REPO" worktree add -q "$SPIRA_RUN/worktree/sp-s6" spira/sp-s6 2>/dev/null

out="$(bash "$SLAY" --bead sp-s6 2>&1)"
is     "zero-ahead branch is deleted"  1 \
       "$(git -C "$REPO" show-ref --verify -q refs/heads/spira/sp-s6 2>/dev/null; echo $?)"
is     "zero-ahead branch is not parked" 1 \
       "$(git -C "$REPO" show-ref --verify -q refs/slain/sp-s6 2>/dev/null; echo $?)"
nowant "does not report parking"       "parked" "$out"
teardown sp-s6

# ======================================================================================
# SLAY --close on an already-closed bead — a note is added rather than a second close
# attempted (which bd would refuse).
# ======================================================================================
echo
echo "slay --close on already-closed bead:"

seed sp-s7 closed ""

out="$(bash "$SLAY" --bead sp-s7 --close "second close" 2>&1)"
rc=$?
is "slay --close on closed bead exits 0" 0      "$rc"
is "bead is still closed"                closed "$(status_of sp-s7)"


# ======================================================================================
# REFUSALS — a destructive tool must fail closed on a target it never found.
#
# Both of these were real and both fired on the same command on 2026-09-13:
#   bash slay.sh --bead sp-gjpc "blocked on the P0 fixes"
# The second positional silently REPLACED the bead id, the run proceeded against a bead
# named after the sentence, found nothing, printed "slain: <sentence>" and exited 0.
# Nothing was slain and the operator was told three times that something had been.
# ======================================================================================
echo
echo "refusals:"

# --- a bead id no bead carries -----------------------------------------------------
out="$(bash "$SLAY" --bead sp-nosuchbead9 --why probe 2>&1)"; rc=$?
is   "bogus id exits 2"                    2 "$rc"
want "bogus id names the store"            "refusing to act" "$out"
nowant "bogus id does not report a slaying" "slain: sp-nosuchbead9" "$out"
is   "bogus id leaves no .slain marker"    no \
     "$([ -f "$SPIRA_RUN/sp-nosuchbead9.slain" ] && echo yes || echo no)"

# --- a reason passed where the id used to go ---------------------------------------
# There are no positionals at all now, so the sentence is refused as an unexpected
# argument rather than mistaken for a second id. The message must name BOTH flags,
# because the operator who typed this wanted --why and reached for the wrong shape.
seed sp-s8
out="$(bash "$SLAY" --bead sp-s8 "blocked on the P0 fixes" 2>&1)"; rc=$?
is   "a positional argument exits 2"       2 "$rc"
want "refuses it by name"                  'unexpected argument' "$out"
want "says the tool takes named arguments" 'named arguments only' "$out"
want "points at --bead"                    '--bead' "$out"
want "points at --why"                     '--why' "$out"
is   "the real bead was NOT touched"       in_progress "$(status_of sp-s8)"

# --- --bead twice is also refused ---------------------------------------------------
out="$(bash "$SLAY" --bead sp-s8 --bead sp-s9 2>&1)"; rc=$?
is   "--bead twice exits 2"                2 "$rc"
want "names both values"                   'given twice' "$out"

# --- -h lists every argument --------------------------------------------------------
# It was `sed -n '2,12p' $0`, a fixed line range of the header: editing the header
# silently truncated the help, which is how a flag comes to be undocumented.
out="$(bash "$SLAY" -h 2>&1)"; rc=$?
is   "-h exits 0"                          0 "$rc"
for _f in --bead --why --keep-work --close --reopen; do
    want "-h documents $_f"                "$_f" "$out"
done
want "-h gives the exit codes"             'EXIT' "$out"

# --- POSITIVE CONTROL --------------------------------------------------------------
# Without this, a slay.sh that refused EVERYTHING would pass both cases above.
out="$(bash "$SLAY" --bead sp-s8 --keep-work --why "positive control" 2>&1)"; rc=$?
is   "a real id with a --why still slays"  0    "$rc"
is   "and the bead is released"            open "$(status_of sp-s8)"
teardown sp-s8

# ======================================================================================
# WIP COMMIT — dirty worktree leaves a wip commit on the branch in refs/heads.
# acceptance (a): slay of a dirty worktree leaves a wip commit on spira/<id> in
# refs/heads and the bead open.
# acceptance (c): clean worktree produces no wip commit.
# ======================================================================================
echo
echo "wip commit (dirty worktree — acceptance a):"

seed sp-wip1
git -C "$REPO" branch -q spira/sp-wip1 main 2>/dev/null || true
git -C "$REPO" worktree add -q "$SPIRA_RUN/worktree/sp-wip1" spira/sp-wip1 2>/dev/null
echo "uncommitted" > "$SPIRA_RUN/worktree/sp-wip1/dirty.txt"

out="$(bash "$SLAY" --bead sp-wip1 2>&1)"; rc=$?
is "slay dirty exits 0"                  0    "$rc"
is "bead is open"                        open "$(status_of sp-wip1)"
is "branch stays in refs/heads"          0    "$(git -C "$REPO" show-ref --verify -q refs/heads/spira/sp-wip1 2>/dev/null; echo $?)"
_wip="$(git -C "$REPO" log --format='%s' -n 1 spira/sp-wip1 2>/dev/null)"
want "last commit is wip"                "wip — salvaged at slay" "$_wip"
is   "worktree is gone"                  no   "$([ -d "$SPIRA_RUN/worktree/sp-wip1" ] && echo yes || echo no)"
teardown sp-wip1

echo
echo "no wip commit (clean worktree — acceptance c):"

seed sp-wip2
make_work sp-wip2

out="$(bash "$SLAY" --bead sp-wip2 2>&1)"; rc=$?
is "slay clean exits 0"                  0    "$rc"
is "bead is open"                        open "$(status_of sp-wip2)"
is "branch stays in refs/heads"          0    "$(git -C "$REPO" show-ref --verify -q refs/heads/spira/sp-wip2 2>/dev/null; echo $?)"
_clean="$(git -C "$REPO" log --format='%s' -n 1 spira/sp-wip2 2>/dev/null)"
nowant "no wip commit on clean worktree" "wip — salvaged at slay" "$_clean"
teardown sp-wip2

# ======================================================================================
# SLAIN BRIEF (acceptance b) — the next summon's brief contains the slain-attempt
# section with the diffstat and the transcript path.
# Tests slain_attempt_brief() from lib.sh, which aeon.sh calls on each summon.
# ======================================================================================
echo
echo "slain brief (next summon — acceptance b):"

seed sp-wip3
git -C "$REPO" branch -q spira/sp-wip3 main 2>/dev/null || true
git -C "$REPO" worktree add -q "$SPIRA_RUN/worktree/sp-wip3" spira/sp-wip3 2>/dev/null
echo "some work" > "$SPIRA_RUN/worktree/sp-wip3/work.txt"
git -C "$SPIRA_RUN/worktree/sp-wip3" add work.txt
git -C "$SPIRA_RUN/worktree/sp-wip3" commit -q -m "sp-wip3: real work"
echo "unfinished" > "$SPIRA_RUN/worktree/sp-wip3/dirty.txt"

bash "$SLAY" --bead sp-wip3 >/dev/null 2>&1  # leaves wip commit on branch

_brief="$(slain_attempt_brief "$REPO" spira/sp-wip3 origin/main sp-wip3 "$SPIRA_RUN" "$SPIRA_RUN/worktree/sp-wip3")"
if [ -n "$_brief" ]; then ok "brief is non-empty after a slain dirty worktree"
else bad "brief is non-empty after a slain dirty worktree" "slain_attempt_brief returned nothing"; fi
want "brief contains slain-attempt heading" "Prior attempt was slain" "$_brief"
want "brief contains diffstat"              "work.txt"                "$_brief"
want "brief contains wip note"             "salvaged wip commit"      "$_brief"
want "brief contains transcript path"       "sp-wip3.log"             "$_brief"
teardown sp-wip3

# PAIR: no wip commit means no slain brief.
seed sp-wip4
make_work sp-wip4
bash "$SLAY" --bead sp-wip4 >/dev/null 2>&1
_brief2="$(slain_attempt_brief "$REPO" spira/sp-wip4 origin/main sp-wip4 "$SPIRA_RUN" "$SPIRA_RUN/worktree/sp-wip4")"
if [ -z "$_brief2" ]; then ok "no slain brief when last commit is not a wip"
else bad "no slain brief when last commit is not a wip" "brief was non-empty: $_brief2"; fi
teardown sp-wip4

# ======================================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
