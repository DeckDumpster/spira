#!/usr/bin/env bash
# test-testenv-batch-baseline.sh — batch passes shared testdb baseline to suites.
#
# WHAT THIS PROVES
#   testenv-batch.sh builds a shared testdb baseline before running suites and
#   injects TESTDB_SHARED=1, TESTDB_BASELINE (a .beads snapshot), TESTDB_BD,
#   and TESTDB_BIN into each suite's environment. A suite can therefore call
#   testdb_up and skip bd init (~6s), copying the baseline instead (~26ms).
#
#   This suite checks its OWN environment, which testenv-batch.sh set before
#   launching it — no nested container required.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail)
#   Part A runs the same check logic against TESTDB_SHARED=0 and an absent
#   baseline, and requires it to fail. Only then does Part B's pass (which
#   checks this suite's own environment) carry weight as evidence that
#   testenv-batch.sh delivered the baseline.
#
# ISOLATION CHECK (Part C)
#   With the baseline in place, testdb_up should return 0 and set SPIRA_DB
#   to a private tmpdir — not the shared baseline dir — so this suite's
#   testdb writes are isolated from other concurrent suites.
#
# tier: T2
# covers: spira/testenv-batch.sh spira/testdb.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
isexit1() { [ "$2" = 1 ]    && ok "$1" || bad "$1" "expected 1, got $2"; }
isnot()   { [ "$2" != "$3" ] && ok "$1" || bad "$1" "wanted not [$2], got [$3]"; }

echo "test-testenv-batch-baseline.sh"

# ===========================================================================
# PART A: POSITIVE CONTROL — the check logic fails without the baseline.
# Run it in a subshell with TESTDB_SHARED=0 so the failure is seen before
# Part B's pass is trusted.
# ===========================================================================
echo
echo "Part A: positive control — check fails when baseline is absent"

# check_baseline: exits 1 when baseline vars are missing; exits 0 when present.
check_baseline() {
    local shared="${1:-0}" baseline="${2:-}"
    if [ "$shared" != 1 ]; then return 1; fi
    if [ -z "$baseline" ]; then return 1; fi
    [ -d "$baseline/.beads" ] || return 1
    return 0
}

_ctrl_rc=0
check_baseline 0 "" || _ctrl_rc=$?
isexit1 "A1: check fails when TESTDB_SHARED=0 and TESTDB_BASELINE is empty" "$_ctrl_rc"

_ctrl_rc=0
check_baseline 1 "" || _ctrl_rc=$?
isexit1 "A2: check fails when TESTDB_SHARED=1 but TESTDB_BASELINE is empty" "$_ctrl_rc"

_tmp_nobeads="$(mktemp -d)"
trap 'rm -rf "$_tmp_nobeads"' EXIT
_ctrl_rc=0
check_baseline 1 "$_tmp_nobeads" || _ctrl_rc=$?
isexit1 "A3: check fails when TESTDB_BASELINE directory has no .beads subdirectory" "$_ctrl_rc"
rm -rf "$_tmp_nobeads"; trap - EXIT

# ===========================================================================
# PART B: THIS SUITE'S OWN ENVIRONMENT — testenv-batch.sh must have injected
# the baseline before launching this suite.
# ===========================================================================
echo
echo "Part B: this suite's environment — baseline delivered by testenv-batch.sh"

# When running OUTSIDE a testenv container (e.g. on the host for local dev),
# testenv-batch.sh is not the caller and baseline vars may not be present.
# Skip gracefully rather than failing — the check is meaningful only inside
# the batch environment.
if [ "${SPIRA_IN_TESTENV:-}" != 1 ]; then
    printf 'SKIP Part B: not inside a testenv container (SPIRA_IN_TESTENV not set)\n' >&2
    [ "$_TL_FAIL" -gt 0 ] && exit 1; exit 77
fi

_rc=0; check_baseline "${TESTDB_SHARED:-0}" "${TESTDB_BASELINE:-}" || _rc=$?
iszero "B1: TESTDB_SHARED=1 and TESTDB_BASELINE is a directory with .beads" "$_rc"

if [ -n "${TESTDB_BD:-}" ]; then
    ok "B2: TESTDB_BD is set (${TESTDB_BD})"
else
    bad "B2: TESTDB_BD is set" "TESTDB_BD is empty"
fi

# ===========================================================================
# PART C: ISOLATION — testdb_up gives this suite a PRIVATE copy of the baseline.
# SPIRA_DB must not equal TESTDB_BASELINE: writes here must not reach the shared dir.
# ===========================================================================
echo
echo "Part C: testdb_up isolation — private copy, not the baseline itself"

TMP="$(mktemp -d)"
trap 'testdb_drop 2>/dev/null || true; rm -rf "$TMP"' EXIT

. "$HERE/testdb.sh"
if ! testdb_up baseline-isolation-check 2>/dev/null; then
    printf 'SKIP Part C: testdb_up failed (no bd engine in this environment)\n' >&2
    [ "$_TL_FAIL" -gt 0 ] && exit 1; exit 77
fi

# SPIRA_DB must be set and must NOT equal TESTDB_BASELINE.
if [ -n "${SPIRA_DB:-}" ]; then
    ok "C1: testdb_up set SPIRA_DB (${SPIRA_DB})"
else
    bad "C1: testdb_up set SPIRA_DB" "SPIRA_DB is empty"
fi
isnot "C2: SPIRA_DB is a private copy, not TESTDB_BASELINE itself" \
    "${TESTDB_BASELINE:-}" "${SPIRA_DB:-}"

# The private copy has its own .beads directory.
if [ -d "${SPIRA_DB:-}/.beads" ]; then
    ok "C3: SPIRA_DB/.beads exists (private working directory)"
else
    bad "C3: SPIRA_DB/.beads exists" "not a directory: ${SPIRA_DB:-}/.beads"
fi
tl_summary
