#!/usr/bin/env bash
#
# test-roster-warn.sh — roster_warnings fires once per exclusion-set change, not per pass.
#
#   ./test-roster-warn.sh
#
# WHY THIS EXISTS. roster_warnings logged a WARN on every sentinel pass for every
# chamber fayth absent from SPIRA_FAYTHS. With concierge deliberately excluded (the slot is
# for builders), the warning fired 966 times — once per pass — training operators to skip
# the sentinel log. That log is where the delivers thrash loop was hiding.
#
# THE FIX. roster_warnings computes a stamp of the exclusion set and writes it to
# $SPIRA_RUN/roster-warn.stamp. A subsequent call with the same exclusion set reads the
# same stamp and returns silently. The warning fires only when the set changes.
#
# WHAT THIS SUITE VERIFIES.
#   1. POSITIVE CONTROL — warning fires on first call with an excluded fayth.
#      Without this, a suppressor that mutes everything is indistinguishable from
#      one that mutes correctly (law-absence-needs-a-positive-control).
#   2. SUPPRESSION — second call with same roster emits no warning.
#   3. RESUME ON CHANGE — third call after roster changes fires again.
#   4. CLEAR ON EMPTY — when no fayths are excluded the stamp file is removed,
#      so a subsequent narrowing is not silently suppressed.
#
# defect: sp-j4j7
# tier: T1
# covers: spira/lib.sh
# hermetic-ok: no database, no systemd; uses a synthetic chamber
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"


T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
RUN="$T/run"; mkdir -p "$RUN"
CHAMBER="$T/chamber"; mkdir -p "$CHAMBER"

# Synthetic chamber: two fayths, alpha and beta.
printf 'FAYTH_LABELS=test\n' > "$CHAMBER/alpha.fayth"
printf 'FAYTH_LABELS=test\n' > "$CHAMBER/beta.fayth"

export SPIRA_HOME="$T"
export SPIRA_RUN="$RUN"
export SPIRA_CONF="$T/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-roster-warn.sh"

# ==========================================================================================
echo
echo "positive control — warning fires when a fayth is excluded from the roster"
# ==========================================================================================
# Plant: call with alpha excluded. The stamp file must not exist beforehand.
rm -f "$RUN/roster-warn.stamp"
out="$(roster_warnings "beta" 2>&1)"
want "warning fires for excluded alpha" "alpha.fayth is in the chamber but not in SPIRA_FAYTHS" "$out"

# ==========================================================================================
echo
echo "suppression — second call with same roster emits no warning"
# ==========================================================================================
# The stamp now exists from the first call. Same roster → same exclusion set → silent.
out="$(roster_warnings "beta" 2>&1)"
nowant "no warning on repeat pass" "alpha.fayth is in the chamber but not in SPIRA_FAYTHS" "$out"

# ==========================================================================================
echo
echo "resume on change — warning fires again when exclusion set changes"
# ==========================================================================================
# Now exclude beta as well. Exclusion set is now {alpha, beta}, different from {alpha}.
out="$(roster_warnings "" 2>&1)"
want "warning fires for alpha after roster change" "alpha.fayth is in the chamber but not in SPIRA_FAYTHS" "$out"
want "warning fires for beta after roster change"  "beta.fayth is in the chamber but not in SPIRA_FAYTHS"  "$out"

# ==========================================================================================
echo
echo "clear on empty — stamp removed when nothing is excluded; next narrowing fires again"
# ==========================================================================================
# Full roster: no exclusions. Stamp should be cleared.
roster_warnings "alpha beta" > /dev/null 2>&1
[ ! -f "$RUN/roster-warn.stamp" ] \
    && ok "stamp file is gone when roster is full" \
    || bad "stamp file is gone when roster is full" "file still exists"
# Now narrow again: the cleared stamp means the warning must fire.
out="$(roster_warnings "beta" 2>&1)"
want "warning fires again after roster re-narrows" "alpha.fayth is in the chamber but not in SPIRA_FAYTHS" "$out"

echo
tl_summary
