#!/usr/bin/env bash
#
# test-queue-stuck.sh — the stuck-queue alert's two pure functions (UC-42):
#   queue_last_moved and queue_stuck_action. batch.sh's landstate sweep calls both every
#   pass (spira/batch.sh:602,612); this drives them directly with synthetic inputs, the way
#   test-landing-gate-wait.sh's predecessor did for gate_fits/gate_lock_wait.
#
# WHAT THIS RESTORES. test-batch-trigger.sh (sp-vsob2, cd4118e60) carried these two
# functions' only T1 coverage alongside the batch-cut trigger predicate it was written for,
# and was deleted wholesale when that predicate retired — even though queue_last_moved and
# queue_stuck_action were not retired. test-batch-stuck.sh stays at its integration tier as
# the sole proof of the real wiring (docs/test-plan/landing-merge-queue.md section 8); this
# file is the faster, targeted companion it lost.
#
# tier: T1
# covers: spira/batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

. "$HERE/conf.sh"
. "$HERE/lib.sh"
# shellcheck disable=SC1091
. "$HERE/batch.sh"

echo "test-queue-stuck.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ── queue_last_moved: newest BATCHED/LANDED epoch, land_mark's no-newline shape ───
LS="$TMP/landstate"; mkdir -p "$LS"
is "no records: 0" "0" "$(queue_last_moved "$LS")"

printf 'BATCHED none 100' > "$LS/sp-a"
printf 'LANDED none 200' > "$LS/sp-b"
printf 'CERTIFIED none 9999' > "$LS/sp-c"   # not BATCHED/LANDED: ignored regardless of recency
is "newest BATCHED/LANDED wins, CERTIFIED ignored (positive control)" "200" "$(queue_last_moved "$LS")"

printf 'BATCHED none not-a-number' > "$LS/sp-d"
is "non-numeric epoch skipped, does not crash" "200" "$(queue_last_moved "$LS")"

# ── queue_stuck_action: no-history / not-stuck / alert / already-alerted ──────────
is "no last-moved: refuses rather than guessing" "no-history" \
    "$(queue_stuck_action 0 10000 7200 0)"
is "recent movement: not-stuck"                  "not-stuck" \
    "$(queue_stuck_action 9000 10000 7200 0)"
is "stalled, no flag yet: alert"                  "alert" \
    "$(queue_stuck_action 100 10000 7200 0)"
is "stalled, flag already set: stays quiet"       "already-alerted" \
    "$(queue_stuck_action 100 10000 7200 1)"
# a 5th arg advances the clock the age is measured from (never the no-history gate) —
# sp-w4tyd: a deep but draining queue has old certs yet recent movement.
is "stall-from clock overrides age, not the no-history gate" "not-stuck" \
    "$(queue_stuck_action 100 10000 7200 0 9500)"
is "stall-from does not suppress no-history"      "no-history" \
    "$(queue_stuck_action 0 10000 7200 0 9500)"

tl_summary
