#!/usr/bin/env bash
#
# test-aeon-yield-headless.sh — a session that ends its turn waiting for a background
#                               task notification is recorded as yield-headless, not
#                               generic unlanded.
#
#   ./test-aeon-yield-headless.sh
#
# THE DEFECT THIS TESTS. An aeon running headless ends its turn to "wait for a
# background task notification". In headless mode no such wakeup can arrive: the
# session terminates, background tasks are killed, and the bead is left in_progress
# with rc=0 and no diagnostic signal. Previously the ledger recorded this as
# status=in_progress — "finished, didn't close" — with no indication that the work
# was lost to a yield. The harness now classifies it as yield-headless so the cause
# is visible in the ledger and the next summon gets a concrete repair.
#
# WHAT IS TESTED:
#   1. A session whose last assistant text contains a background-task-wait pattern
#      gets ledger status=yield-headless and a matching bead note.
#      Positive control: a session without the pattern gets status=in_progress.
#
# covers: spira/aeon.sh spira/lib.sh
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
testdb_require test-aeon-yield-headless
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonyldhls || { echo "test-aeon-yield-headless: could not build a fixture database"; exit 1; }
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
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-yield-headless: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }

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
fresh() { testdb_reset; }

echo "test-aeon-yield-headless.sh"

# ======================================================================================
echo
echo "yield-headless: last message says 'wait for the background task notification':"
# ======================================================================================
# THE PATTERN THIS REPRODUCES. An aeon runs a long command, ends its turn with the
# standard text an agent uses when it backgrounded a task, and exits rc=0. Previously
# this produced status=in_progress in the ledger with no further signal. Now it produces
# status=yield-headless and a note that names the cause.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo build"}}]}}\n'
printf '{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"The build is running. I will wait for the background task notification to continue."}],"stop_reason":"end_turn"}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":5000,"num_turns":2,"total_cost_usd":0.01}\n'
exit 0
SHIM
chmod +x "$BIN/claude"
fresh; seed sp-yh-1
run_aeon > /dev/null 2>&1
want "ledger records yield-headless"      "status=yield-headless" "$(grep 'done builder sp-yh-1' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null)"
want "bead note mentions yield-headless"  "yield-headless"        "$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" notes sp-yh-1 2>/dev/null)"

# ======================================================================================
echo
echo "POSITIVE CONTROL — session ends without yield pattern gets unlanded (not yield-headless):"
# ======================================================================================
# The detection must not misclassify a generic unlanded session (one that called tools,
# ran to its end, but never showed a bg-task-wait pattern) as yield-headless.
cat > "$BIN/claude" <<'SHIM2'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"echo ok"}}]}}\n'
printf '{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"I ran the command. The output looks fine."}],"stop_reason":"end_turn"}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":2,"total_cost_usd":0.001}\n'
exit 0
SHIM2
chmod +x "$BIN/claude"
fresh; seed sp-yh-2
run_aeon > /dev/null 2>&1
nowant "no yield-headless for generic unlanded" "yield-headless" "$(grep 'done builder sp-yh-2' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
