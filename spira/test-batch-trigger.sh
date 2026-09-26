#!/usr/bin/env bash
#
# test-batch-trigger.sh — the batch-cut trigger predicate: count >= max, age >= wait,
#   an internal express-eviction re-cut, CI idle, and an express-labelled bead.
#
# WHAT THIS REPLACES. test-batch::2 (3 real branches, aged by rewriting landstate,
# 2 full batch.sh runs), test-batch-express.sh's express/non-express/label-off cases
# (3 more runs) and all of test-batch-idle-cut.sh (5 runs) each drove this same ~25-line
# decision in batch.sh through a real repo, testdb and forge fixture. batch_cut_reason_cheap,
# batch_cut_idle and batch_cut_express (batch.sh) are the pure functions main() now calls;
# this file drives them directly with synthetic inputs. test-batch.sh case 1 stays as the
# one assembly control that proves the wiring from decision to an opened PR still works;
# test-batch-express.sh keeps only the eviction-mechanics cases (state, not a predicate).
#
# ORDER MATTERS: batch.sh has always checked count, then age, then the internal
# express-eviction flag, before ever paying for a forge call (idle) or a bdjson call
# (express label) — "asked last, and only when it can change the answer". The ordering
# case below is a regression fence on that, not just the individual predicates.
#
# tier: T1
# covers: spira/batch.sh UC-landing-merge-queue-34 UC-landing-merge-queue-42
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

. "$HERE/conf.sh"
. "$HERE/lib.sh"
# shellcheck disable=SC1091
. "$HERE/batch.sh"

echo "test-batch-trigger.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ── batch_cut_reason_cheap: count, age, internal express-eviction ─────────────
r="$(batch_cut_reason_cheap 8 10 8 1800 0)"; rc=$?
is "count>=max: cuts"           0       "$rc"
is "count>=max: reason"         "count" "$r"

r="$(batch_cut_reason_cheap 3 10 8 1800 0)"; rc=$?
is "below both thresholds: does not cut (positive control)" 1 "$rc"
is "below both thresholds: no reason printed" "" "$r"

r="$(batch_cut_reason_cheap 3 1801 8 1800 0)"; rc=$?
is "age>=wait: cuts"            0     "$rc"
is "age>=wait: reason"          "age" "$r"

r="$(batch_cut_reason_cheap 3 10 8 1800 1)"; rc=$?
is "evict-for-express flag: cuts"   0               "$rc"
is "evict-for-express flag: reason" "evict-express" "$r"

# count is checked before age: a row that satisfies both reports count.
r="$(batch_cut_reason_cheap 8 1801 8 1800 0)"
is "count checked before age" "count" "$r"

# ── batch_cut_idle: only the literal "0" reads as idle; ? and busy do not ─────
wantrc "idle=0: cuts"                     0 "$(batch_cut_idle 0; echo $?)"
wantrc "idle=3 (busy): does not cut"      1 "$(batch_cut_idle 3; echo $?)"
wantrc "idle=? (unknown): does not cut"   1 "$(batch_cut_idle '?'; echo $?)"
wantrc "idle=garbage: does not cut"       1 "$(batch_cut_idle 'not-a-number'; echo $?)"
wantrc "idle=empty: does not cut"         1 "$(batch_cut_idle ''; echo $?)"

# ── batch_cut_express: any bead in the priority JSON carrying the label ───────
EXPR_JSON='[{"id":"sp-a","labels":["spira","plan","express"]},{"id":"sp-b","labels":["spira","plan"]}]'
NOEXPR_JSON='[{"id":"sp-b","labels":["spira","plan"]}]'

wantrc "express label present: cuts"        0 "$(batch_cut_express "$EXPR_JSON" express; echo $?)"
wantrc "express label absent: does not cut (positive control)" \
    1 "$(batch_cut_express "$NOEXPR_JSON" express; echo $?)"
wantrc "express label renamed off: does not cut" \
    1 "$(batch_cut_express "$EXPR_JSON" other-label; echo $?)"
wantrc "malformed prio json fails closed (no cut)" \
    1 "$(batch_cut_express 'not json' express; echo $?)"

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
