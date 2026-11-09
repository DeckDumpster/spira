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
sleep() { SLEPT=$((SLEPT + ${1:-0})); }
UNIT_STATE=activating
fake_sc() { case "$*" in *" is-active "*) printf '%s\n' "$UNIT_STATE"; [ "$UNIT_STATE" = active ] ;; *) return 1 ;; esac; }
SC=fake_sc
ANSWERS_AFTER=3; CALLS=0
ready_scheduler_answers() { CALLS=$((CALLS + 1)); [ "$CALLS" -gt "$ANSWERS_AFTER" ]; }

run() { SLEPT=0; CALLS=0; ready_wait_scheduler spira-runtime-prod.service "$1"; printf 'rc=%s slept=%s\n' "$?" "$SLEPT"; }

echo "--- an activating unit whose socket answers late is waited for ---"
UNIT_STATE=activating; ANSWERS_AFTER=3
out="$(run 60)"
want "answers on the 4th ask: the wait ends in success" "rc=0 slept=3" "$out"

echo "--- the wait is bounded by the deadline ---"
ANSWERS_AFTER=1000
out="$(run 5)"
want "never answers: gives up at the deadline" "rc=1 slept=5" "$out"

echo "--- a unit that is not starting is not waited on ---"
UNIT_STATE=failed; ANSWERS_AFTER=1000
out="$(run 60)"
want "failed unit: no wait" "rc=1 slept=0" "$out"

echo "--- ready.sh runs this, not a copy ---"
want "ready.sh waits before judging the scheduler" 'ready_wait_scheduler "$_sched_unit"' "$(cat "$HERE/ready.sh")"
tl_summary
