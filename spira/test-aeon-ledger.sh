#!/usr/bin/env bash
#
# test-aeon-ledger.sh — the `done` ledger line says what the session SPENT, and says `?`
# rather than 0 for anything it could not read.
#
#   ./test-aeon-ledger.sh
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
# live one's tokens, and nothing downstream could tell. Attempt 2 is therefore asserted to be
# unknown while attempt 1, in the same file, is asserted to be a full reading — the positive
# control without which "it says ?" proves only that the parser is broken
# (law-absence-needs-a-positive-control).
#
# Driven through the REAL aeon.sh against a real bd on a throwaway fixture, with a shim
# standing in for the model, because what is under test is what the harness records about a
# session it ran — and a model of aeon.sh would be a second implementation of the thing in
# question (law-prefer-the-real-dependency).
#
# defect: sp-214
# covers: spira/aeon.sh spira/lib.sh
# timeout: 180
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
testdb_require test-aeon-ledger
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
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

# THE SHIM IS THE SESSION. It emits whatever this suite has put in $TMP/result — zero, one or
# several records, verbatim — which is the whole point: the records a real client writes are
# the input under test, and they are not something aeon.sh can be asked to produce on demand.
#
# The guard below is not decoration: conf.sh replaces $PATH, so a suite that tried to shim
# `claude` by PATH alone would run the real model against a real account, silently and at
# full cost.
BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-ledger: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
# ONE REAL EVENT BEFORE ANYTHING ELSE, so the segment is a session that spoke and acted. A
# trace of nothing but a result record is a shape no client produces, and session_outcome
# reads the tool calls as well — a fixture that omitted them would be asserting against a
# code path the harness does not take (law-fixtures-carry-real-cadence).
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
seed() {   # seed <id> [repo-label]
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:${2:-fixture}\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1; }
# The disposition line, and never a `born`/`awake` one: those carry no session to describe.
done_line() { grep ' done ' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null | tail -1; }
fresh()    { : > "$SPIRA_RUN/aeon-ledger.log"; }

# A COMPLETE RECORD, WITH NO ROUND NUMBERS IN IT. Every figure below is one the shipped code
# cannot produce by accident: the durations are not whole seconds, so truncation and rounding
# are told apart; the cost has more decimals than it is rendered with; and no two fields
# share a value, so a parser reading the wrong key is a failure rather than a coincidence.
FULL='{"type":"result","subtype":"success","is_error":false,"duration_ms":90480,"duration_api_ms":61400,"num_turns":7,"total_cost_usd":1.3474715,"usage":{"input_tokens":1234,"cache_creation_input_tokens":105984,"cache_read_input_tokens":456789,"output_tokens":2222,"output_tokens_details":{"thinking_tokens":333}},"result":"done"}'

echo
echo "a session that ran to its end — the done line carries what it spent:"
fresh; testdb_reset; seed sp-lg-1; session 1 1 <<< "$FULL"; run_aeon
line="$(done_line)"
is   "the bead is converted to submitted, not left closed" open \
     "$(bd -C "$SPIRA_DB" show sp-lg-1 --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("status") or "")' 2>/dev/null)"
want "the disposition is still the first thing said" "done builder sp-lg-1 rc=" "$line"
want "and it is the disposition the bead reached"       "status=submitted" "$line"
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
fresh; testdb_reset; seed sp-lg-2; session 0 0 <<< ""; run_aeon
line="$(done_line)"
want   "the disposition is recorded as ever" "done builder sp-lg-2 rc=" "$line"
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
echo "a record missing some figures — the missing ones alone are ?, beside real numbers:"
# A client that does not report a duration has been seen in the wild beside one that does, so
# "the trace had no result" and "this record did not carry that field" are different answers
# and only the second may leave its neighbours readable.
fresh; testdb_reset; seed sp-lg-3
session 1 1 <<< '{"type":"result","subtype":"success","is_error":false,"num_turns":4,"total_cost_usd":0.5,"usage":{"input_tokens":11,"output_tokens":22}}'
run_aeon
line="$(done_line)"
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
fresh; testdb_reset; seed sp-lg-4
{ printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"duration_api_ms":1000,"num_turns":1,"total_cost_usd":0.01,"usage":{"input_tokens":1,"cache_read_input_tokens":1,"output_tokens":1,"output_tokens_details":{"thinking_tokens":1}}}'
  printf '%s\n' "$FULL"; } | session 1 1
run_aeon
line="$(done_line)"
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
fresh; testdb_reset; seed sp-lg-7
# First record: the main session (24 minutes of real work)
MAIN='{"type":"result","subtype":"success","is_error":false,"duration_ms":1480000,"duration_api_ms":386600,"num_turns":21,"total_cost_usd":4.2700,"usage":{"input_tokens":15000,"cache_read_input_tokens":5415000,"output_tokens":15040,"output_tokens_details":{"thinking_tokens":5000}},"result":"done"}'
# Second record: the wake-up notification turn (4 seconds; api_ms is cumulative for session)
WAKEUP='{"type":"result","subtype":"success","is_error":false,"duration_ms":4000,"duration_api_ms":387000,"num_turns":1,"total_cost_usd":4.2746,"usage":{"input_tokens":3,"cache_read_input_tokens":133587,"output_tokens":42,"output_tokens_details":{"thinking_tokens":0}},"result":"done"}'
{ printf '%s\n' "$MAIN"; printf '%s\n' "$WAKEUP"; } | session 1 1
run_aeon
line="$(done_line)"
want   "wall_s reflects the whole session"      "wall_s=1484"    "$line"
nowant "wall_s is not just the wake-up turn"    "wall_s=4"       "$line"
want   "api_s from the last record (cumulative)" "api_s=387"     "$line"
want   "turns sum both records"                  "turns=22"      "$line"
want   "cost from the last record's cumulative"  "cost_usd=4.2746" "$line"
# THE IMPOSSIBILITY GUARD. wall_s < api_s is structurally impossible for a sequential
# session, so if the output contains api_s=387 it must not also contain wall_s=4.
want   "wall_s=1484 is greater than api_s=387"  "wall_s=1484"    "$line"

echo
echo "a second attempt on the same bead reads its OWN segment, not the one above it:"
# THE BOUNDARY. Both attempts append to one trace file, so attempt 2 — which died before it
# could speak — sits directly beneath attempt 1's complete record. Reading the file instead
# of the attempt's segment bills the dead session for the live one's tokens, and no reader
# downstream could tell. Attempt 1 is asserted first and in the same file, so "it says ?" is
# a fact about the boundary rather than about a parser that never works.
fresh; testdb_reset; seed sp-lg-5
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

echo
echo "a disposition written before any session could run still carries the fields:"
# `?` HERE IS THE POINT AND SO IS THE LINE EXISTING AT ALL. A bead whose repo: label resolves
# to nothing is released before a trace file has even been named, so this is the one caller
# that reaches the parser with no log at all — and it must render the same eight keys rather
# than a short line a reader would mistake for an older format. Under `set -u` it is also the
# path where an unset log variable would kill the teardown outright.
fresh; testdb_reset; seed sp-lg-6 nosuchrepo; session 0 0 <<< ""; run_aeon
line="$(done_line)"
want "the disposition names the fault"   "status=unmapped-repo" "$line"
want "and the spend is unknown, in full" \
     "wall_s=? api_s=? turns=? in_tok=? cache_read_tok=? out_tok=? think_tok=? cost_usd=?" "$line"

echo
echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
