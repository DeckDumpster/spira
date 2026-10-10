#!/usr/bin/env bash
#
# test-file-overlap-defer-deleted.sh — the sentinel's file-overlap deferral (CHECK 7e) is
# DELETED. Two beads whose branches touch one file are both claimable: rounds merge members
# onto the base and rebase, and Sift screens for conflicts, so a claim-time serialisation
# only parked rework behind beads that were themselves unclaimed.
#
# defect: sp-dzwj7w
# tier: T1
# covers: sentinel/src spira-claim/src spira/lib.sh spira/conf.d spira/config-delta.toml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-file-overlap-defer-deleted.sh"

ROOT="$(cd "$HERE/.." && pwd)"

# Positive control: the matcher must find a planted offender before silence means anything.
planted="$(printf 'let x = "file-overlap-defer";\n' | grep -c 'file-overlap-defer\|overlap_defer\|OVERLAP_DEFER')"
is "the matcher finds a planted offender" 1 "$planted"

hits="$(grep -rIl 'file-overlap-defer\|overlap_defer\|OVERLAP_DEFER\|detect_file_overlaps\|defer_file_overlaps\|check7e' \
    "$ROOT/sentinel/src" "$ROOT/spira-claim/src" "$ROOT/spira/lib.sh" "$ROOT/spira/conf.d" \
    "$ROOT/spira/config-delta.toml" 2>/dev/null)"
is "no exclusion list, config key or check carries the old label" "" "$hits"

tl_summary
