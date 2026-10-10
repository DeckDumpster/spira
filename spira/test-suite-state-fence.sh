#!/usr/bin/env bash
# tier: T1
# requires: testenv
# covers: spira/suite-state-fence.sh testenv/src/suites/* UC-safety-fences-28 UC-test-infrastructure-32
#
# suite-state-fence.sh's own logic — everything that was never suite-state.sh's
# (sp-9gd4e). The parse/lint structural cases (missing reason, bead-less quarantine,
# unknown state, missing suite) moved with suite-state.sh's logic to
# `testenv suites lint` and are covered there (`cargo test -p testenv suites::lint_*`,
# DESIGN-suites.md §6b) and by `testenv::suite::tests` (parse/state_of). What is under
# test here is the fence script itself: locating and calling that binary, reading its
# rows back, and the one rule that was always the fence's own — a quarantine against a
# CLOSED bead has no exit path, so it refuses instead of silently passing (D4/UC-28).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

_sd="$(mktemp -d)"
mkdir -p "$_sd/spira/bin"
cp "$HERE/suite-state-fence.sh" "$_sd/spira/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$_sd/spira/test-fake-suite.sh"
chmod +x "$_sd/spira/test-fake-suite.sh"

# ---------------------------------------------------------------------------
# no state file at all: clean, exit 0 — before the binary is ever invoked
# ---------------------------------------------------------------------------
rm -f "$_sd/spira/suite-state"
_out="$(SPIRA_TESTENV_HARNESS="$_sd" bash "$_sd/spira/suite-state-fence.sh" 2>&1)"; _rc=$?
wantrc "no state file: exits 0"        0             "$_rc"
want   "no state file: says so"        "nothing to lint" "$_out"

# ---------------------------------------------------------------------------
# a clean, well-formed file with no quarantine entries: exit 0
# ---------------------------------------------------------------------------
printf 'test-fake-suite.sh | disabled | 2026-01-01T00:00:00Z | | too slow\n' \
    > "$_sd/spira/suite-state"
_out="$(SPIRA_TESTENV_HARNESS="$_sd" bash "$_sd/spira/suite-state-fence.sh" 2>&1)"; _rc=$?
wantrc "clean file: exits 0"           0             "$_rc"
want   "clean file: reports its count" "clean — 1"   "$_out"

# ---------------------------------------------------------------------------
# testenv missing from PATH: refuse, never silently pass (fail-closed)
# ---------------------------------------------------------------------------
_out="$(SPIRA_TESTENV_HARNESS="$_sd" PATH="/usr/bin:/bin" bash "$_sd/spira/suite-state-fence.sh" 2>&1)"; _rc=$?
wantrc "testenv not on PATH: exits 1"  1             "$_rc"
want   "testenv not on PATH: says so"  "not on PATH" "$_out"

# ---------------------------------------------------------------------------
# CLOSED-BEAD QUARANTINE (D4/UC-safety-fences-28). A quarantine against a CLOSED bead
# has no exit path: suites.sh hygiene requires land_state:LANDED to reactivate one, and
# CLOSED is never LANDED. This is against a stub `bd show --json` rather than a real
# create/close round-trip on a throwaway Dolt DB (test-bead-lint.sh pins the real `show
# --json` shape); what is under test here is the fence's own branch on that shape, which
# a stub answers exactly as well.
# ---------------------------------------------------------------------------
printf '#!/usr/bin/env bash\nprintf %%s '"'"'{"status":"closed"}'"'"'\n' > "$_sd/spira/bin/bd"
chmod +x "$_sd/spira/bin/bd"
printf 'test-fake-suite.sh | quarantined | 2026-01-01T00:00:00Z | sp-closed-fixture | flaky\n' \
    > "$_sd/spira/suite-state"
_cb_out="$(SPIRA_TESTENV_HARNESS="$_sd" SPIRA_BD="$_sd/spira/bin/bd" SPIRA_DB=stub SPIRA_CONF=/dev/null \
    bash "$_sd/spira/suite-state-fence.sh" 2>&1)"; _cb_rc=$?
wantrc "closed-bead quarantine: fence exits 1 (stub bd)" 1 "$_cb_rc"
want   "it names the CLOSED bead"                        "CLOSED" "$_cb_out"

# An open (non-closed) bead behind the same quarantine is not refused on that ground.
printf '#!/usr/bin/env bash\nprintf %%s '"'"'{"status":"open"}'"'"'\n' > "$_sd/spira/bin/bd"
_ob_out="$(SPIRA_TESTENV_HARNESS="$_sd" SPIRA_BD="$_sd/spira/bin/bd" SPIRA_DB=stub SPIRA_CONF=/dev/null \
    bash "$_sd/spira/suite-state-fence.sh" 2>&1)"; _ob_rc=$?
wantrc "open-bead quarantine: fence exits 0"  0           "$_ob_rc"
want   "it reports clean"                     "clean — 1" "$_ob_out"

rm -rf "$_sd"

tl_summary
