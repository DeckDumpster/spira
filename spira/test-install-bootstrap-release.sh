#!/usr/bin/env bash
#
# test-install-bootstrap-release.sh — install.sh's _bootstrap_decision, called directly.
# install.sh defines it before its argument parsing and returns without running when
# sourced (BASH_SOURCE[0] != $0), so this suite never runs doctor.sh, never renders a
# unit, and never copies a release tree.
#
#   ./test-install-bootstrap-release.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. REFUSE: <prod>'s parent is outside <releases> → exit 2, names "activate.sh",
#    names the SPIRA_PROD path.
# 2. BOOTSTRAP: <prod>'s parent resolves under <releases> → exit 0.
#
# FAIL-FIRST: the refuse case is verified first so a silent bootstrap is believed
# (law-absence-needs-a-positive-control).
#
# tier: T1
# covers: install.sh UC-instance-lifecycle-17
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-install-bootstrap-release.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Sourcing install.sh (not executing it) defines _bootstrap_decision and returns before
# argument parsing, doctor.sh or any phase runs.
. "$HERE/../install.sh"

# ===========================================================================
echo
echo "POSITIVE CONTROL: SPIRA_PROD outside SPIRA_RELEASES refuses"
# ===========================================================================
RELEASES_A="$TMP/releases-a"
OUTSIDE_PROD="$TMP/outside/spira"
mkdir -p "$RELEASES_A"

_refuse_out="$(_bootstrap_decision "$OUTSIDE_PROD" "$RELEASES_A" 2>&1)"; _refuse_rc=$?
wantrc "refuse: exits 2"                  "2" "$_refuse_rc"
want   "refuse: names 'activate.sh'"      "activate.sh" "$_refuse_out"
want   "refuse: names SPIRA_PROD path"    "$OUTSIDE_PROD" "$_refuse_out"

# ===========================================================================
echo
echo "BOOTSTRAP: SPIRA_PROD under SPIRA_RELEASES proceeds"
# ===========================================================================
RELEASES_B="$TMP/releases-b"
UNDER_PROD="$RELEASES_B/current"
mkdir -p "$RELEASES_B"

_boot_out="$(_bootstrap_decision "$UNDER_PROD" "$RELEASES_B" 2>&1)"; _boot_rc=$?
wantrc "bootstrap: exits 0"                             "0" "$_boot_rc"
nowant "bootstrap: does not mention 'activate.sh'"      "activate.sh" "$_boot_out"

tl_summary
