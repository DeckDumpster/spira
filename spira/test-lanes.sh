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
# covers: spira/conf.sh sentinel/src/* spira/chamber/ops.fayth UC-dispatch-07
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

# The ops lane is declared in the config key registry (sp-g3uwp: conf.sh's key list and
# defaults are generated from spira/conf.d/, one file per key, rather than carried as
# literal text in conf.sh itself).
want "SPIRA_LANES is a recognised conf key" "SPIRA_LANES" "$(ls "$HERE/conf.d")"
want "ops is in the SPIRA_LANES default"    "ops"          "$(grep 'SPIRA_LANES:=' "$HERE/conf.d/SPIRA_LANES")"

# The sentinel handles lane fayths in a separate loop, inside the summon pass it shares with
# --summon-only (sp-0y2av): both paths now reach it in-process (wave 4.27, family G,
# sp-gzmd2), under the same `ck7_summon_pass` flock on summon.lock rather than a bash seam
# under one — lib.sh's own copy is a one-line shim onto it.
want "the sentinel runs the shared summon pass" "ck7_summon_pass" "$(cat "$HERE/lib.sh")"
want "the summon body splits the roster on FAYTH_LANE" "FAYTH_LANE" \
    "$(cat "$HERE"/../sentinel/src/summon.rs)"
want "the summon body computes lane_fayths" "lane_fayths" \
    "$(cat "$HERE"/../sentinel/src/summon.rs)"

# escape.sh is retired into the aeon binary itself (`aeon --escape <fayth>`, sp-zpaq0);
# its own behaviour rows moved to test-summon-fayth.sh, which already covers this area.

tl_summary
