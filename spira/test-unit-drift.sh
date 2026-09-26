#!/usr/bin/env bash
# tier: T2
# covers: systemd/install.sh spira/skew.sh UC-instance-lifecycle-34
#
# test-unit-drift.sh — a landed template change leaves the installed unit stale, and is
# detected.
#
#   ./test-unit-drift.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# systemd/install.sh renders templates into units and copies them to ~/.config/systemd/user.
# When a fast-forward changes a template, the installed copy stays stale and nothing detects
# it — install.sh --diff exists to find that state, but nothing ran it until skew.sh gained
# a STALE finding. This suite proves the detector works: a matching unit passes, a differing
# one is caught, and a check that cannot run says so rather than reporting clean.
#
# THE FIXTURE IS BUILT FROM THE REAL INSTALLER, not from a model of what it does. install.sh
# --render produces the rendered units; those are written to a fake DEST, one is modified, and
# --diff is asked to compare. A test that modelled the rendering would reproduce whichever half
# of the substitution the test author remembered, and miss the same placeholders the real one
# does (law-prefer-the-real-dependency). The fixture and the render itself come from
# lib-test-install.sh, shared with the other suites that drive this same installer output.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION (law-absence-needs-a-positive-control). Before
# claiming the check finds a stale unit, prove it finds anything at all — the installed
# directory must exist, the installer must render without error, and the check on a matching
# set must report clean. A --diff that always exits 0 would pass every assertion after this
# one.
#
# THE ENVIRONMENT IS EXPLICIT AND MINIMAL. HOME is pointed at a temp directory so DEST
# resolves there and never at the operator's real units. SPIRA_CONF is set to a nonexistent
# file so no box configuration leaks in (law-gates-run-in-a-clean-environment).
#
# defect: sp-0dx3
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/lib-test-install.sh"

echo "test-unit-drift.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

FIXTURE="$TMP/harness"
tinstall_fixture "$FIXTURE"

DEST="$TMP/home/.config/systemd/user"
mkdir -p "$DEST"

# The installer, run in a controlled environment. HOME decides DEST.
inst() {
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        bash "$FIXTURE/systemd/install.sh" "$@" 2>&1
}

# ==========================================================================
echo
echo "positive control — the installer can render and the fixture is sane:"
# ==========================================================================
rendered="$(tinstall_render "$FIXTURE" "$TMP/home")"; rc=$?
is "render exits 0" "0" "$rc"
want "render produces output" "=====" "$rendered"

tinstall_write_dest "$DEST" "$rendered"
installed_count="$(find "$DEST" -maxdepth 1 -type f | wc -l)"
[ "$installed_count" -gt 0 ] && ok "rendered $installed_count unit(s) into DEST" \
    || bad "rendered units into DEST" "no files in $DEST"

# ==========================================================================
echo
echo "matching units — --diff reports clean:"
# ==========================================================================
diff_out="$(inst --diff 2>&1)"; rc=$?
is "diff exits 0 when installed matches rendered" "0" "$rc"
want "diff says units match" "installed units match" "$diff_out"

# ==========================================================================
echo
echo "stale unit — --diff catches a modified installed copy:"
# ==========================================================================
# Pick one installed unit and append a line.
stale_unit="$(find "$DEST" -maxdepth 1 -name '*.service' -type f | head -1)"
if [ -n "$stale_unit" ]; then
    printf '\n# stale modification by test fixture\n' >> "$stale_unit"
    diff_out="$(inst --diff 2>&1)"; rc=$?
    is "diff exits non-zero on stale unit" "1" "$rc"
    want "diff names the stale unit" "DIFFERS" "$diff_out"
    want "diff shows what changed" "stale modification" "$diff_out"
else
    bad "stale unit test" "no .service file found in DEST to modify"
fi

# ==========================================================================
echo
echo "missing unit — --diff catches an absent installed copy:"
# ==========================================================================
# Remove all installed copies and check.
rm -f "$DEST"/*
diff_out="$(inst --diff 2>&1)"; rc=$?
is "diff exits non-zero when units are missing" "1" "$rc"
want "diff names at least one MISSING unit" "MISSING" "$diff_out"

# ==========================================================================
echo
echo "skew.sh units — the standalone entry point:"
# ==========================================================================
# Set up a tree where skew.sh can find install.sh at $SPIRA_HOME/../systemd/install.sh.
# SPIRA_HOME is $FIXTURE/spira, so it looks at $FIXTURE/systemd/install.sh.

# Restore matching units from the cached render.
tinstall_write_dest "$DEST" "$rendered"

skew_units() {
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$FIXTURE/spira" SPIRA_REPO="$FIXTURE" \
        SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        bash "$HERE/skew.sh" units 2>&1
}

out="$(skew_units)"; rc=$?
is "skew.sh units exits 0 when clean" "0" "$rc"
want "skew.sh units says units match" "units match" "$out"

# Make one unit stale.
stale_unit="$(find "$DEST" -maxdepth 1 -name '*.service' -type f | head -1)"
if [ -n "$stale_unit" ]; then
    printf '\n# stale\n' >> "$stale_unit"
    out="$(skew_units)"; rc=$?
    is "skew.sh units exits 1 on drift" "1" "$rc"
    want "skew.sh units names the diff" "DIFFERS" "$out"
fi

# ==========================================================================
echo
echo "skew.sh units — missing installer:"
# ==========================================================================
# Point SPIRA_HOME at a directory with no systemd/ sibling.
out="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$TMP/empty-spira" SPIRA_REPO="$TMP" \
    SPIRA_DOLT_DATA="" \
    SPIRA_TESTDB_DATA="" \
    bash "$HERE/skew.sh" units 2>&1)"; rc=$?
is "skew.sh units exits 3 when installer missing" "3" "$rc"
want "skew.sh units names the missing installer" "missing" "$out"

tl_summary
