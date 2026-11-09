#!/usr/bin/env bash
#
# test-ready-scheduler-wait.sh — ready.sh waits for the scheduler to answer on its control
# socket, instead of judging a unit that systemd still reports as "activating". Run in-process
# from ready-lib.sh with `sleep`, `world` and "$SC" as shell functions, so no case waits on a clock.
#
# tier: T1
# covers: spira/ready.sh spira/ready-lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/ready-lib.sh"

SLEPT=0
sleep() { SLEPT=$((SLEPT + ${1:-0})); SECONDS=$((SECONDS + ${1:-0})); }
ACTIVE_AFTER=0; ANSWERS_AFTER=0; FAILED=0
fake_sc() {
    case "$*" in
        *" is-active "*)
            if [ "$FAILED" = 1 ]; then echo failed; return 3; fi
            if [ "$SLEPT" -ge "$ACTIVE_AFTER" ]; then echo active; else echo activating; fi ;;
        *) return 1 ;;
    esac
}
SC=fake_sc
ready_scheduler_answers() { [ "$SLEPT" -ge "$ANSWERS_AFTER" ]; }

run() { SLEPT=0; ready_wait_scheduler spira-runtime-prod.service "$1"; printf 'rc=%s slept=%s\n' "$?" "$SLEPT"; }

echo "--- an activating unit whose socket answers late is waited for ---"
ACTIVE_AFTER=3; ANSWERS_AFTER=3; FAILED=0
want "answers once started: success after the wait" "rc=0 slept=3" "$(run 60)"

echo "--- an answering socket on a unit still activating is not ready yet ---"
ACTIVE_AFTER=4; ANSWERS_AFTER=1
want "waits for the unit as well" "rc=0 slept=4" "$(run 60)"

echo "--- the wait is bounded by the deadline ---"
ACTIVE_AFTER=1000; ANSWERS_AFTER=1000
want "never answers: gives up at the deadline" "rc=1 slept=5" "$(run 5)"

echo "--- a unit that is not starting is not waited on ---"
FAILED=1
want "failed unit: no wait" "rc=1 slept=0" "$(run 60)"

echo "--- ready.sh runs this, not a copy ---"
want "ready.sh waits before judging the scheduler" 'ready_wait_scheduler "$_sched_unit"' "$(cat "$HERE/ready.sh")"
tl_summary
