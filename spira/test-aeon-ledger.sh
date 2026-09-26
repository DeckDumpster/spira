#!/usr/bin/env bash
#
# test-aeon-ledger.sh — the `done` ledger line says what the session SPENT, and says `?`
#   rather than 0 for anything it could not read. session_result_fields (lib.sh) is the
#   parser; trace_segment (lib.sh) is what lets this suite hand it a NAMED attempt's own
#   bytes from a fixture file instead of running a real aeon session to produce one.
#
# WHAT IS UNDER TEST. Every session's terminal `result` record carries its duration, its
# turns, its token usage and its cost, and that trace is the only copy anyone keeps. Putting
# those on the aeon ledger's disposition line turns "where did the hours go" and "what does a
# landed bead cost" into an awk one-liner over one small file, instead of a purpose-built
# script over tens of megabytes of traces.
#
# WHY IT NEEDS A SUITE AT ALL. The failure mode is silent by construction: a parser that
# stops finding the record does not error, it renders — and if it rendered 0 the ledger would
# report a fleet of free, instantaneous aeons, which is a reassuring number and therefore the
# worst possible one. So `?` and 0 are asserted as DIFFERENT answers on every field, and the
# case that produces each is driven end to end.
#
# THE BOUNDARY IS THE OTHER HALF. The trace is appended to across attempts, so a session that
# died before speaking sits directly beneath a previous attempt's complete `result` record.
# Reading the file rather than the attempt's own segment would bill the dead session for the
# live one's tokens. That case stays a real two-attempt aeon run (T3 below) because what it
# proves is the WIRING — that aeon.sh's own marks and attempt_trace's backward scan agree
# with trace_segment's forward one — which no fixture file assembled by hand can prove.
#
# defect: sp-214
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-ledger.sh"

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
echo "T1: session_result_fields on fixture trace files — no aeon run, no bd"
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
echo "T1: trace_segment <log> <attempt> — the boundary trace_segment itself is responsible for"
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

# ===========================================================================================
echo
echo "T3: the real boundary, via a real aeon — attempt 2 reads its OWN segment"
# ===========================================================================================
# testdb-mode: default (embedded). What only a real aeon run can prove: that aeon.sh's own
# marks (spira_trace_mark) and attempt_trace's backward scan land on the same boundary
# trace_segment's forward scan does above.
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-ledger
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonledger || { echo "test-aeon-ledger: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-ledger: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
if [ "$(cat "$TMP/docommit")" = 1 ]; then
    printf 'my work\n' >> f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
fi
[ "$(cat "$TMP/doclose")" = 1 ] && bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
cat "$TMP/result"
exit 0
SHIM
chmod +x "$BIN/claude"

session() {   # session <commit:0|1> <close:0|1>  — the next session's shape; result on stdin
    printf '%s' "$1" > "$TMP/docommit"
    printf '%s' "$2" > "$TMP/doclose"
    cat > "$TMP/result"
}
seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1; }
done_line() { grep ' done ' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null | tail -1; }

testdb_reset; seed sp-lg-5
session 1 0 <<< "$FULL"; run_aeon          # ran, spent, left the bead open
first="$(done_line)"
session 0 0 <<< ""; run_aeon               # never spoke
second="$(done_line)"
want   "attempt 1 is a full reading"          "turns=7"      "$first"
want   "and it cost what the record said"     "cost_usd=1.3475" "$first"
nowant "attempt 2 does not inherit its turns"  "turns=7"     "$second"
nowant "nor its cost"                          "cost_usd=1.3475" "$second"
want   "attempt 2 reads as unknown"            "turns=?"     "$second"
want   "and unknown on the cost"               "cost_usd=?"  "$second"
# NOT A CLOSED BEAD. The status is read before the claim is released, so an attempt that
# ended without finishing reads in_progress — and the fields must ride that line too, or
# the spend of every session that did NOT finish would be the spend nobody could see.
want   "and the fields ride a disposition that is not a close" "status=in_progress" "$second"

tl_summary
