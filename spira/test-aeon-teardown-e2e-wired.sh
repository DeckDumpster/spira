#!/usr/bin/env bash
#
# test-aeon-teardown-e2e-wired.sh — decision-blocked, own-closeout and ledger-segment teardown rows
# One of three parts of the aeon teardown wiring proof, split so no part nears the suite wall
# bound: test-aeon-teardown-e2e.sh holds the charge and pre-session rows, -wired the decision
# and ledger rows, -exit the yield, exit-code and operator-wait rows. Each builds the shared
# full-aeon fixture once and resets it between rows.
#
# SERVER-MODE bd: attempts_of/requeues_of read the events table via `bd sql`, which embedded
# mode refuses (testdb.sh).
#
# defect: sp-egge2 sp-ne93n sp-l7f5 sp-214 sp-ywlti sp-iu10 sp-2a4hd sp-wnsks
# tier: T3
# covers: aeon/src/* spira/lib.sh mail/src/* spira-lc/src/work.rs work/* cockpit/ops/src/resolve.rs cockpit/ops/src/resolve_main.rs
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# testdb-mode: server — attempts_of/requeues_of read the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/full-aeon-fixture.sh"

fa_setup teardownwired || exit 77
trap 'fa_teardown' EXIT INT TERM
. "$HERE/testlib/teardown-e2e.sh"

echo "test-aeon-teardown-e2e-wired.sh"

# ==========================================================================================
echo
echo "ROW: decision-blocked — released, no attempt charged"
# ==========================================================================================
# Shim creates a decision bead blocking the claimed bead, then exits non-zero — simulating an
# aeon that filed a question via mail for a decision bead. The dep is added AFTER the bead
# is claimed (in_progress); bd ready only returns unblocked beads, so a pre-existing dep would
# prevent the claim entirely.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
_bd="${SPIRA_BD:-bd}"
# The bound bead is BEAD_ID, from the aeon: since sp-v62vn the claim is the lifecycle
# row's, and bd's status no longer reads in_progress for it.
id="${BEAD_ID:-}"
if [ -n "$id" ]; then
    BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" create \
        "Operator question about $id" \
        -l "${SPIRA_ASK_LABEL:-needs-operator},overseer" \
        --type decision \
        --deps "blocks:$id" \
        --silent >/dev/null 2>&1 || true
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 1
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-db-2; fa_run_aeon >/dev/null
is "bead is still open (correctly not closed)" "open" "$(fa_status sp-db-2)"
notes2="$(fa_notes sp-db-2)"
want "note says released due to decision blocker" "decision" "$notes2"
want "note says no attempt charged" "No attempt charged" "$notes2"
nowant "note does not say Unlanded" "Unlanded" "$notes2"
want "ledger says decision-blocked" "decision-blocked" "$(fa_ledger_line sp-db-2)"

# The sp-dvsqc defect (an ask-labelled dep via a relates-to edge treated as a blocker) does
# not get a row here: open_ask_blocker (aeon::decide::open_ask_blocker), the dependency read
# this row's own decision-blocked branch feeds on, is pure and is a table in
# aeon/src/decide.rs instead (`cargo test -p aeon decide::tests::open_ask_blocker_table`) —
# no live session needed, no cost against this file's cap.

# ==========================================================================================
echo
echo "ROW: own issue-closeout ask — not a blocker, attempt IS charged"
# ==========================================================================================
# sp-2a4hd: a bead closed, asked about, and then reopened before its own "Close GitHub
# issue ... for bead <id>" ask was resolved must not be decision-blocked by that ask — it
# must be worked normally.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
_bd="${SPIRA_BD:-bd}"
# The bound bead is BEAD_ID, from the aeon: since sp-v62vn the claim is the lifecycle
# row's, and bd's status no longer reads in_progress for it.
id="${BEAD_ID:-}"
if [ -n "$id" ]; then
    # COMMITS (sp-1zxru): this row proves the own-closeout ask is not a decision-blocker,
    # which needs the session to reach the real unlanded/charged path to show — a session
    # with no commit now lands on the no-progress exit instead (its own row covers that
    # shape), which would make the "No attempt charged" nowant below a false positive.
    printf 'the aeon wrote this %s\n' "$(date +%s%N)" > f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
    BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" create \
        "Close GitHub issue github:fixture/testrepo#99 for bead $id" \
        -l "${SPIRA_ASK_LABEL:-needs-operator},overseer" \
        --type decision \
        --deps "blocks:$id" \
        --silent >/dev/null 2>&1 || true
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 1
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-db-4; fa_run_aeon >/dev/null
notes4="$(fa_notes sp-db-4)"
want   "own-closeout-ask: attempt IS charged (Unlanded, not released)" "Unlanded" "$notes4"
nowant "own-closeout-ask: not released as decision-blocked" "No attempt charged" "$notes4"
nowant "own-closeout-ask: ledger must not say decision-blocked" "decision-blocked" "$(fa_ledger_line sp-db-4)"

# ==========================================================================================
echo
echo "ROW: ledger segment boundary — attempt 2 reads its OWN segment, not attempt 1's"
# ==========================================================================================
# What only a real aeon run can prove: that the aeon's own marks and attempt_trace's
# backward scan land on the same boundary a forward scan would find (aeon::ledger). The
# forward-scan half (spira_trace_mark/trace_segment, and session_result_fields's own field
# table) was lib.sh's and retired dead with it (sp-j89pd, wave 4.2) — superseded by
# aeon::ledger::session_fields and its own unit tests.
FULL='{"type":"result","subtype":"success","is_error":false,"duration_ms":90480,"duration_api_ms":61400,"num_turns":7,"total_cost_usd":1.3474715,"usage":{"input_tokens":1234,"cache_creation_input_tokens":105984,"cache_read_input_tokens":456789,"output_tokens":2222,"output_tokens_details":{"thinking_tokens":333}},"result":"done"}'
cat > "$FA_BIN/claude" <<'SHIM'
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
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-lg-5
printf 1 > "$FA_TMP/docommit"; printf 0 > "$FA_TMP/doclose"; printf '%s\n' "$FULL" > "$FA_TMP/result"
fa_run_aeon >/dev/null           # ran, spent, left the bead open
first="$(fa_ledger_line sp-lg-5)"
printf 0 > "$FA_TMP/docommit"; printf 0 > "$FA_TMP/doclose"; : > "$FA_TMP/result"
fa_run_aeon >/dev/null           # never spoke
second="$(fa_ledger_line sp-lg-5)"
want   "attempt 1 is a full reading"          "turns=7"      "$first"
want   "and it cost what the record said"     "cost_usd=1.3475" "$first"
nowant "attempt 2 does not inherit its turns"  "turns=7"     "$second"
nowant "nor its cost"                          "cost_usd=1.3475" "$second"
want   "attempt 2 reads as unknown"            "turns=?"     "$second"
want   "and unknown on the cost"               "cost_usd=?"  "$second"
want   "and the fields ride a disposition that is not a close" "status=in_progress" "$second"

tl_summary
