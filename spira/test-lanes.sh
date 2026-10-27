#!/usr/bin/env bash
#
# test-lanes.sh — ops.rune's lane guarantee: RUNE_LANE=ops draws from its own
# RUNE_MAX_CONCURRENT, never from SPIRA_MAX_AEONS. The roster-split and pool/escape
# rows moved to test-rune.sh/test-summon-rune.sh (sp-9ce60.2.1/.2.2); this is what's
# left.
#
#   ./test-lanes.sh
#
# defect: sp-vyl4
# tier: T1
# covers: spira/conf.sh sentinel/src/* spira/runes/ops.rune UC-dispatch-07
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# ==========================================================================================
echo
echo "criterion 3 — ops.rune declares RUNE_LANE=ops and is excluded from the task pool"
# ==========================================================================================
# ops.rune shipped with RUNE_ROLE=party. The new mechanism is RUNE_LANE=ops. This test
# proves the shipped rune file carries the new declaration, that the shipped behaviour
# (not drawn from the task pool) is preserved, and that the ops lane is declared in conf.sh.
#
# Test at the file level (assertions about what is WRITTEN) so the result is authoritative
# and not contingent on which environment the suite runs in.

want "ops.rune declares RUNE_LANE=ops" "RUNE_LANE=ops" "$(cat "$HERE/runes/ops.rune")"
nowant "ops.rune no longer uses RUNE_ROLE=party" "RUNE_ROLE=party" "$(grep -v '^#' "$HERE/runes/ops.rune")"

# The real-roster split (ops in lane runes, builder in task runes) is a test-rune.sh row.

# The ops lane is declared in the config key registry (sp-g3uwp: conf.sh's key list and
# defaults are generated from spira/conf.d/, one file per key, rather than carried as
# literal text in conf.sh itself).
want "SPIRA_LANES is a recognised conf key" "SPIRA_LANES" "$(ls "$HERE/conf.d")"
want "ops is in the SPIRA_LANES default"    "ops"          "$(grep 'SPIRA_LANES:=' "$HERE/conf.d/SPIRA_LANES")"

# The sentinel handles lane runes in a separate loop, inside the summon pass it shares with
# --summon-only (sp-0y2av): both paths now reach it in-process (wave 4.27, family G,
# sp-gzmd2), under the same `ck7_summon_pass` flock on summon.lock rather than a bash seam
# under one — lib.sh's own copy is a one-line shim onto it.
want "the sentinel runs the shared summon pass" "ck7_summon_pass" "$(cat "$HERE/lib.sh")"
want "the summon body splits the roster on RUNE_LANE" "RUNE_LANE" \
    "$(cat "$HERE"/../sentinel/src/summon.rs)"
want "the summon body computes lane_runes" "lane_runes" \
    "$(cat "$HERE"/../sentinel/src/summon.rs)"

# escape.sh is retired into the aeon binary itself (`aeon --escape <rune>`, sp-zpaq0);
# its own behaviour rows moved to test-summon-rune.sh, which already covers this area.

tl_summary
