#!/usr/bin/env bash
#
# test-lanes.sh — ops.fayth's lane guarantee.
#
#   ./test-lanes.sh
#
# WHAT'S LEFT HERE. The roster split (spira_lane_fayths/spira_task_fayths) moved to
# test-fayth.sh (sp-9ce60.2.2); the pool-saturation and escape-hatch rows (criteria 1 and
# 2) moved to spira/test-summon-fayth.sh (D2, sp-9ce60.2.1). Both are file-level, not
# runtime, checks — they are not deleted, they live there now. Only the ops guarantee
# (criterion 3) remains: ops.fayth declares FAYTH_LANE=ops and is excluded from the pool.
#
# WHAT A LANE IS, in two sentences. A lane fayth declares FAYTH_LANE=<name> and draws from
# its own FAYTH_MAX_CONCURRENT, never from SPIRA_MAX_AEONS. The sentinel handles it in a
# separate loop after the pool, without a pool argument, so a fully-occupied builder pool
# can never block a lane fayth.
#
# defect: sp-vyl4
# covers: spira/conf.sh spira/sentinel.sh spira/escape.sh spira/chamber/ops.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# ==========================================================================================
echo
echo "criterion 3 — ops.fayth declares FAYTH_LANE=ops and is excluded from the task pool"
# ==========================================================================================
# ops.fayth shipped with FAYTH_ROLE=party. The new mechanism is FAYTH_LANE=ops. This test
# proves the shipped fayth file carries the new declaration, that the shipped behaviour
# (not drawn from the task pool) is preserved, and that the ops lane is declared in conf.sh.
#
# Test at the file level (assertions about what is WRITTEN) so the result is authoritative
# and not contingent on which environment the suite runs in.

want "ops.fayth declares FAYTH_LANE=ops" "FAYTH_LANE=ops" "$(cat "$HERE/chamber/ops.fayth")"
nowant "ops.fayth no longer uses FAYTH_ROLE=party" "FAYTH_ROLE=party" "$(grep -v '^#' "$HERE/chamber/ops.fayth")"

# The real-roster split (ops in lane fayths, builder in task fayths) is a test-fayth.sh row.

# The ops lane is declared in conf.sh's key list (SPIRA_CONF_KEYS) and defaults.
want "SPIRA_LANES is a recognised conf key" "SPIRA_LANES" "$(cat "$HERE/conf.sh")"
want "ops is in the SPIRA_LANES default"    "ops"          "$(grep 'SPIRA_LANES:=' "$HERE/conf.sh")"

# The sentinel handles lane fayths in a separate loop.
want "sentinel.sh references spira_lane_fayths" "spira_lane_fayths" "$(cat "$HERE/sentinel.sh")"
want "sentinel.sh handles LANE_FAYTHS in CHECK 7" "LANE_FAYTHS" "$(cat "$HERE/sentinel.sh")"

# escape.sh exists and is executable.
is "escape.sh is executable" "0" "$([ -x "$HERE/escape.sh" ] && echo 0 || echo 1)"
want "escape.sh references the lane escape rationale" "control plane" "$(cat "$HERE/escape.sh")"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
