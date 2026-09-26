#!/usr/bin/env bash
#
# test-aeon-teardown-e2e.sh — one shared full-aeon fixture (full-aeon-fixture.sh), one row
# per open-bead teardown path that only a real aeon.sh run can prove. Everything that does
# NOT need a live session — the disposition precedence table, the spend parser, the
# yield-headless phrasing table — already lives at T1 in test-aeon-disposition.sh,
# test-session-result-fields.sh and test-session-yield-headless.sh. What is left here is the
# WIRING: that aeon.sh's own marks, mail.sh's own marker, and attempts_of's own SQL agree
# with what those T1 tables predict.
#
# Replaces seven suites (docs/test-plan/aeon-execution.md D8, D12, D15 — sp-g44ke):
# test-aeon-decision-blocked.sh, test-aeon-operator-wait.sh, test-requeue.sh,
# test-aeon-ledger.sh, test-aeon-presession-death.sh, test-aeon-yield-headless.sh,
# test-aeon-exit.sh — ~1,460 lines, each building its own bare origin, clone, chamber and
# claude shim to assert one `aeon-ledger.log status=` line apiece. Every one of those
# suites' use cases is a row below; test-requeue.sh's deadlock-sweep half (UC-24, no aeon
# run) moved to test-deadlock-sweep.sh instead, since it never touched a live session.
#
# ONE FIXTURE, RESET BETWEEN ROWS (fa_reset), never rebuilt: the git and chamber setup is
# built once by full-aeon-fixture.sh's fa_setup, which is the D15 saving — 20 suites each
# paid for `git init --bare` + clone + chamber write-out on every one of their own runs.
#
# SERVER-MODE bd, for the whole file: the rebase-conflict row's attempts_of/requeues_of
# read the events table via `bd sql`, which embedded mode refuses (testdb.sh), and mode is
# a property of the whole store, not chosen per call.
#
# defect: sp-egge2 sp-ne93n sp-l7f5 sp-214 sp-ywlti sp-iu10 sp-2a4hd
# tier: T3
# covers: spira/aeon.sh spira/lib.sh spira/mail.sh UC-aeon-execution-02 UC-aeon-execution-11 UC-aeon-execution-12 UC-aeon-execution-18
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

export SPIRA_TESTDB_MODE=server
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/full-aeon-fixture.sh"

fa_setup teardowne2e || exit 77
trap 'fa_teardown' EXIT INT TERM

echo "test-aeon-teardown-e2e.sh"

# ==========================================================================================
echo
echo "ROW: session did not close (charged) / rebase-conflict reopen (requeued, not charged)"
# ==========================================================================================
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control): "no attempt was charged" is
# the answer a counter that never runs gives too, so the decline below sits beside a charge
# the same code path produces from the same fixture.
#
# THE REBASE CONFLICT IS REAL, not simulated by a flag: the base gains a commit touching the
# same line the shim's commit touches, which is what the aeon's own rebase step then fails
# on — a fixture that set the outcome directly would be asserting against a model of the
# thing under test.
MAINREPO="$FA_REPO"; export MAINREPO
shim() {   # shim <commit:0|1> <close:0|1> [move-the-base:0|1] [claude-rc:0|1]
    printf '%s' "$1" > "$FA_TMP/docommit"; printf '%s' "$2" > "$FA_TMP/doclose"
    printf '%s' "${3:-0}" > "$FA_TMP/domove"; printf '%s' "${4:-0}" > "$FA_TMP/doclauderc"
    cat > "$FA_BIN/claude" <<'SHIM'
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
printf '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit "$(cat "$TMP/doclauderc")"
SHIM
    chmod +x "$FA_BIN/claude"
}
field() { fa_field "$1" "$2"; }
lib() { bash -c ". \"$HERE/lib.sh\"; $1" 2>/dev/null; }
count_of()   { local c; c="$(lib "attempts_of $1")"; printf '%s' "${c:-0}"; }
requeue_of() { local c; c="$(lib "requeues_of $1")"; printf '%s' "${c:-0}"; }

fa_reset; fa_seed sp-rq-1; shim 1 1 1; fa_run_aeon >/dev/null
is     "reopened over a rebase conflict — the bead is open again" open "$(field sp-rq-1 status)"
want   "and the log says why"                  "REOPENED — closed behind" "$(fa_out)"
is     "no attempt is charged"                 "0" "$(count_of sp-rq-1)"
is     "it is counted as a requeue instead"    "1" "$(requeue_of sp-rq-1)"
want   "the teardown says no attempt was charged" "no attempt charged" "$(fa_out)"
want   "the bead carries the decision"         "Requeue 1 (rebase-conflict)" "$(fa_notes sp-rq-1 | tr -s ' ')"
want   "and the ledger carries the outcome"    "status=requeue-rebase-conflict" "$(fa_ledger_line sp-rq-1)"
# THE BRANCH SURVIVES THE REQUEUE. The aeon committed before closing; the rebase failed
# after close and was aborted, leaving the branch at its pre-abort tip.
_nc="$(git -C "$FA_REPO" rev-list --count "$(git -C "$FA_REPO" rev-parse origin/main)..spira/sp-rq-1" 2>/dev/null || echo 0)"
is     "the branch still carries the aeon's commit after the requeue" "1" "$_nc"

# ALSO the exit-code positive control (UC-18): a bead left open with claude's own rc=1 must
# make aeon itself exit non-zero — reusing this run rather than a dedicated one, since the
# fix side of the same UC (a CLOSED bead, claude rc=1, aeon exits 0) still gets its own row
# below where a stray non-zero exit is the whole point.
fa_reset; fa_seed sp-rq-2; shim 0 0 0 1; rc="$(fa_run_aeon)"
is   "session did not close the bead — bead is open" open "$(field sp-rq-2 status)"
is   "and one attempt IS charged (the normal unlanded case)" "1" "$(count_of sp-rq-2)"
is   "with nothing on the requeue counter"     "0" "$(requeue_of sp-rq-2)"
want "and the note reads Unlanded"             "Unlanded" "$(fa_notes sp-rq-2)"
is   "POSITIVE CONTROL — bead not closed, claude rc=1 — aeon exits non-zero" "1" "$rc"

# ==========================================================================================
echo
echo "ROW: decision-blocked — released, no attempt charged"
# ==========================================================================================
# Shim creates a decision bead blocking the claimed bead, then exits non-zero — simulating an
# aeon that filed a question via mail.sh for a decision bead. The dep is added AFTER the bead
# is claimed (in_progress); bd ready only returns unblocked beads, so a pre-existing dep would
# prevent the claim entirely.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
_bd="${SPIRA_BD:-bd}"
id="$(BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
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

# The sp-dvsqc defect (an ask-labelled dep via a relates-to edge treated as a blocker, even
# though aeon.sh now filters on dependency_type == "blocks" only) is deferred to a follow-up
# bead rather than given its own row here: it is a real, previously-defective path with no
# T1 coverage, but every row in this file costs real wall-clock against the area's 60s cap,
# and the discriminating "blocks vs relates-to" logic sits beside decision-blocked's own
# dependency read, not inside the disposition table this suite otherwise wires. See sp-5t53s.

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
id="$(BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
if [ -n "$id" ]; then
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
echo "ROW: operator-wait marker — released, no attempt charged"
# ==========================================================================================
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
if [ -n "${BEAD_ID:-}" ] && [ -n "${SPIRA_RUN:-}" ]; then
    touch "$SPIRA_RUN/$BEAD_ID.operator-wait"
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-ow-2; fa_run_aeon >/dev/null
is   "bead is still open (correctly not closed)" "open" "$(fa_status sp-ow-2)"
notes_ow="$(fa_notes sp-ow-2)"
want "note says kind-question mail"      "kind-question mail" "$notes_ow"
want "note says No attempt charged"      "No attempt charged" "$notes_ow"
nowant "note does not say Unlanded"      "Unlanded"           "$notes_ow"
want "ledger says operator-wait" "operator-wait" "$(fa_ledger_line sp-ow-2)"

echo
echo "mail.sh, driven directly (no aeon run): kind=question writes the operator-wait marker"
BEAD_ID=sp-ow-mail SPIRA_RUN="$SPIRA_RUN" bash -c '
. "$1/mail.sh" 2>/dev/null || true
cmd_send operator --from "Builder <builder@spira>" --subject "fixture question" \
    --kind question --default "proceed without waiting" <<BODY
## Question

Can the fixture answer this itself?

## Default

Proceed without waiting.
BODY
' _ "$HERE" >/dev/null 2>&1
is "mail.sh send kind=question wrote the marker itself" "yes" \
   "$([ -e "$SPIRA_RUN/sp-ow-mail.operator-wait" ] && echo yes || echo no)"

# ==========================================================================================
echo
echo "ROW: pre-session death — worktree failure is named, charged, and the loop is bounded"
# ==========================================================================================
# THE DEFECT THIS GUARDS. An aeon that dies before its Claude session starts used to be
# recorded as a refusal (no attempt charged) and re-summoned forever: the ledger line read
# rc=0 status=in_progress with every metric ?, identical to a short successful run.
#
# A DIFFERENT REPO SHAPE, on purpose: a local REMOTE plus a REPO that tracks it, so
# refs/remotes/origin/main can be deleted to make the land ref unresolvable without
# touching the shared FA_REPO/FA_ORIGIN the other rows use.
PSD_REMOTE="$FA_TMP/psd-remote"
git init -q -b main "$PSD_REMOTE"
git -C "$PSD_REMOTE" config user.email t@t; git -C "$PSD_REMOTE" config user.name t
printf 'seed\n' > "$PSD_REMOTE/f"; git -C "$PSD_REMOTE" add f; git -C "$PSD_REMOTE" commit -qm seed

PSD_REPO="$FA_TMP/psd-repo"
git init -q -b main "$PSD_REPO"
git -C "$PSD_REPO" config user.email t@t; git -C "$PSD_REPO" config user.name t
git -C "$PSD_REPO" remote add origin "$PSD_REMOTE"
git -C "$PSD_REPO" fetch -q origin 2>/dev/null
git -C "$PSD_REPO" checkout -q -b main --track origin/main 2>/dev/null \
    || git -C "$PSD_REPO" checkout -q main 2>/dev/null || true
git -C "$PSD_REPO" remote set-head origin --auto 2>/dev/null || true
git -C "$PSD_REPO" update-ref -d refs/remotes/origin/main

PSD_REPO_MAP="$FA_TMP/psd-repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$PSD_REPO" > "$PSD_REPO_MAP"

# THE SHIM MUST NEVER BE CALLED — the aeon dies before the session starts.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf 'test-aeon-teardown-e2e: shim was invoked — aeon did not die pre-session\n' >&2
exit 1
SHIM
chmod +x "$FA_BIN/claude"

fa_reset; fa_seed sp-pd-1
export SPIRA_REPO_MAP="$PSD_REPO_MAP"
fa_run_aeon >/dev/null
want "error message is logged (not swallowed)"    "land ref cannot be resolved" "$(fa_out)"
want "ledger status is pre-session"               "status=pre-session"          "$(fa_ledger_line sp-pd-1)"
nowant "not recorded as in_progress"              "status=in_progress"          "$(fa_ledger_line sp-pd-1)"
nowant "rc is not 0 (session never ran)"          "rc=0 "                       "$(fa_ledger_line sp-pd-1)"

# The bead is back in open state after cleanup released it. Run again — ref still absent.
fa_run_aeon >/dev/null
_count="$(fa_ledger_lines sp-pd-1 | grep -c 'status=pre-session' 2>/dev/null || echo 0)"
want "second death also charges (ledger-based; the infinite loop is broken)" "status=pre-session" "$(fa_ledger_line sp-pd-1)"
is   "two pre-session entries in the ledger" "2" "$_count"

# The rapid-recur park (a third consecutive sub-10s death labels and parks the bead so a
# fourth summon cannot repeat the same futile retry — SPIRA_RAPID_RECUR_THRESHOLD) is
# deferred to a follow-up bead rather than given its own rows here: it is a real mechanism
# that loses its only test with this file's deletion, but two more real aeon runs would push
# this suite over the area's 60s cap for a park behaviour distinct from the FATAL/charge/loop
# story UC-aeon-execution-02 itself names. See sp-5t53s.

# RESTORE the shared repo-map — every row after this one uses FA_REPO again.
export SPIRA_REPO_MAP="$FA_REPO_MAP"

# ==========================================================================================
echo
echo "ROW: ledger segment boundary — attempt 2 reads its OWN segment, not attempt 1's"
# ==========================================================================================
# What only a real aeon run can prove: that aeon.sh's own marks (spira_trace_mark) and
# attempt_trace's backward scan land on the same boundary session_result_fields (T1, in
# test-session-result-fields.sh) already proves trace_segment finds by a forward scan.
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
id="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
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
is "bead is converted to submitted, not left closed" "open" "$(fa_status sp-ex-2)"
is "aeon exits 0 despite claude rc=1 (the fix)" "0" "$rc"
want "ledger still records the real rc" "rc=1" "$(fa_ledger_line sp-ex-2)"
want "and records the submitted status" "status=submitted" "$(fa_ledger_line sp-ex-2)"
# The positive control for this UC (bead not closed, claude rc=1, aeon exits non-zero) is
# the "session did not close" row above (sp-rq-2) — the same discrimination, one fewer run.

# ==========================================================================================
echo
echo "ROW: exit code, sweep mode — claude rc=1 but ran still exits 0"
# ==========================================================================================
cat > "$FA_HOME/chamber/sweeper.fayth" <<SFAYTH
FAYTH_NAME=sweeper
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
SFAYTH
printf 'sweep {{BEAD_ID}}\n{{PARK}}\n' > "$FA_HOME/chamber/sweeper.md"

cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":2000,"num_turns":1,"total_cost_usd":0.001}\n'
exit "$(cat "$TMP/shim-rc" 2>/dev/null || echo 0)"
SHIM
chmod +x "$FA_BIN/claude"
printf 1 > "$FA_TMP/shim-rc"
sweep_rc="$("$HERE/aeon.sh" sweeper --sweep --prompt "check pipeline" > "$FA_TMP/sweep-out" 2>&1; echo $?)"
is "sweep with claude rc=1 but ran exits 0 (ops/qa sweep fix)" "0" "$sweep_rc"
# The positive control for this UC (a refused sweep — no tool calls — exits non-zero so a
# real ops failure stays visible) is deferred to a follow-up bead rather than a third sweep
# run here: bead mode already carries this UC's positive control (the "session did not
# close" row), and this file is at the area's 60s cap. See sp-5t53s.

tl_summary
