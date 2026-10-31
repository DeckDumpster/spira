#!/usr/bin/env bash
#
# test-epic-claim-dwarf.sh — T6 (sp-ns46j): dwarf's own claim, wired end to end onto the
# epic-first rank, claims a started epic's reworked child — not whatever `bd ready --claim`
# would take on bead priority alone. Split off test-epic-claim-order.sh for wall time; the
# shared fixture is testlib/epic-claim-fixture.sh.
#
# defect: sp-ns46j sp-o4trx
# tier: T3
# covers: spira/lib.sh dwarf/src/* spira-claim/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90

ECL_SUITE=test-epic-claim-dwarf
# shellcheck disable=SC1090
. "$HERE/testlib/epic-claim-fixture.sh"

echo "test-epic-claim-dwarf.sh"

# ==========================================================================================
echo
echo "T6: dwarf.sh's own claim, wired end to end, claims the epic-first candidate — not"
echo "    whatever 'bd ready --claim' would have taken on bead priority alone"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 0)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-rework sp-e1 3)
$(bead sp-unrelated-p0 "" 0)
JSONL
ecl_dwarf_env
# SPIRA_RUN/SPIRA_DB/SPIRA_REPO_MAP are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare via tl_config, not the env prefix below, which no process reads any more.
tl_config SPIRA_RUN="$DWARF_RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$DWARF_REPO_MAP"
( PATH="$TMP/lc-dwarf:$PATH" SPIRA_HOME="$DWARF_HOME" \
  SPIRA_CONF="$TMP/no-such2.conf" \
  dwarf --home "$DWARF_HOME" builder > "$TMP/dwarf-out" 2>&1 )

# WHICH BEAD THE DWARF TOOK is the claim the machine recorded: the holder of sp-e1-rework's
# Claim, and nobody's on the unrelated P0 head. (The old reading — bd status reopened to
# open plus spira-submitted by the teardown's bd-close conversion, sp-qsona — is gone with
# sp-v62vn: every session runs restricted, and a restricted session's bd close is inert.)
is "dwarf.sh claimed the started epic's reworked child, not the unrelated P0 head" \
    "yes" "$(grep -q '^sp-e1-rework ' "$DWARF_RUN/lc-claims.log" 2>/dev/null && echo yes || echo no)"
want "and its session was handed that bead" "work sp-e1-rework " "$(cat "$TMP/prompt" 2>/dev/null)"
is "the unrelated P0 bead was left alone, never claimed" \
    "no" "$(grep -q '^sp-unrelated-p0 ' "$DWARF_RUN/lc-claims.log" 2>/dev/null && echo yes || echo no)"
want "the log names the epic-first rank as the reason" "epic-first rank" "$(cat "$TMP/dwarf-out")"

tl_summary
