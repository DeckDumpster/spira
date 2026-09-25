#!/usr/bin/env bash
#
# test-aeon-yield-headless.sh — session_yield_headless (lib.sh) is a pure predicate over a
#   trace file: does the session's LAST assistant text say it is waiting for a background
#   task notification? In headless mode no such wakeup can arrive: the session terminates,
#   background tasks are killed, and rc=0 with no diagnostic signal would otherwise read as
#   status=in_progress — "finished, didn't close" — with no indication the work was lost to
#   a yield. aeon.sh classifies it as yield-headless instead, so the cause is visible in the
#   ledger and the next summon gets a concrete repair.
#
# Previously this predicate had no direct test at all — 2 real aeon runs and one phrasing
# assert covered it indirectly. Below: a phrasing table over the real function (no aeon run,
# no bd), plus ONE real aeon run proving aeon.sh's own wiring (ledger status, bead note).
#
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-yield-headless.sh"

syh() {   # syh <file> -> session_yield_headless's own rc over the real lib.sh
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; session_yield_headless "$2"' _ "$HERE" "$1" 2>/dev/null
}
trace_of() {   # trace_of <file> <assistant-text...> — writes one assistant message per arg
    local f="$1"; shift
    : > "$f"
    local t
    for t in "$@"; do
        python3 -c 'import json,sys; print(json.dumps({"type":"assistant","message":{"id":"m","content":[{"type":"text","text":sys.argv[1]}]}}))' "$t" >> "$f"
    done
}

# ===========================================================================================
echo
echo "T1: session_yield_headless <trace> — phrasing table, no aeon run, no bd"
# ===========================================================================================
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control).

f="$TMP/a.log"

trace_of "$f" "The build is running. I will wait for the background task notification to continue."
syh "$f"; wantrc "positive control: the exact phrase yields" 0 "$?"

trace_of "$f" "I ran the command. The output looks fine."
syh "$f"; wantrc "ordinary unlanded text does not yield" 1 "$?"

trace_of "$f" "kicked off the job; background is waiting on the runner now"
syh "$f"; wantrc "background ... waiting within 30 chars yields" 0 "$?"

trace_of "$f" "waiting for a slow background disk task to finish up"
syh "$f"; wantrc "waiting ... background ... task within range yields" 0 "$?"

trace_of "$f" "Kicking off the long build now — will be woken when it lands."
syh "$f"; wantrc "'will be woken' yields" 0 "$?"

trace_of "$f" "Started it with run_in_background so I can keep going."
syh "$f"; wantrc "a run_in_background mention yields" 0 "$?"

trace_of "$f" "WILL BE WOKEN when the CI run finishes."
syh "$f"; wantrc "matching is case-insensitive" 0 "$?"

trace_of "$f" "I will wait for the background task notification." "Never mind — I finished the work myself just now."
syh "$f"; wantrc "only the LAST text governs: an earlier yield phrase is overridden" 1 "$?"

trace_of "$f" "Kicking off the build now." "I will wait for the background task notification to continue."
syh "$f"; wantrc "only the LAST text governs: a later yield phrase fires" 0 "$?"

: > "$f"
python3 -c 'import json; print(json.dumps({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}))' >> "$f"
syh "$f"; wantrc "an assistant message with no text content does not yield" 1 "$?"

: > "$f"
syh "$f"; wantrc "an empty trace does not yield" 1 "$?"

syh "$TMP/no-such-file.log"; wantrc "an unreadable trace does not yield" 1 "$?"

# ===========================================================================================
echo
echo "T3: one real aeon run proves the wiring — ledger status and bead note"
# ===========================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-yield-headless
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
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$HERE/aeon.sh" builder > "$TMP/out" 2>&1; }
bead_notes() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("notes", "") or "")' 2>/dev/null
}

cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo build"}}]}}\n'
printf '{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"The build is running. I will wait for the background task notification to continue."}],"stop_reason":"end_turn"}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":5000,"num_turns":2,"total_cost_usd":0.01}\n'
exit 0
SHIM
chmod +x "$BIN/claude"
testdb_reset; seed sp-yh-1
run_aeon
want "ledger records yield-headless"      "status=yield-headless" "$(grep 'done builder sp-yh-1' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null)"
want "bead note mentions yield-headless"  "background task notification" "$(bead_notes sp-yh-1)"

tl_summary
