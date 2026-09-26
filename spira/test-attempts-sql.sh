#!/usr/bin/env bash
#
# test-attempts-sql.sh — the SQL that counts attempts, reopens and reclaims from the events
#   trail, and the bulk query CHECK 4 reads it through instead of one call per bead.
#
#   ./test-attempts-sql.sh
#
# MERGED (sp-eq8a4.2.4, duplicate clusters D1/D5/D6, UC-aeon-execution-19): test-attempts.sh's
# dolt fixture (b1..b8, the _attempts_sql_query builder), test-check4-events.sh (events-trail
# counting, no counter labels), test-check4-batch.sh (dispatchable_open's shape and
# check4_bulk_data agreeing with the per-bead functions), test-timeout.sh's
# fresh-bead/one-timeout/in_progress counter rows, and test-requeue.sh's counter assertions
# (a reopened event is a requeue, not an attempt). The sentinel-INTEGRATION criteria those
# files also carried — a bead poisoning via events, CHECK 4 acting on bulk data — moved with
# check4_decide: covered by test-check4-unit.sh (T1, the decision table) and the rewritten
# test-poison.sh (the sole T3 CHECK 4 pass). Six files became two.
#
# ONE SEED, EVENTS INSERTED BY SQL rather than driven through real claim/close cycles: the
# events table is the real bd table and attempts_of/check4_bulk_data read it through real bd
# sql, so what is under test is the SQL, not the sequence of CLI calls that could produce the
# same rows. test-check4-batch.sh and test-poison-edge.sh already seed 'reopened' and
# 'requeued'/thrash rows this way; b1..b8 below follow the same precedent instead of a
# hand-rolled dolt schema (test-attempts.sh's old copy, replaced here).
#
# tier: T2
# defect: sp-lzt sp-f1m7f sp-6bop sp-qd2ul
# covers: spira/lib.sh
# timeout: 90
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "counter labels: no harness path writes one (D6)"

BANNED='sp-attempt-|sp-reclaim-|sp-reclaim$|sp-requeue-|sp-requeue$|sp-timeout-|sp-recur-'
found="$(grep -rlE "label add.*($BANNED)" "$HERE"/*.sh 2>/dev/null \
    | grep -v '/test-' | grep -v '/lib\.sh$' | grep -v '/attempts\.sh$' || true)"
is "no harness script writes counter labels directly" "" "$found"

for fn in bump_attempt bump_reclaim bump_requeue bump_timeout bump_recur; do
    body="$(sed -n "/^${fn}()/,/^}/p" "$HERE/lib.sh" 2>/dev/null)"
    has_label_add="$(grep -c 'bdq label add\|bump_counter' <<<"$body" || true)"
    is "$fn does not write a label" "0" "$has_label_add"
done

echo
echo "_attempts_sql_query: what counts as an attempt"

. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require attempts-sql
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — attempts_of/reopens_of/reclaims_of/check4_bulk_data all read the
# events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up attempts-sql || skip "server testdb not available"
. "$HERE/lib.sh"

seed() {   # seed <id> — one open, claimable bead
    testdb_seed <<JSONL
{"id":"$1","title":"a bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL
}
num() { local v="$1"; printf '%d' "${v:-0}"; }
seedn() {   # seedn <id> <event_type> <new_value> <n> — n raw events via bd sql
    local id="$1" et="$2" nv="$3" n="${4:-1}" i=0 uuid
    while [ "$i" -lt "$n" ]; do
        uuid="$(python3 -c 'import uuid; print(uuid.uuid4())')"
        bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', '$et', 'harness', '$nv', NOW())" >/dev/null 2>&1
        i=$((i+1))
    done
}
# seedt <id> <event_type> <new_value> <created_at> — one event at an EXPLICIT timestamp.
# NOW() gives every event in one test process the same second, and the poison.cleared floor
# is a strict ">" — two events tied on the same second could land on either side of it
# depending on nothing but scheduling. The before/after cases below need the ordering to be
# the thing under test, not a race against the clock.
seedt() {
    local id="$1" et="$2" nv="$3" ts="$4" uuid
    uuid="$(python3 -c 'import uuid; print(uuid.uuid4())')"
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', '$et', 'harness', '$nv', '$ts')" >/dev/null 2>&1
}

testdb_reset
testdb_seed <<'JSONL'
{"id":"b1","title":"no events","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"b3","title":"three claims, three closes","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"b4","title":"three claims, never closed","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"b5","title":"three claim+thrash pairs","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"b6","title":"two thrash pairs then one real failure","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"b7","title":"three claims each unjudged","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"b8","title":"two unjudged then one real failure","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"b9","title":"one reopen, no attempt","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"b10","title":"one reclaim","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL

# b3: THREE claims, THREE successful closes. A harness requeue is not a failed attempt:
# sp-7tj completed three groom passes and was poisoned for it because raw claims were
# counted. Expect 0.
seedn b3 claimed '' 3; seedn b3 closed '' 3
# b4: three claims, never closed. A genuinely failing bead. Expect 3 (CONTROL).
seedn b4 claimed '' 3
# b5: THREE claim+thrash pairs — the class sp-requeue-thrash reports. Expect 0.
seedn b5 claimed '' 3; seedn b5 requeued thrash 3
# b6: two thrash pairs then one real failure. Expect 1 (CONTROL).
seedn b6 claimed '' 3; seedn b6 requeued thrash 2
# b7: THREE claims, each ended by a worker that died before judging. sp-wfqha was poisoned
# this way and re-poisoned six seconds after a hand clear. Expect 0.
seedn b7 claimed '' 1; seedn b7 requeued unjudged-refused 1
seedn b7 claimed '' 1; seedn b7 requeued unjudged-refused 1
seedn b7 claimed '' 1; seedn b7 requeued unjudged-no-trace 1
# b8: two unjudged deaths then one real failure. Expect 1 (CONTROL).
seedn b8 claimed '' 1; seedn b8 requeued unjudged-refused 1
seedn b8 claimed '' 1; seedn b8 requeued unjudged-refused 1
seedn b8 claimed '' 1
# b9: one reopened event, no attempt. The harness putting finished work back is not an
# attempt at the work (test-requeue.sh's origin: "it is counted as a requeue instead").
seedn b9 reopened '' 1
# b10: one reclaimed event — the box killed the worker, the work was never judged.
seedn b10 reclaimed '' 1

is "b1: no events = 0 attempts"                                    "0" "$(num "$(attempts_of b1)")"
is "b3: three claims each closed successfully = 0 (a requeue is not an attempt)" "0" "$(num "$(attempts_of b3)")"
is "CONTROL b4: three claims and never closed = 3 attempts"        "3" "$(num "$(attempts_of b4)")"
is "b5: three claim+thrash pairs = 0 attempts (thrash is not a failure)" "0" "$(num "$(attempts_of b5)")"
is "CONTROL b6: two thrash + one real failure = 1 attempt"         "1" "$(num "$(attempts_of b6)")"
is "b7: three claims each ended unjudged = 0 (a dead worker is not a verdict)" "0" "$(num "$(attempts_of b7)")"
is "CONTROL b8: two unjudged + one real failure = 1 attempt"       "1" "$(num "$(attempts_of b8)")"
is "b9: one reopened event = 0 attempts"                           "0" "$(num "$(attempts_of b9)")"
is "b9: one reopened event = 1 reopen"                             "1" "$(num "$(reopens_of b9)")"
is "b10: one reclaimed event = 1 reclaim"                          "1" "$(num "$(reclaims_of b10)")"
is "b10: a reclaim alone is not an attempt"                        "0" "$(num "$(attempts_of b10)")"

body_attempts="$(sed -n '/^attempts_of()/,/^}/p' "$HERE/lib.sh" 2>/dev/null)"
is "attempts_of delegates to the SQL builder" "1" \
   "$(grep -c '_attempts_sql_query' <<<"$body_attempts" || true)"

echo
echo "poison.cleared: attempts_of counts only what happened after it (sp-qd2ul):"

testdb_reset
testdb_seed <<'JSONL'
{"id":"c1","title":"three failures, cleared, nothing since","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"c2","title":"three failures, cleared, one new failure","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"c3","title":"reopens and reclaims stand either side of a clear","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
{"id":"c4","title":"never cleared","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL

# c1: three claims before the clear, nothing after. THE CONTROL for the fixture itself: an
# identical bead with no clear reads 3 (c4, below) — proving the drop to 0 is the floor, not
# an empty events table.
seedt c1 claimed '' '2026-09-06 00:00:01'
seedt c1 claimed '' '2026-09-06 00:00:02'
seedt c1 claimed '' '2026-09-06 00:00:03'
seedt c1 poison.cleared operator '2026-09-06 00:01:00'
is "c1: three claims before a clear, nothing since = 0 attempts" "0" "$(num "$(attempts_of c1)")"

# c2: the same three claims and clear, plus ONE claim after it. The clear discounts the old
# three; it is not a permanent exemption, so the new claim is the whole count.
seedt c2 claimed '' '2026-09-06 00:00:01'
seedt c2 claimed '' '2026-09-06 00:00:02'
seedt c2 claimed '' '2026-09-06 00:00:03'
seedt c2 poison.cleared operator '2026-09-06 00:01:00'
seedt c2 claimed '' '2026-09-06 00:02:00'
is "c2: three claims, cleared, one new claim = 1 attempt (not 4)" "1" "$(num "$(attempts_of c2)")"

# c3: a reopened and a reclaimed event on each side of the clear. reopens_of/reclaims_of feed
# REQUEUE_AT/RECLAIM_AT, thresholds independent of spira-poison by design (check4_decide) —
# the floor must not touch them.
seedt c3 reopened  '' '2026-09-06 00:00:01'
seedt c3 reclaimed '' '2026-09-06 00:00:02'
seedt c3 poison.cleared operator '2026-09-06 00:01:00'
seedt c3 reopened  '' '2026-09-06 00:02:00'
seedt c3 reclaimed '' '2026-09-06 00:02:01'
is "c3: reopens are not floored by a poison.cleared event"   "2" "$(num "$(reopens_of c3)")"
is "c3: reclaims are not floored by a poison.cleared event"  "2" "$(num "$(reclaims_of c3)")"

# c4 (CONTROL): the same three claims as c1, never cleared. Proves c1's 0 came from the floor.
seedt c4 claimed '' '2026-09-06 00:00:01'
seedt c4 claimed '' '2026-09-06 00:00:02'
seedt c4 claimed '' '2026-09-06 00:00:03'
is "CONTROL c4: three claims, never cleared = 3 attempts" "3" "$(num "$(attempts_of c4)")"

echo
echo "poison.cleared: check4_bulk_data agrees with the per-bead functions across a clear:"

SH_C4="$TMP/spira-c4"; mkdir -p "$SH_C4/chamber"
printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison"\nFAYTH_MAX_CONCURRENT=0\n' \
    > "$SH_C4/chamber/t.fayth"
bulk_predicate() {
    SPIRA_HOME="$SH_C4" SPIRA_RUN="$TMP/run-c4" SPIRA_DB="$SPIRA_DB" SPIRA_GOAL=sp-goal \
    SPIRA_FAYTHS="t" bash -c ". \"$HERE/lib.sh\"; $1" 2>/dev/null
}
bulk_c4="$(bulk_predicate 'check4_bulk_data "c1	spira,plan
c2	spira,plan
c3	spira,plan
c4	spira,plan"')"
bulk_att_c1="$(printf '%s' "$bulk_c4" | awk -F'\t' '$1=="c1"{print $2}')"
bulk_att_c2="$(printf '%s' "$bulk_c4" | awk -F'\t' '$1=="c2"{print $2}')"
bulk_rep_c3="$(printf '%s' "$bulk_c4" | awk -F'\t' '$1=="c3"{print $3}')"
bulk_rcl_c3="$(printf '%s' "$bulk_c4" | awk -F'\t' '$1=="c3"{print $4}')"
bulk_att_c4="$(printf '%s' "$bulk_c4" | awk -F'\t' '$1=="c4"{print $2}')"
is "bulk c1 attempts match the per-bead floor (0)"     "0" "${bulk_att_c1:-0}"
is "bulk c2 attempts match the per-bead floor (1)"     "1" "${bulk_att_c2:-0}"
is "bulk c3 reopens unaffected by the floor (2)"       "2" "${bulk_rep_c3:-0}"
is "bulk c3 reclaims unaffected by the floor (2)"      "2" "${bulk_rcl_c3:-0}"
is "bulk c4 attempts match the per-bead control (3)"   "3" "${bulk_att_c4:-0}"

echo
echo "bump_poison_cleared writes no label (D6):"
body_bpc="$(sed -n '/^bump_poison_cleared()/,/^}/p' "$HERE/lib.sh" 2>/dev/null)"
is "bump_poison_cleared does not write a label" "0" \
   "$(grep -c 'bdq label add\|bump_counter' <<<"$body_bpc" || true)"

echo
echo "attempts_of / reopens_of against real bd events (positive controls, real claims):"

seed sp-ev1
is "a fresh bead with no status changes has 0 attempts" "0" "$(num "$(attempts_of sp-ev1)")"

seed sp-ev2
bdq update sp-ev2 --status in_progress >/dev/null 2>&1
is "one in_progress transition is one attempt (fresh-bead/one case, ex test-timeout.sh)" \
   "1" "$(num "$(attempts_of sp-ev2)")"
is "and no reopen is recorded alongside it"                        "0" "$(num "$(reopens_of sp-ev2)")"

# NO OVER-COUNT FROM OTHER EVENT TYPES. A label whose text contains 'in_progress' must not
# be counted — the filter on event_type='status_changed' prevents it (ex test-check4-events.sh).
seed sp-ev5
bdq label add sp-ev5 "in_progress-fake" >/dev/null 2>&1
is "a label containing 'in_progress' does not count" "0" "$(num "$(attempts_of sp-ev5)")"

echo
echo "dispatchable_open emits id TAB labels (ex test-check4-batch.sh criterion 1):"

SH="$TMP/spira"; mkdir -p "$SH/chamber"
printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison"\nFAYTH_MAX_CONCURRENT=0\n' \
    > "$SH/chamber/t.fayth"
lib_predicate() {
    SPIRA_HOME="$SH" SPIRA_RUN="$TMP/run" SPIRA_DB="$SPIRA_DB" SPIRA_GOAL=sp-goal \
    SPIRA_FAYTHS="t" bash -c ". \"$HERE/lib.sh\"; $1" 2>/dev/null
}
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-d1","title":"bead one","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"sp-d2","title":"bead two","status":"open","issue_type":"task","labels":["spira","plan","repo:spira"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL
out="$(lib_predicate dispatchable_open)"
d1_line="$(printf '%s' "$out" | grep '^sp-d1')"
d2_line="$(printf '%s' "$out" | grep '^sp-d2')"
want "sp-d1 line includes id"             "sp-d1" "$d1_line"
want "sp-d1 line has tab separator"       $'\t'   "$d1_line"
want "sp-d2 line includes repo label"     "repo:spira" "$d2_line"
is "first field of sp-d1 line is the id"  "sp-d1" "$(printf '%s' "$d1_line" | cut -f1)"

echo
echo "check4_bulk_data agrees with attempts_of/reopens_of/reclaims_of, per bead (bulk==per-bead):"

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-b1","title":"no events","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"sp-b2","title":"two attempts","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"sp-b3","title":"attempts, reopens and reclaims","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL
bdq update sp-b2 --status in_progress >/dev/null 2>&1
bdq update sp-b2 --status open >/dev/null 2>&1
bdq update sp-b2 --status in_progress >/dev/null 2>&1
bdq update sp-b2 --status open >/dev/null 2>&1
bdq update sp-b3 --status in_progress >/dev/null 2>&1
bdq update sp-b3 --status open >/dev/null 2>&1
seedn sp-b3 reopened '' 3
seedn sp-b3 reclaimed '' 2

disp="$(printf 'sp-b1\tspira,plan\nsp-b2\tspira,plan\nsp-b3\tspira,plan')"
bulk="$(check4_bulk_data "$disp")"

att_b2="$(num "$(attempts_of sp-b2)")"; att_b3="$(num "$(attempts_of sp-b3)")"
rep_b3="$(num "$(reopens_of  sp-b3)")"; rcl_b3="$(num "$(reclaims_of sp-b3)")"

bulk_att_b1="$(printf '%s' "$bulk" | awk -F'\t' '$1=="sp-b1"{print $2}')"
bulk_att_b2="$(printf '%s' "$bulk" | awk -F'\t' '$1=="sp-b2"{print $2}')"
bulk_att_b3="$(printf '%s' "$bulk" | awk -F'\t' '$1=="sp-b3"{print $2}')"
bulk_rep_b3="$(printf '%s' "$bulk" | awk -F'\t' '$1=="sp-b3"{print $3}')"
bulk_rcl_b3="$(printf '%s' "$bulk" | awk -F'\t' '$1=="sp-b3"{print $4}')"

is "sp-b1 bulk attempts is 0 (no attempt-type events)"    "0" "${bulk_att_b1:-0}"
is "sp-b2 attempts match per-bead"                        "$att_b2" "${bulk_att_b2:-0}"
is "sp-b3 attempts match per-bead"                         "$att_b3" "${bulk_att_b3:-0}"
is "sp-b3 reopens match per-bead (non-zero check)"        "$rep_b3" "${bulk_rep_b3:-0}"
is "sp-b3 has non-zero reopens (proves detectability)"    "3" "$rep_b3"
is "sp-b3 reclaims match per-bead (non-zero check, G11)"  "$rcl_b3" "${bulk_rcl_b3:-0}"
is "sp-b3 has non-zero reclaims (proves detectability)"   "2" "$rcl_b3"

tl_summary
