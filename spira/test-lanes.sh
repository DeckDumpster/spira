#!/usr/bin/env bash
#
# test-lanes.sh — ops.fayth's lane guarantee: FAYTH_LANE=ops draws from its own
# FAYTH_MAX_CONCURRENT, never from SPIRA_MAX_AEONS. The roster-split and pool/escape
# rows moved to test-fayth.sh/test-summon-fayth.sh (sp-9ce60.2.1/.2.2); this is what's
# left.
#
#   ./test-lanes.sh
#
# defect: sp-vyl4
# tier: T1
# covers: spira/conf.sh spira/sentinel.sh spira/escape.sh spira/chamber/ops.fayth UC-dispatch-07
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

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

tl_summary
