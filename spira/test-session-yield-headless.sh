#!/usr/bin/env bash
#
# test-session-yield-headless.sh — session_yield_headless (lib.sh) is a pure predicate over
# a trace file: does the session's LAST assistant text say it is waiting for a background
# task notification? In headless mode no such wakeup can arrive: the session terminates,
# background tasks are killed, and rc=0 with no diagnostic signal would otherwise read as
# status=in_progress — "finished, didn't close" — with no indication the work was lost to
# a yield. aeon.sh classifies it as yield-headless instead, so the cause is visible in the
# ledger and the next summon gets a concrete repair.
#
# Split out of test-aeon-yield-headless.sh (sp-gcx3k, docs/test-plan/aeon-execution.md D15):
# this phrasing table needs no aeon run and no bd. The one case that does — a real aeon.sh
# run proving the ledger status and bead note actually get written — is now
# test-aeon-teardown-e2e.sh's "yield-headless" row.
#
# tier: T1
# covers: spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-session-yield-headless.sh"

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
echo "session_yield_headless <trace> — phrasing table, no aeon run, no bd"
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

tl_summary
