#!/usr/bin/env bash
# test-thrash-teardown.sh — G2: the thrash-requeue teardown branch in aeon.sh's cleanup(),
#   driven through the real aeon.sh rather than asserted by line-order/awk greps.
#
# THE GAP THIS CLOSES (sp-eq8a4.2.2, G2). The heartbeat writes $BEAD_ID.thrash and kills the
# session when the deliverable has stalled (hb_tick's `thrash` verdict); cleanup() reads that
# marker and requeues instead of charging an attempt, ON THE THEORY that the aeon was killed
# for stalling, not judged on its work. Until now the only suite touching this branch grepped
# aeon.sh's source for the shape of the code — never ran it. Two behaviours are asserted here:
#
#   1. THE FIRST thrash requeue on a bead is exempt: `bump_requeue <id> thrash` (an event
#      attempts_of subtracts), the note says "No attempt charged", the ledger status is
#      `requeue-thrash`.
#   2. A SECOND thrash requeue with the branch tip UNCHANGED since the first — nothing has
#      been committed, so this is the same stall, not a second unlucky aeon (sp-gs24i) — IS
#      charged: `bump_requeue <id> thrash-stale` (not subtracted), the note says "STICKING
#      POINT" and "attempt IS charged", the ledger status is `requeue-thrash-charged`.
#
# The marker is planted by the claude shim exactly as the heartbeat subshell would leave it
# behind after a kill -TERM -$$: this suite is about cleanup()'s read of that marker, not
# about the heartbeat's own trip condition (that is hb_tick's table, test-aeon-lease.sh).
#
# defect: sp-4rzlw
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-thrash-teardown
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up thrash-teardown || { bail "could not build fixture database"; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || bail "aeon.sh has no SPIRA_AGENT injection — refusing to run the real model"

# Shim A (positive control): exits with the bead open and no marker — attempt IS charged.
cat > "$BIN/claude-no-thrash" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$BIN/claude-no-thrash"

# Shim B: plants the .thrash marker exactly as the heartbeat would, then exits — simulating
# a session the heartbeat has already killed for the deliverable not moving.
cat > "$BIN/claude-thrash" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
if [ -n "${BEAD_ID:-}" ] && [ -n "${SPIRA_RUN:-}" ]; then
    printf 'stalled: no last action\n' > "$SPIRA_RUN/$BEAD_ID.thrash"
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$BIN/claude-thrash"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
run_aeon() {
    rm -rf "$SPIRA_RUN/worktree"
    "$HERE/aeon.sh" builder > "$TMP/out" 2>&1
    echo $?
}
bead_status() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status", ""))' 2>/dev/null
}
bead_notes() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("notes", "") or "")' 2>/dev/null
}
ledger_line() { grep "done builder $1" "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null | tail -1; }
fresh() { testdb_reset; }

echo "test-thrash-teardown.sh"

# ======================================================================================
# POSITIVE CONTROL: without the .thrash marker, exit with the bead open IS charged.
# ======================================================================================
echo
echo "positive control — no thrash marker — attempt IS charged"

ln -sf "$BIN/claude-no-thrash" "$BIN/claude"
fresh; seed sp-tt-1
run_aeon
is   "SEEN RED: bead is still open"    "open"     "$(bead_status sp-tt-1)"
want "SEEN RED: note says Unlanded"    "Unlanded" "$(bead_notes sp-tt-1)"

# ======================================================================================
# CASE 1: first thrash requeue — no attempt charged, ledger requeue-thrash.
# ======================================================================================
echo
echo "first thrash requeue on a bead — no attempt charged"

ln -sf "$BIN/claude-thrash" "$BIN/claude"
fresh; seed sp-tt-2
run_aeon
is    "bead is still open (requeued, not closed)"    "open" "$(bead_status sp-tt-2)"
notes1="$(bead_notes sp-tt-2)"
want   "note says Requeued (thrash)"                  "Requeued (thrash)"  "$notes1"
want   "note says no attempt charged"                 "No attempt charged" "$notes1"
nowant "note does not yet name a sticking point"      "STICKING POINT"     "$notes1"
want   "ledger status is requeue-thrash (first time)" "requeue-thrash"     "$(ledger_line sp-tt-2)"
nowant "ledger is not yet the charged variant"        "requeue-thrash-charged" "$(ledger_line sp-tt-2)"

# ======================================================================================
# CASE 2: second thrash requeue, SAME branch tip (no commit happened) — attempt IS charged.
# ======================================================================================
echo
echo "second thrash requeue at the same tip — attempt IS charged (sp-gs24i)"

run_aeon
is    "bead is still open (requeued again)"          "open" "$(bead_status sp-tt-2)"
notes2="$(bead_notes sp-tt-2)"
want  "note names the sticking point"                "STICKING POINT"     "$notes2"
want  "note says an attempt IS charged this time"    "attempt IS charged" "$notes2"
want  "ledger status is requeue-thrash-charged"       "requeue-thrash-charged" "$(ledger_line sp-tt-2)"

# ======================================================================================
# Structural: the thrash trip kills the process group (-$$), not just the parent shell —
# a kill to $$ alone defers behind the running claude session (bash defers TERM while
# waiting on a foreground process), so the trip would not actually stop it in time.
# ======================================================================================
echo
echo "aeon.sh structural: thrash trip uses process-group kill (-\$\$)"

AEON="$HERE/aeon.sh"
write_line="$(grep -n 'printf.*BEAD_ID\.thrash' "$AEON" | head -1 | cut -d: -f1)"
if [ -z "$write_line" ]; then
    bad "thrash marker write (printf > BEAD_ID.thrash)" "not found in aeon.sh"
else
    kill_cmd="$(awk "NR>$write_line && NR<=$((write_line+5))" "$AEON" | grep 'kill')"
    want "thrash trip kills the process group" "kill -TERM -\$\$" "$kill_cmd"
fi

tl_summary
