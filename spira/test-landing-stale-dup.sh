#!/usr/bin/env bash
#
# test-landing-stale-dup.sh — a bead that changed state between the scan and the gate is not
# landed or reopened twice, and a parallel duplicate's reopen note names the bead that landed.
# Split off test-landing.sh for wall time; the shared fixture is testlib/landing-fixture.sh.
#
# defect: sp-q9i sp-9194o
# tier: T2
# covers: landing-pass/* spira/lib.sh UC-landing-merge-queue-13 UC-landing-merge-queue-15 UC-landing-merge-queue-17 UC-landing-merge-queue-22 UC-landing-merge-queue-23
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Resolved from this tree before any fixture repoints SPIRA_REPO.

LAND_SUITE=test-landing-stale-dup
# shellcheck disable=SC1090
. "$HERE/testlib/landing-fixture.sh"

echo "test-landing-stale-dup.sh"

# --------------------------------------------------------------------------------------
# A BEAD WHOSE STATUS CHANGED BETWEEN THE SCAN AND THE GATE
#
# The scan reads status once before the loop. The gate takes minutes. A bead reopened in
# that window must not be landed on a stale "closed" — the re-read at the point of action
# catches it.
# --------------------------------------------------------------------------------------
# The gate stub reopens the bead during the gate, simulating a concurrent reopen. Since
# sp-mve9i a reopen is the bead's lifecycle row going back to REWORK (bd status is inert), so
# that is what the stub moves; the re-read at the point of action reads the row.
reopen_in_lc='rm -f "'"$LC_FIX"'"/bead/*/"$SPIRA_GATE_BEAD"; mkdir -p "'"$LC_FIX"'/bead/REWORK"
printf "{\"bead_id\":\"%s\",\"state\":\"REWORK\",\"tip\":\"\",\"since\":0}" "$SPIRA_GATE_BEAD" > "'"$LC_FIX"'/bead/REWORK/$SPIRA_GATE_BEAD"
printf "{\"bead\":{\"bead_id\":\"%s\",\"state\":\"REWORK\",\"tip\":\"\"}}" "$SPIRA_GATE_BEAD" > "'"$LC_FIX"'/show/$SPIRA_GATE_BEAD"'
stub gate.sh "
$reopen_in_lc"'
echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
seed; branch sp-stale
out="$(landing)"
nowant "a bead reopened during the gate is not landed" "landed spira/sp-stale" "$out"
want   "the re-read catches the state change"         "bead is now REWORK" "$out"
is     "and the bead stays in rework"                 REWORK "$(lc_state_of sp-stale)"
drop_branch sp-stale

# Same test but for the gate-failure path: a bead reopened during a failing gate must
# not be reopened AGAIN (which would charge a second attempt).
stub gate.sh "
$reopen_in_lc"'
echo "gate: VERDICT=FAIL reason=stub-fail branch=$1 repo=${2:-?}" >&2; exit 1'
seed; branch sp-stalered
out="$(landing)"
nowant "a bead already reopened is not reopened again by the gate failure" \
       "Reopened by sentinel" "$out"
want   "the re-read catches the state change on the failure path" \
       "bead is now REWORK" "$out"
drop_branch sp-stalered

# Restore the full gate stub for any future tests.
cp "$TMP/gate-full.sh" "$SH/gate.sh"
# --------------------------------------------------------------------------------------
# A PARALLEL DUPLICATE: same feature, different text, one landed.
#
# sp-35pl and sp-dvlq in miniature. Two branches edit the same file with different words;
# one lands and the other's rebase conflicts BECAUSE the base already holds the fix. The
# reopen note must name the landed bead rather than saying "resolve the conflict", because
# the correct resolution is to drop the duplicate's commits.
# --------------------------------------------------------------------------------------
echo
cp "$TMP/gate-full.sh" "$SH/gate.sh"

# Both branches are created from the SAME base before either lands. sp-dupa sorts before
# sp-dupb, so the pass lands sp-dupa first and sp-dupb's rebase conflicts — the exact
# shape the bead is written for. Both add shared.txt from scratch with different content,
# so the rebase after the first landing produces an ADD/ADD conflict.
seed; branch sp-dupa shared.txt "the fix, as sp-dupa wrote it"
branch sp-dupb shared.txt "the fix, as sp-dupb wrote it"
out="$(landing)"
want "the first branch lands"          "landed spira/sp-dupa" "$out"
want "and the duplicate conflicts"     "reopened sp-dupb" "$out"
notes="$(notes_of sp-dupb | tr -s '[:space:]' ' ')"
want "and the note names the other bead"   "sp-dupa" "$notes"
want "and says to check before resolving"  "check whether this work is already landed" "$notes"
nowant "and does NOT say 'not an escalation'" "not an escalation" "$notes"
drop_branch sp-dupa; drop_branch sp-dupb

# A REAL CONFLICT WITH NO OTHER BEAD PRODUCES THE ORIGINAL NOTE.
seed; branch sp-mine shared.txt "my version"
printf '%s\n' "an unrelated edit by nobody" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base edits shared.txt (no bead id)"
timeout 5 git -C "$REPO" push -q origin main; timeout 5 git -C "$REPO" fetch -q origin
out="$(landing)"
want "a conflict with no bead on the base reopens normally" "reopened sp-mine" "$out"
notes="$(notes_of sp-mine)"
want "the note says it is not an escalation" "not an escalation" "$notes"
nowant "and does NOT mention other beads"    "check whether" "$notes"
drop_branch sp-mine

tl_summary

tl_summary
