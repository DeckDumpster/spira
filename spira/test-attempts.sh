#!/usr/bin/env bash
#
# test-attempts.sh — what counts as an attempt at the work, and what only counts as a worker
# that died.
#
#   ./test-attempts.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. Beads were poisoned without their work having been tried
# once. Three separate defects composed into it, and each has a case below:
#
#   1. `bd unclaim --if-assignee` was passed the FAYTH's name while the claim records the
#      AEON's, so the compare-and-swap could never match and a dying aeon never released its
#      bead.
#   2. The teardown ran under `set -e`, so that failing unclaim ended the shell inside its own
#      EXIT trap — after the counter had been bumped, before anything was logged.
#   3. strand.sh then reclaimed the ghost and bumped the SAME counter again. One death, two of
#      the three attempts, and the third summon poisoned the bead.
#
# The counter is now two counters, which is the property under test throughout: poison must
# measure the work and nothing else. And the charging rule is default-DENY — only an outcome
# that names what the WORK did wrong may charge — because every failure mode that cost the
# most was unenumerated when it fired, so a list of exemptions could not have saved any of
# them.
#
# A REAL bd ON A THROWAWAY DATABASE, because every claim here is a claim about what bd does
# with a label, a lease and a compare-and-swap. A stub would be a second implementation of the
# one thing being asked about (law-prefer-the-real-dependency).
#
# defect: sp-sc3
# covers: spira/*.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# ======================================================================================
# session_outcome — what ended this session? Pure text over a trace file, so it runs with no
# database at all. Only `unlanded` may charge an attempt; every other answer, INCLUDING the
# one that means "we cannot tell", is about the worker.
# ======================================================================================
TMP="$(mktemp -d)"
export SPIRA_DB="${SPIRA_DB:-$TMP/no-such-db}" SPIRA_RUN="$TMP/run"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "session_outcome:"

: > "$TMP/empty.log"
is "an empty trace is a session that never ran" refused "$(session_outcome "$TMP/empty.log")"

# NOT `refused`. A trace that is not there and a session that wrote nothing are different
# facts, and collapsing them would make a mis-aimed log path indistinguishable from a healthy
# harness reading a quiet one (law-absence-needs-a-positive-control). Both decline to charge;
# only the recorded cause tells a human which happened.
is "a missing trace is not classified at all" unknown "$(session_outcome "$TMP/no-such.log")"
is "an unnamed trace is not classified at all" unknown "$(session_outcome "")"

# The verbatim shape of a refusal: a synthetic assistant message and a result carrying an
# api_error_status, seconds after the claim.
cat > "$TMP/ratelimit.log" <<'LOG'
{"type":"system","subtype":"init","session_id":"x"}
{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"session limit reached"}]},"error":"rate_limit"}
{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"session limit reached"}
LOG
is "a rate-limited session is not an attempt" refused "$(session_outcome "$TMP/ratelimit.log")"

# A session that acted and then ended badly at the API is still not a verdict about the work:
# the terminal record says the account refused it, and that is a fact about the account.
cat > "$TMP/worked-then-refused.log" <<'LOG'
{"type":"system","subtype":"init","session_id":"x"}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}
{"type":"result","subtype":"success","is_error":true,"api_error_status":429}
LOG
is "a session refused after it acted is still not an attempt" refused "$(session_outcome "$TMP/worked-then-refused.log")"

cat > "$TMP/clean.log" <<'LOG'
{"type":"system","subtype":"init","session_id":"x"}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{}}]}}
{"type":"result","subtype":"success","is_error":false,"num_turns":40}
LOG
is "a session that ran to its own end and left the bead open IS an attempt" unlanded "$(session_outcome "$TMP/clean.log")"

# THE CLI NOW EMITS "api_error_status":null ON EVERY RESULT RECORD. A session that ran real
# work and returned null for api_error_status is NOT a refusal — null means "no API error
# occurred". Classifying on the key's presence (not its value) caused every completed session
# to read as refused, blocking all attempt charges (defect: sp-1g37h).
cat > "$TMP/null-status.log" <<'LOG'
{"type":"system","subtype":"init","session_id":"x"}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}
{"type":"result","subtype":"success","is_error":false,"api_error_status":null}
LOG
is "a session with api_error_status:null is an attempt, not a refusal" unlanded "$(session_outcome "$TMP/null-status.log")"

# A trace with tool calls and no terminal record is a session that was killed part-way — the
# host died, the cgroup was torn down, or its worktree was deleted under it. Nothing in it is
# a verdict about the bead.
cat > "$TMP/killed.log" <<'LOG'
{"type":"system","subtype":"init","session_id":"x"}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Now I will"}]}}
LOG
is "a session killed mid-work is not an attempt" killed "$(session_outcome "$TMP/killed.log")"

cat > "$TMP/truncated.log" <<'LOG'
{"type":"system","subtype":"init","session_id":"x"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Let me look at the bead."}]}}
LOG
is "a truncated trace with no tool call is not an attempt" refused "$(session_outcome "$TMP/truncated.log")"

# THE TRACE IS APPENDED TO ACROSS ATTEMPTS, so the classifier must read the LAST SEGMENT and
# not the file. Without that, a session refused before it wrote anything inherits the previous
# attempt's terminal `result` — which reads as `unlanded` and charges the refusal as a verdict
# about the work. That is default-allow restored through the back door, in exactly the case
# the whole rule exists for, and nothing about it would look wrong: the count just grows.
#
# THE PAIR IS THE POINT. The same two segments read as `unlanded` while the first is the last
# one, so a green result here cannot be a classifier that answers `refused` to everything
# (law-absence-needs-a-positive-control).
cp "$TMP/clean.log" "$TMP/appended.log"
is "one finished segment reads as an attempt" unlanded "$(session_outcome "$TMP/appended.log")"
printf '%s 2 aeon-t
' "$SPIRA_TRACE_MARK" >> "$TMP/appended.log"
is "a second segment that never spoke is not" refused "$(session_outcome "$TMP/appended.log")"
# And a second segment that DID run to its own end is judged on its own terms, not the first's.
cat >> "$TMP/appended.log" <<'LOG'
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}
{"type":"result","subtype":"success","is_error":true,"api_error_status":429}
LOG
is "and a refused second segment reads as refused" refused "$(session_outcome "$TMP/appended.log")"

# THE CHARGING RULE ITSELF, stated once over every outcome the classifier can return. This is
# the assertion that has to fail if someone adds an outcome and forgets to decide about it:
# default-deny is only default-deny while this list is exhaustive.
charges() { if outcome_charges "$1"; then echo charges; else echo free; fi; }
is "only a work verdict charges"     charges "$(charges unlanded)"
is "a refusal does not charge"       free    "$(charges refused)"
is "a killed worker does not charge" free    "$(charges killed)"
is "and UNKNOWN does not charge"     free    "$(charges unknown)"
is "nor does an outcome nobody has enumerated" free "$(charges something-new)"

# ======================================================================================
# THE TEARDOWN MUST RUN TO ITS END. Structural, and deliberately so: the regression was that
# a LATER line failed, so nothing short of reading the first line proves the guard is where it
# has to be. The behaviour it guards is exercised right below it.
# ======================================================================================
echo
echo "aeon teardown:"

first="$(sed -n '/^cleanup() {/,/^}/p' "$HERE/aeon.sh" | tail -n +2 \
          | grep -vE '^\s*(#|local |$)' | sed -n 1p)"
is "cleanup disarms errexit before anything can fail" "    set +e" "$first"

# The hazard itself, so the assertion above is not a rule nobody can see fire: under `set -e`
# a failing command inside an EXIT trap ends the shell where it stands, and every later step
# of the teardown is skipped in silence.
cat > "$TMP/hazard.sh" <<'H'
set -uo pipefail
cleanup() { echo ENTERED; false; echo LEDGER; }
trap cleanup EXIT
set -e
H
is "errexit ends a trap mid-teardown" "ENTERED" "$(bash "$TMP/hazard.sh" 2>/dev/null)"
cat > "$TMP/guarded.sh" <<'H'
set -uo pipefail
cleanup() { set +e; echo ENTERED; false; echo LEDGER; }
trap cleanup EXIT
set -e
H
is "set +e first lets it finish" "ENTERED
LEDGER" "$(bash "$TMP/guarded.sh" 2>/dev/null)"

# ======================================================================================
# COUNTER LABELS DELETED (sp-lzt). Attempts are now computed from the events trail —
# each status_changed event with new_value containing 'in_progress' is one attempt.
# No harness script writes sp-attempt-*, sp-reclaim-*, sp-requeue-*, sp-timeout-*, or
# sp-recur-* labels. The bump_* functions are no-ops.
#
# Two structural properties replace the original "two doors" count:
#   1. No harness script writes a counter label (not even through bump_counter).
#   2. The REQUEUE_CAUSE path in aeon.sh still exits before session_outcome is
#      consulted — a reopened bead is not charged an attempt.
# ======================================================================================
echo
echo "counter labels deleted — structural properties:"

BANNED='sp-attempt-|sp-reclaim-|sp-requeue-|sp-timeout-|sp-recur-'
found="$(grep -rlE "label add.*($BANNED)" "$HERE"/*.sh 2>/dev/null \
    | grep -v '/test-' | grep -v '/lib\.sh$' | grep -v '/attempts\.sh$' || true)"
is "no harness script writes counter labels directly" "" "$found"

for fn in bump_attempt bump_reclaim bump_requeue bump_timeout bump_recur; do
    body="$(sed -n "/^${fn}()/,/^}/p" "$HERE/lib.sh" 2>/dev/null)"
    has_label_add="$(grep -c 'bdq label add\|bump_counter' <<<"$body" || true)"
    is "$fn is a no-op — does not write a label" "0" "$has_label_add"
done

# The REQUEUE_CAUSE exemption must still be decided before session_outcome is consulted.
# A bead that was put back by the harness must not be charged an attempt.
before="$(grep -n 'REQUEUE_CAUSE" \]; then' "$HERE/aeon.sh" | sed -n 1p | cut -d: -f1)"
after="$(grep -n 'cause="\$(session_outcome' "$HERE/aeon.sh" | sed -n 1p | cut -d: -f1)"
is "the requeue path exits before the trace is classified" yes \
   "$( [ -n "$before" ] && [ -n "$after" ] && [ "$before" -lt "$after" ] && echo yes || echo no)"

# BEHAVIOUR, NOT THE QUERY STRING. The original assertion here checked that the SQL
# contained the word 'status_changed'. It passed while the predicate returned 0 for every
# bead an aeon had ever worked, because an aeon claim writes event_type='claimed' and only
# a hand-driven `bd update --status in_progress` writes 'status_changed'. A test that
# restates the implementation agrees with it about everything, including its mistakes.
#
# So: run the query against a fixture holding one of each event shape and assert the NUMBER.
body_sql="$(sed -n '/^_attempts_sql_query()/,/^}/p' "$HERE/lib.sh" 2>/dev/null)"
is "attempts counts an aeon claim" "1" \
   "$(grep -c "event_type='claimed'" <<<"$body_sql" || true)"
is "attempts also counts a hand-driven in_progress transition" "1" \
   "$(grep -c 'status_changed' <<<"$body_sql" || true)"

# THE b1..b8 FIXTURE (hand-rolled dolt schema, one row per attempt-counting scenario:
# harness-requeue vs genuine failure, thrash pairs, unjudged deaths) moved to
# test-attempts-sql.sh (sp-eq8a4.2.4), seeded through real bd events instead of a copy of
# the events schema — the same fixture, testing the real store rather than a model of it.
#
# The note says no attempt was charged; the counter reads events, so the branch must write one.
body_unjudged="$(grep -n 'Not judged (' "$HERE/aeon.sh" | head -1 | cut -d: -f1)"
is "aeon.sh's not-judged branch records an unjudged requeue event" "1" \
   "$(sed -n "$(( ${body_unjudged:-1} - 6 )),${body_unjudged:-1}p" "$HERE/aeon.sh" | grep -c 'bump_requeue "\$BEAD_ID" "unjudged-' || true)"

# ======================================================================================
# The counters and the release, against a real bd.
# ======================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-attempts
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — attempts_of (its own subject) reads the events table via bd sql, which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up attempts || {
    printf 'SKIP test-attempts: server testdb not available\n' >&2
    exit 77
}

seed() {   # seed <id> — one open, claimable bead
    testdb_reset
    testdb_seed <<JSONL
{"id":"$1","title":"a bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL
}
status_of() { bdjson show "$1" | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("status","") if d else "")' 2>/dev/null; }
num() { local v="$1"; printf '%d' "${v:-0}"; }

echo
echo "counters (real bd) — events-based, no labels written:"

seed sp-c1
# THE POSITIVE CONTROL. A fresh bead with no status changes has zero attempts.
is "a fresh bead has no attempts"       0 "$(num "$(attempts_of sp-c1)")"
is "reclaims_of is a diagnostic stub"  0 "$(num "$(reclaims_of sp-c1)")"
is "requeues_of is a diagnostic stub"  0 "$(num "$(requeues_of sp-c1)")"

# bump_* must not write any label. After bump_attempt the bead has no sp-attempt-* labels.
bump_attempt sp-c1 unlanded
labels_c1="$(bdq label list sp-c1 2>/dev/null)" || labels_c1=""
[[ "$labels_c1" != *"sp-attempt"* ]] && ok "bump_attempt writes no label" \
    || bad "bump_attempt writes no label" "got [$labels_c1]"

# attempts_of reads events, not labels. One bd update to in_progress is one attempt.
bdq update sp-c1 --status in_progress >/dev/null 2>&1
is "one in_progress transition counts as 1" 1 "$(num "$(attempts_of sp-c1)")"

# AND THE OTHER HALF: genuine failure still poisons via events. Three in_progress events
# reach the threshold.
poisons() { local n; n="$(num "$(attempts_of "$1")")"; [ "$n" -ge 3 ] && echo yes || echo no; }
seed sp-c2
bdq update sp-c2 --status in_progress >/dev/null 2>&1
bdq update sp-c2 --status open >/dev/null 2>&1
bdq update sp-c2 --status in_progress >/dev/null 2>&1
bdq update sp-c2 --status open >/dev/null 2>&1
is "two events do not poison yet" no "$(poisons sp-c2)"
bdq update sp-c2 --status in_progress >/dev/null 2>&1
is "three events poison" yes "$(poisons sp-c2)"

# ======================================================================================
# attempts.sh reclassify — the store starts empty (sp-lzt deleted counter labels).
# No sp-attempt-N labels exist in a fresh store; reclassify and prune-reclaims find
# nothing to do. The tool must still behave correctly on an empty candidate set.
# ======================================================================================
echo
echo "attempts.sh reclassify — on a store with no counter labels:"

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/attempts.sh" "$SPIRA_HOME/"
printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison"\nFAYTH_MAX_CONCURRENT=0\n' \
    > "$SPIRA_HOME/chamber/t.fayth"
ATT="$SPIRA_HOME/attempts.sh"

# The audit scans for sp-attempt-N labels (no cause suffix). With bump_* as no-ops,
# none are ever written; the store should be clean.
seed sp-h1
out_audit="$("$ATT" audit 2>&1)"
[[ "$out_audit" != *"sp-h1"* ]] && ok "audit finds no beads with legacy attempt labels" \
    || bad "audit finds no beads with legacy attempt labels" "got [$out_audit]"

# Reclassify with nothing to do must print 'nothing to reclassify' and exit 0.
out_rcl="$("$ATT" reclassify --apply 2>&1)"
case "$out_rcl" in *"nothing to reclassify"*) ok "reclassify --apply on clean store exits cleanly" ;;
                   *) bad "reclassify --apply on clean store exits cleanly" "got [$out_rcl]" ;; esac

# prune-reclaims requires an explicit bead id (no sweep mode). A fresh bead has no
# sp-reclaim-N-unrecorded labels; it should print 'nothing to prune'.
seed sp-p1
out_prn="$("$ATT" prune-reclaims sp-p1 --apply 2>&1)"
case "$out_prn" in *"nothing to prune"*) ok "prune-reclaims on clean bead exits cleanly" ;;
                   *) bad "prune-reclaims on clean bead exits cleanly" "got [$out_prn]" ;; esac

# NO IDs = error, not a sweep.
if "$ATT" prune-reclaims 2>/dev/null; then r=0; else r=1; fi
is "prune-reclaims with no args exits non-zero" 1 "$r"

echo
echo "the release (real bd):"

# THE DISCRIMINATING FACT, seen both ways. The claim records BEADS_ACTOR; the teardown must
# name that same actor or the compare-and-swap can never match.
seed sp-r1
BEADS_ACTOR=aeon-cindy bdq update sp-r1 --claim >/dev/null 2>&1
is "the claim records the aeon, not the fayth" in_progress "$(status_of sp-r1)"

if bdq unclaim sp-r1 --if-assignee aeon-builder >/dev/null 2>&1; then r=0; else r=1; fi
is "releasing as the fayth fails"        1           "$r"
is "and leaves the bead held"            in_progress "$(status_of sp-r1)"

if bdq unclaim sp-r1 --if-assignee aeon-cindy >/dev/null 2>&1; then r=0; else r=1; fi
is "releasing as the aeon succeeds"      0    "$r"
is "and the bead is claimable again"     open "$(status_of sp-r1)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
