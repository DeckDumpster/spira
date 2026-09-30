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
# 1. REFUSE: <prod>'s parent is outside <releases> → exit 2, names "release install-tarball"
#    (sp-jsnbm; was "activate.sh"), names the SPIRA_PROD path.
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
want   "refuse: names 'release install-tarball'" "release install-tarball" "$_refuse_out"
want   "refuse: names SPIRA_PROD path"    "$OUTSIDE_PROD" "$_refuse_out"

# ===========================================================================
echo
echo "GUARD: SPIRA_RELEASES not writable → _bootstrap_release refuses, no stray current"
# ===========================================================================
# Regression for the root-mounted-repo condition (SPIRA_REPO="/", equivalently a container
# whose checkout is bind-mounted at a root-level directory): SPIRA_RELEASES then resolves
# under a path the installing user cannot write. mkdir under it must fail, and
# _bootstrap_release must refuse loudly rather than leave a current symlink pointing at a
# release that was never actually populated — proven here before the clean bootstrap below
# is trusted.
RELEASES_C="$TMP/releases-c"
mkdir -p "$RELEASES_C"
chmod 555 "$RELEASES_C"
_guard_out="$(_bootstrap_release "$RELEASES_C/bootstrap" "$RELEASES_C" "$HERE" 2>&1)"
_guard_rc=$?
chmod 755 "$RELEASES_C"
wantrc "guard: _bootstrap_release exits 2 on an unwritable release dir" "2" "$_guard_rc"
want   "guard: names the unwritable directory"                          "$RELEASES_C" "$_guard_out"
[ ! -L "$RELEASES_C/current" ] \
    && ok  "guard: no current symlink left behind after a failed bootstrap" \
    || bad "guard: current symlink" "found $RELEASES_C/current after mkdir should have failed"

# ===========================================================================
echo
echo "BOOTSTRAP: SPIRA_PROD under SPIRA_RELEASES absent → release dir + current symlink"
# ===========================================================================
RELEASES_B="$TMP/releases-b"
UNDER_PROD="$RELEASES_B/current"
mkdir -p "$RELEASES_B"

_boot_out="$(_bootstrap_decision "$UNDER_PROD" "$RELEASES_B" 2>&1)"; _boot_rc=$?
wantrc "bootstrap: exits 0"                             "0" "$_boot_rc"
nowant "bootstrap: does not mention 'release install-tarball'" "release install-tarball" "$_boot_out"

# The installing clone is a small stand-in, not the real checkout ($HERE, rebound to the repo
# root by sourcing install.sh): under testenv the real checkout carries target/, the in-place
# build whose lock files this user cannot read, and copying it measures the build tree, not
# _bootstrap_release. What is copied is not the property; that it is copied, and current set.
CLONE_B="$TMP/clone-b"
mkdir -p "$CLONE_B/spira"
cp "$HERE/install.sh" "$CLONE_B/install.sh"
cp "$HERE/spira/conf.sh" "$CLONE_B/spira/conf.sh"
_release_rc=0
_bootstrap_release "$RELEASES_B/bootstrap" "$RELEASES_B" "$CLONE_B" || _release_rc=$?
wantrc "bootstrap: _bootstrap_release exits 0"          "0" "$_release_rc"
[ -L "$RELEASES_B/current" ] && [ "$(readlink "$RELEASES_B/current")" = "bootstrap" ] \
    && ok  "bootstrap: current symlink points at bootstrap" \
    || bad "bootstrap: current symlink" "expected $RELEASES_B/current -> bootstrap"
[ -f "$RELEASES_B/bootstrap/install.sh" ] \
    && ok  "bootstrap: release dir holds a copy of the installing clone" \
    || bad "bootstrap: release dir contents" "install.sh missing under $RELEASES_B/bootstrap"

tl_summary
