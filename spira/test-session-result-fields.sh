#!/usr/bin/env bash
#
# test-session-result-fields.sh — session_result_fields and trace_segment (lib.sh), the
# pure parser and boundary-finder behind the aeon ledger's `done` spend fields. Split out
# of test-aeon-ledger.sh (sp-gcx3k, docs/test-plan/aeon-execution.md D15): these two
# functions need no aeon run and no bd, so they no longer pay for the real two-attempt
# aeon run that only the trace-segment BOUNDARY case needs — that one case is now
# test-aeon-teardown-e2e.sh's "ledger segment boundary" row, proving aeon.sh's own marks
# and attempt_trace's backward scan agree with trace_segment's forward one.
#
# WHY IT NEEDS A SUITE AT ALL. The failure mode is silent by construction: a parser that
# stops finding the record does not error, it renders — and if it rendered 0 the ledger
# would report a fleet of free, instantaneous aeons, which is a reassuring number and
# therefore the worst possible one. So `?` and 0 are asserted as DIFFERENT answers on
# every field, and the case that produces each is driven end to end.
#
# defect: sp-214
# tier: T1
# covers: spira/lib.sh UC-aeon-execution-18
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-session-result-fields.sh"

srf() {   # srf <file> -> session_result_fields's output over the real lib.sh
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; session_result_fields "$2"' _ "$HERE" "$1" 2>/dev/null
}
tseg() {   # tseg <file> <attempt> -> trace_segment's own bytes
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; trace_segment "$2" "$3"' _ "$HERE" "$1" "$2" 2>/dev/null
}

# A COMPLETE RECORD, WITH NO ROUND NUMBERS IN IT. Every figure below is one the shipped code
# cannot produce by accident: the durations are not whole seconds, so truncation and rounding
# are told apart; the cost has more decimals than it is rendered with; and no two fields
# share a value, so a parser reading the wrong key is a failure rather than a coincidence.
FULL='{"type":"result","subtype":"success","is_error":false,"duration_ms":90480,"duration_api_ms":61400,"num_turns":7,"total_cost_usd":1.3474715,"usage":{"input_tokens":1234,"cache_creation_input_tokens":105984,"cache_read_input_tokens":456789,"output_tokens":2222,"output_tokens_details":{"thinking_tokens":333}},"result":"done"}'

# ===========================================================================================
echo
echo "session_result_fields on fixture trace files — no aeon run, no bd"
# ===========================================================================================

f="$TMP/full.log"; printf '%s\n' "$FULL" > "$f"
line="$(srf "$f")"
want "wall clock, in seconds"          "wall_s=90"            "$line"
want "of which the API held"           "api_s=61"             "$line"
want "the turn count"                  "turns=7"              "$line"
want "fresh input tokens"              "in_tok=1234"          "$line"
want "cache reads, the figure that predicts a limit" "cache_read_tok=456789" "$line"
want "output tokens"                   "out_tok=2222"         "$line"
want "of which thinking"               "think_tok=333"        "$line"
want "and what it cost"                "cost_usd=1.3475"      "$line"

echo
echo "a session that died before writing a result — every field is ? and none is 0:"
f="$TMP/empty.log"; : > "$f"
line="$(srf "$f")"
want   "wall clock is unknown"          "wall_s=?"          "$line"
want   "and so is the API time"         "api_s=?"           "$line"
want   "and the turn count"             "turns=?"           "$line"
want   "and every token count"          "in_tok=? cache_read_tok=? out_tok=? think_tok=?" "$line"
want   "and the cost"                   "cost_usd=?"        "$line"
# THE ASSERTION THE WHOLE SUITE IS FOR. A zero here would read as a session that ran and cost
# nothing, which is a plausible sentence and a false one, and it would average into a cost
# per bead that looks best exactly when the harness is failing hardest.
nowant "nothing renders as a zero cost"  "cost_usd=0"        "$line"
nowant "nor as zero tokens"              "in_tok=0"          "$line"
nowant "nor as an instantaneous session" "wall_s=0"          "$line"

echo
echo "no log at all (empty path) — the same eight keys, all ?:"
line="$(srf "")"
is "unmapped-repo has no log yet, still renders all eight keys" \
   "wall_s=? api_s=? turns=? in_tok=? cache_read_tok=? out_tok=? think_tok=? cost_usd=?" "$line"

echo
echo "a record missing some figures — the missing ones alone are ?, beside real numbers:"
# A client that does not report a duration has been seen in the wild beside one that does, so
# "the trace had no result" and "this record did not carry that field" are different answers
# and only the second may leave its neighbours readable.
f="$TMP/partial.log"
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"num_turns":4,"total_cost_usd":0.5,"usage":{"input_tokens":11,"output_tokens":22}}' > "$f"
line="$(srf "$f")"
want "the turn count it did carry"       "turns=4"           "$line"
want "the cost it did carry"             "cost_usd=0.5000"   "$line"
want "the token counts it did carry"     "in_tok=11"         "$line"
want "and the output tokens"             "out_tok=22"        "$line"
want "the duration it did NOT carry"     "wall_s=?"          "$line"
want "nor the API duration"              "api_s=?"           "$line"
want "nor the cache reads"               "cache_read_tok=?"  "$line"
want "nor the thinking tokens"           "think_tok=?"       "$line"

echo
echo "several result records in one segment — all records contribute to the session total:"
# A session woken by a task notification emits a second result record for only that turn.
# Per-turn fields are summed across all records; cost comes from the last (cumulative) record.
f="$TMP/multi.log"
{ printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"duration_api_ms":1000,"num_turns":1,"total_cost_usd":0.01,"usage":{"input_tokens":1,"cache_read_input_tokens":1,"output_tokens":1,"output_tokens_details":{"thinking_tokens":1}}}'
  printf '%s\n' "$FULL"; } > "$f"
line="$(srf "$f")"
want   "both records' turns are summed"  "turns=8"    "$line"
nowant "neither record alone"            "turns=7"    "$line"
want   "cost from the last record"       "cost_usd=1.3475" "$line"
want   "wall_s sums both records"        "wall_s=91"  "$line"

echo
echo "wake-up notification adds a second result record — wall_s covers the whole session:"
# THE BUG THIS SUITE GUARDS AGAINST. A session woken by a task notification emits a second
# result record for just that turn. The last record alone would show wall_s=4 api_s=387 — an
# impossible combination (api_s > wall_s) proving the fields are from different scopes.
# After the fix, summing duration_ms gives the session total and the contradiction cannot occur.
MAIN='{"type":"result","subtype":"success","is_error":false,"duration_ms":1480000,"duration_api_ms":386600,"num_turns":21,"total_cost_usd":4.2700,"usage":{"input_tokens":15000,"cache_read_input_tokens":5415000,"output_tokens":15040,"output_tokens_details":{"thinking_tokens":5000}},"result":"done"}'
WAKEUP='{"type":"result","subtype":"success","is_error":false,"duration_ms":4000,"duration_api_ms":387000,"num_turns":1,"total_cost_usd":4.2746,"usage":{"input_tokens":3,"cache_read_input_tokens":133587,"output_tokens":42,"output_tokens_details":{"thinking_tokens":0}},"result":"done"}'
f="$TMP/wakeup.log"
{ printf '%s\n' "$MAIN"; printf '%s\n' "$WAKEUP"; } > "$f"
line="$(srf "$f")"
want   "wall_s reflects the whole session"      "wall_s=1484"    "$line"
nowant "wall_s is not just the wake-up turn"    "wall_s=4"       "$line"
want   "api_s from the last record (cumulative)" "api_s=387"     "$line"
want   "turns sum both records"                  "turns=22"      "$line"
want   "cost from the last record's cumulative"  "cost_usd=4.2746" "$line"
# THE IMPOSSIBILITY GUARD. wall_s < api_s is structurally impossible for a sequential
# session, so if the output contains api_s=387 it must not also contain wall_s=4.
want   "wall_s=1484 is greater than api_s=387"  "wall_s=1484"    "$line"

# ===========================================================================================
echo
echo "trace_segment <log> <attempt> — the boundary trace_segment itself is responsible for"
# ===========================================================================================
MARK='=== spira attempt'
SEG1_BODY='{"type":"result","subtype":"success","num_turns":7}'
SEG2_BODY='{"type":"result","subtype":"success","num_turns":99}'
f="$TMP/marked.log"
{ printf '%s 1 aeon=a at=t kept=0\n' "$MARK"
  printf '%s\n' "$SEG1_BODY"
  printf '%s 2 aeon=a at=t kept=999\n' "$MARK"
  printf '%s\n' "$SEG2_BODY"; } > "$f"

s1="$(tseg "$f" 1)"
s2="$(tseg "$f" 2)"
want   "attempt 1's segment carries its own mark"     "$MARK 1" "$s1"
want   "attempt 1's segment carries its own body"     "$SEG1_BODY" "$s1"
nowant "attempt 1's segment does not reach attempt 2" "$SEG2_BODY" "$s1"
want   "attempt 2's segment carries its own mark"     "$MARK 2" "$s2"
want   "attempt 2's segment carries its own body"     "$SEG2_BODY" "$s2"
nowant "attempt 2's segment does not carry attempt 1" "$SEG1_BODY" "$s2"

f2="$TMP/unmarked.log"; printf '%s\n' "$FULL" > "$f2"
is "no mark at all — attempt 1 is the whole legacy file" \
   "$FULL" "$(tseg "$f2" 1)"
is "asking for attempt 2 of an unmarked file is empty (there is no second attempt)" \
   "" "$(tseg "$f2" 2)"

is "asking past the last real attempt is empty" "" "$(tseg "$f" 3)"

tl_summary
