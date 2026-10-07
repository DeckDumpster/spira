#!/usr/bin/env bash
#
# test-aeon-teardown-e2e-wired.sh — ledger segment, yield-headless and bead-mode exit-code teardown rows
# One of three parts of the aeon teardown wiring proof (test-aeon-teardown-e2e.sh holds the
# charge, decision and pre-session rows, test-aeon-teardown-e2e-wired.sh the ledger, yield
# and bead-mode exit-code rows, test-aeon-teardown-e2e-exit.sh the sweep and operator-wait rows): split so
# no part nears the suite wall bound. Each builds the shared full-aeon fixture once and
# resets it between rows.
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

# ==========================================================================================
echo
echo "ROW: yield-headless — ledger status and bead note, wired end to end"
# ==========================================================================================
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo build"}}]}}\n'
printf '{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"The build is running. I will wait for the background task notification to continue."}],"stop_reason":"end_turn"}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":5000,"num_turns":2,"total_cost_usd":0.01}\n'
exit 0
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-yh-1; fa_run_aeon >/dev/null
want "ledger records yield-headless"      "status=yield-headless" "$(fa_ledger_line sp-yh-1)"
want "bead note mentions yield-headless"  "background task notification" "$(fa_notes sp-yh-1)"

# ==========================================================================================
echo
echo "ROW: exit code, bead mode — closed bead exits 0 regardless of claude's own rc"
# ==========================================================================================
# THE DEFECT THIS TESTS. ops and qa run as named systemd units. A named unit enters FAILED
# when its ExecStart exits non-zero — an alert that is always firing is one nobody reads
# (law-alerts-must-be-actionable) — so a session that did the work and closed the bead must
# not fail the unit just because the claude CLI's own exit code was a stray non-zero.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
cat /dev/stdin > /dev/null 2>&1
id="${BEAD_ID:-}"   # the bound bead (sp-v62vn: bd status no longer reads in_progress)
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit "$(cat "$TMP/shim-rc" 2>/dev/null || echo 0)"
SHIM
chmod +x "$FA_BIN/claude"

fa_reset; fa_seed sp-ex-2
printf 1 > "$FA_TMP/shim-rc"
rc="$(fa_run_aeon)"
# Since sp-v62vn the session is restricted, and its hand-on (the shim's close, which the
# lifecycle stand-in reads as `work submit`) is the submitted disposition, not teardown's
# closed branch: the bd-close conversion to open+spira-submitted is gone with the branch.
# The ledger's real rc is NOT (UC-aeon-execution-18): the submitted exit recorded the
# aeon's own 0 until the submitted branch ledgered the model's rc itself.
is "aeon exits 0 despite claude rc=1 (the fix)" "0" "$rc"
want "ledger still records the real rc" "rc=1" "$(fa_ledger_line sp-ex-2)"
want "and records the submitted status" "status=submitted" "$(fa_ledger_line sp-ex-2)"
# The positive control for this UC (bead not closed, claude rc=1, aeon exits non-zero) is
# the "session did not close" row of test-aeon-teardown-e2e.sh (sp-rq-2).

# ROW DELETED — FAYTH_GRAPH_ONLY's close standing unconverted (sp-wnsks) was a rule inside
# teardown's closed branch, which no session reaches since sp-v62vn (every session is
# restricted and hands its bead on through the work verbs).

tl_summary
