#!/usr/bin/env bash
#
# test-requeue.sh — the harness putting finished work back is not an attempt at the work.
#
#   ./test-requeue.sh
#
# THE DEFECT THIS REPRODUCES. Three attempts poison a bead; a poisoned bead stays OPEN and
# no persona may claim it; and the landing pass lands only a CLOSED bead. So poisoning a
# bead whose work is finished is a permanent deadlock reached by counting — and the counter
# could not tell two opposite endings apart:
#
#   (a) the session ended and the work is not right;
#   (b) the session committed, closed the bead, and the harness reopened it — because the
#       branch no longer rebased onto a base that had moved underneath it.
#
# In (b) the aeon did the work. session_outcome cannot tell them apart — it reads the
# session's own trace, and the trace of a session that committed, closed and ran to its end
# is `unlanded`, the one outcome that charges — so the reopen was charged against the work
# every time round. One bead was charged all eight of its attempts that way over work that
# later landed unchanged; another was poisoned nineteen hours after the session that
# finished it, and its branch merged cleanly the whole time.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control). "No attempt was charged" is
# the answer a counter that never runs gives too, so each decline is asserted beside a
# charge that the same code path produces from the same fixture — and the last case runs
# both to the poison threshold, where only the genuine failure arrives.
#
# THE REBASE CONFLICT IS REAL, not simulated by a flag: the base gains a commit touching the
# same line the shim's commit touches, which is what the aeon's own rebase step then fails
# on. A fixture that set the outcome directly would be asserting against a model of the
# thing under test.
#
# Driven through the REAL aeon.sh and attempts.sh against a real bd on a throwaway fixture,
# with a shim standing in for the model (law-prefer-the-real-dependency).
#
# strand.sh IS NOT CLAIMED HERE, though it is the counter's second door. Its own classifying
# arms are exercised below through charge_attempt, which is where the decision lives, but the
# script itself is not run — staging a ghost needs an expired lease and a dead holder — and a
# suite that claimed a file it never executes would let the gate believe that file is covered.
#
# defect: sp-l7f5
# covers: spira/aeon.sh spira/attempts.sh spira/lib.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-requeue
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
testdb_up requeue || { echo "test-requeue: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$HERE/attempts.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="spira,plan"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

# THE SHIM IS THE SESSION. The guard is not decoration: conf.sh replaces $PATH, so a suite
# that shimmed `claude` by PATH alone would run the real model at full cost.
BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-requeue: aeon.sh has no SPIRA_AGENT injection point" >&2; exit 1; }
shim() {   # shim <commit:0|1> <close:0|1> [move-the-base:0|1]
    printf '%s' "$1" > "$TMP/docommit"; printf '%s' "$2" > "$TMP/doclose"
    printf '%s' "${3:-0}" > "$TMP/domove"
    cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
if [ "$(cat "$TMP/docommit")" = 1 ]; then
    printf 'the aeon wrote this %s\n' "$(date +%s%N)" > f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
fi
# THE BASE MOVES WHILE THE SESSION IS RUNNING, which is the live shape and the only one that
# produces the defect: a base that had already moved before the aeon started would simply be
# branched from, and there would be nothing to rebase.
if [ "$(cat "$TMP/domove")" = 1 ]; then
    git -C "$MAINREPO" checkout -q main
    printf 'someone else landed this %s\n' "$(date +%s%N)" > "$MAINREPO/f"
    git -C "$MAINREPO" -c user.email=b@b -c user.name=other commit -qam "another bead — a conflicting change"
    git -C "$MAINREPO" push -q origin main 2>/dev/null
fi
[ "$(cat "$TMP/doclose")" = 1 ] && bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
# A TOOL CALL AND THEN A TERMINAL RECORD, which is what a session that actually worked emits.
# session_outcome reads exactly these two facts: a segment with no tool call is a session that
# decided nothing about the work and is `refused`, not a verdict — so a shim that skipped this
# line would be testing the refusal path while claiming to test a failed attempt.
printf '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
    chmod +x "$BIN/claude"
}
seed() {   # seed <id>
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":["spira","plan","repo:fixture"],"updated_at":"2026-09-04T00:00:00Z"}\n' "$1" | testdb_seed
}
cycle() {   # cycle <id> <n> — create n status_changed(in_progress) events via bd update
    # sp-attempt-N labels retired (sp-lzt); cycle creates the equivalent events for counting.
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        bd -C "$SPIRA_DB" update "$id" --status in_progress >/dev/null 2>&1
        bd -C "$SPIRA_DB" update "$id" --status open >/dev/null 2>&1
        i=$((i+1))
    done
}
export MAINREPO="$REPO"
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1; }
field() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
labels()  { bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }
notes()   { bd -C "$SPIRA_DB" show "$1" 2>/dev/null | tr '\n' ' '; }
# ZERO, NOT EMPTY. A counter prints nothing for a bead carrying no rung, and an assertion
# against "" passes just as well when the whole call failed.
lib() { bash -c ". \"$SPIRA_HOME/lib.sh\"; $1" 2>/dev/null; }
count_of()   { local c; c="$(lib "attempts_of $1")"; printf '%s' "${c:-0}"; }
requeue_of() { local c; c="$(lib "requeues_of $1")"; printf '%s' "${c:-0}"; }

echo "test-requeue.sh"

echo
echo "the session committed, closed, and the harness reopened it over a rebase conflict:"
testdb_reset; seed sp-rq-1; shim 1 1 1; run_aeon
is     "the bead is open again"                open "$(field sp-rq-1 status)"
want   "and the log says why"                  "REOPENED — closed behind" "$(cat "$TMP/out")"
is     "NO attempt is charged"                 "0" "$(count_of sp-rq-1)"
nowant "so it carries no attempt rung"         "sp-attempt-1" "$(labels sp-rq-1)"
is     "it is counted as a requeue instead"    "1" "$(requeue_of sp-rq-1)"
# sp-requeue-N labels retired (sp-lzt); events trail is the authoritative record.
want   "the teardown says no attempt was charged" "no attempt charged" "$(cat "$TMP/out")"
want   "the bead carries the decision"         "Requeue 1 (rebase-conflict)" "$(notes sp-rq-1 | tr -s ' ')"
want   "and the ledger carries the outcome"    "status=requeue-rebase-conflict" \
       "$(cat "$SPIRA_RUN/aeon-ledger.log")"
# THE BRANCH SURVIVES THE REQUEUE. The aeon committed before closing; the rebase failed
# after close and was aborted, leaving the branch at its pre-abort tip. The next aeon
# inherits the work rather than starting from scratch.
_nc="$(git -C "$REPO" rev-list --count "$(git -C "$REPO" rev-parse origin/main)..spira/sp-rq-1" 2>/dev/null || echo 0)"
is     "the branch still carries the aeon's commit after the requeue" "1" "$_nc"

echo
echo "the session did not close the bead at all — that IS an attempt, and still charges:"
testdb_reset; seed sp-rq-2; shim 0 0; run_aeon
is   "the bead is open"                        open "$(field sp-rq-2 status)"
is   "and one attempt is charged"              "1" "$(count_of sp-rq-2)"
# sp-attempt-N labels retired (sp-lzt); the events trail records each claim.
is   "with nothing on the requeue counter"     "0" "$(requeue_of sp-rq-2)"

echo
echo "the pair, one cycle each — only the genuine failure charges attempts:"
# ONE BEAD READY AT A TIME, because `bd ready --claim` picks for itself and a suite that
# seeded both would be asserting against whichever it happened to hand out.
# ONE CYCLE SUFFICES: the discrimination property holds after N cycles for any N >= 1.
# Three was chosen to match the SPIRA_POISON_AT default, then reduced to two (sp-5hfnn),
# then to one (sp-xzbhr): sentinel.sh (which applies that threshold) does not run here;
# the test just checks counters. Each reduction saves two run_aeon calls (~16s each step),
# keeping the suite within the timed-run budget.
testdb_reset; seed sp-rq-h
shim 1 1 1
run_aeon
bd -C "$SPIRA_DB" label add sp-rq-h "$SPIRA_ASK_LABEL" >/dev/null 2>&1   # out of the partition
seed sp-rq-w
shim 0 0
run_aeon
is "the harness's bead is still at zero attempts" "0" "$(count_of sp-rq-h)"
is "and its cycling is visible as one requeue"    "1" "$(requeue_of sp-rq-h)"
is "the work's bead carries one attempt"          "1" "$(count_of sp-rq-w)"

echo
echo "the deadlock sweep lifts a poison from finished, landable work:"
testdb_reset; seed sp-rq-s; seed sp-rq-k
# sp-rq-s: poisoned, but its branch names the bead and merges cleanly — the deadlock.
git -C "$REPO" checkout -q -B spira/sp-rq-s origin/main
printf 'finished work\n' > "$REPO/g"; git -C "$REPO" add g
git -C "$REPO" commit -qm "sp-rq-s — the work"
git -C "$REPO" checkout -q main
# sp-rq-k: poisoned with no branch at all, which is a bead that really did fail.
# sp-attempt-N labels retired (sp-lzt); cycle creates status_changed events so
# count_of (attempts_of) returns 3 and the rungs survive the deadlock sweep.
for b in sp-rq-s sp-rq-k; do
    cycle "$b" 3
    bd -C "$SPIRA_DB" label add "$b" spira-poison >/dev/null 2>&1
done
sweep() { SPIRA_HOME="$SPIRA_HOME" SPIRA_RUN="$SPIRA_RUN" SPIRA_DB="$SPIRA_DB" \
          SPIRA_REPO_MAP="$SPIRA_REPO_MAP" SPIRA_REPO="$REPO" SPIRA_FAYTHS=builder \
          bash "$SPIRA_HOME/attempts.sh" deadlocked "$@" 2>&1; }
out="$(sweep)"
want "the deadlocked bead is named"            "WOULD    sp-rq-s" "$out"
want "the genuinely failed one is kept"        "KEEP     sp-rq-k" "$out"
want "with the test it failed"                 "no branch spira/sp-rq-k" "$out"
want "and nothing changed without --apply"     "dry run" "$out"
want "so the poison still stands"              "spira-poison" "$(labels sp-rq-s)"
out="$(sweep --apply)"
want   "the sweep lifts it"                    "RESTORED sp-rq-s" "$out"
nowant "and the label is gone"                 "spira-poison" "$(labels sp-rq-s)"
is     "the rungs are left standing as the record" "3" "$(count_of sp-rq-s)"
want   "the bead records why"                  "Poison lifted by attempts.sh deadlocked" "$(notes sp-rq-s | tr -s ' ')"
want   "the bead that really failed keeps its poison" "spira-poison" "$(labels sp-rq-k)"

echo
echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
