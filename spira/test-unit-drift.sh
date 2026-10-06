#!/usr/bin/env bash
# tier: T2
# covers: install/src/bin/units_install.rs skew/src/* UC-instance-lifecycle-34
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
# it — install.sh --diff exists to find that state, but nothing ran it until skew gained
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

# A release-shaped bin/ beside the fixture's own spira/ and systemd/ (sp-al35q): with
# neither SPIRA_HOME nor SPIRA_REPO set, host_from_env()/templates_dir() both resolve by
# walking up from the units-install binary's OWN location, exactly as a real release's
# pre-activate does. Running units-install FROM $FIXTURE/bin — not from this suite's own
# build output, which has no systemd/ or spira/ sibling at all — is what makes that
# resolution land on THIS fixture's spira/ and systemd/, the same ones tinstall_render's
# explicit SPIRA_HOME="$FIXTURE/spira" used to populate $DEST. Without this, "neither set"
# resolved to two different, self-consistent-but-different release locations and every
# unit showed DIFFERS regardless of whether anything had actually drifted.
mkdir -p "$FIXTURE/bin"
ln -sf "$(command -v units-install)" "$FIXTURE/bin/units-install"

DEST="$TMP/home/.config/systemd/user"
mkdir -p "$DEST"

# The installer, run in a controlled environment. HOME decides DEST.
inst() {
    # A run dir, as every real install has: without one units-install now refuses rather than
    # render StandardOutput=append:/<name>.log (sp-xp0u2).
    tl_config SPIRA_RUN="$TMP/home/run" SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA=""
    # SPIRA_HOME IS THE HOME NOW (locate_home no longer searches): the self-location-by-
    # binary-path trick this comment block used to rely on is gone, so it must be named
    # explicitly — the same fixture that trick used to resolve to (sfail round 3, pattern 1).
    env -i SPIRA_TOML="$SPIRA_TOML" PATH="$FIXTURE/bin:$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$FIXTURE/spira" \
        units-install "$@" 2>&1
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
echo "skew units — the standalone entry point:"
# ==========================================================================
# Set up a tree where skew can find install.sh at $SPIRA_HOME/../systemd/install.sh.
# SPIRA_HOME is $FIXTURE/spira, so it looks at $FIXTURE/systemd/install.sh.

# Restore matching units from the cached render.
tinstall_write_dest "$DEST" "$rendered"

skew_units() {
    tl_config SPIRA_RUN="$TMP/home/run" SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA=""
    env -i SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$FIXTURE/spira" SPIRA_REPO="$FIXTURE" \
        skew units 2>&1
}

out="$(skew_units)"; rc=$?
is "skew units exits 0 when clean" "0" "$rc"
want "skew units says units match" "units match" "$out"

# Make one unit stale.
stale_unit="$(find "$DEST" -maxdepth 1 -name '*.service' -type f | head -1)"
if [ -n "$stale_unit" ]; then
    printf '\n# stale\n' >> "$stale_unit"
    out="$(skew_units)"; rc=$?
    is "skew units exits 1 on drift" "1" "$rc"
    want "skew units names the diff" "DIFFERS" "$out"
fi

# ==========================================================================
echo
echo "skew units — missing installer:"
# ==========================================================================
# units-install is a compiled binary now (sp-31dm0): `skew units` resolves it via
# SPIRA_INSTALL_SH (an explicit pin) first, else a PATH lookup — not a path relative to
# SPIRA_HOME/../systemd, which only made sense for a bash script living beside the old
# systemd/install.sh. $PATH in this suite's own process still has the real units-install
# reachable (testenv built the whole workspace), so the only reliable way to make it
# genuinely unanswerable is to pin SPIRA_INSTALL_SH at something that is not executable —
# exactly skew.sh's own `[ -z "$installer" ] || [ ! -x "$installer" ]` refusal.
mkdir -p "$TMP/empty-spira"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$TMP/empty-spira/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$TMP/empty-spira/"
tl_config SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA=""
out="$(env -i SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$TMP/empty-spira" SPIRA_REPO="$TMP" \
    SPIRA_INSTALL_SH="$TMP/no-such-units-install" \
    skew units 2>&1)"; rc=$?
is "skew units exits 3 when installer missing" "3" "$rc"
want "skew units names the missing installer" "missing" "$out"

tl_summary
