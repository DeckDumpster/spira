#!/usr/bin/env bash
#
# test-testdb-failsafe.sh — testdb_up leaves SPIRA_DB unusable on failure.
#
#   ./test-testdb-failsafe.sh
#
# THE DEFECT THIS TESTS. testdb_up returned non-zero and left SPIRA_DB pointing at whatever
# the caller had — production. A suite that did not check the return then wrote into the real
# store. Four suites produced fixture beads in production this way when a shared-fixture reset
# failed mid-gate.
#
# THE FIX. testdb_up now unsets SPIRA_DB at the start of every call. A failure on any path
# leaves it unset: a suite that ignores the return then dies on its first `bd -C "$SPIRA_DB"`
# reference because the variable is unbound under `set -u`, rather than writing to production.
#
# HOW FAILURE IS FORCED. With TESTDB_SHARED=1 and a TESTDB_BASELINE that has no .beads
# subdir, testdb_reset fails and testdb_up returns non-zero — reproducing the shared-fixture
# collapse that triggered the original incident.
#
# POSITIVE CONTROL (development-time). Before the fix, testdb_up returned non-zero and
# left SPIRA_DB pointing at the stand-in; the suite continued and wrote beads into it.
# The assertion "bead count unchanged" failed. That failure was observed before applying
# the fix, confirming this test detects the leak rather than passing for an unrelated reason.
#
# covers: spira/testdb.sh spira/test-cockpit-landed.sh spira/test-cockpit-unlanded.sh
#         spira/test-cockpit-unsent.sh spira/test-loom-page.sh
#         spira/test-landing.sh spira/test-timeout.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require testdb-failsafe

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# Build the "production stand-in" fixture: a real bd store that we check for spurious writes.
# Use a clean call that bypasses any inherited TESTDB_SHARED from the harness environment.
unset TESTDB_SHARED TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_MODE TESTDB_BIN 2>/dev/null || true
testdb_up testdb-failsafe || { printf 'SKIP testdb-failsafe: could not build stand-in fixture\n' >&2; exit 77; }
PROD_DB="$SPIRA_DB"
PROD_BD="$SPIRA_BD"
PROD_DIR="$TESTDB_DIR"
PROD_BASELINE="${TESTDB_BASELINE:-}"
PROD_BIN="${TESTDB_BIN:-}"

TMP="$(mktemp -d)"
INVALID_BASELINE="$(mktemp -d)"   # exists but has no .beads subdir
trap 'rm -rf "$TMP" "$INVALID_BASELINE" "${PROD_DIR:-}" "${PROD_BASELINE:-}" "${PROD_BIN:-}"' EXIT INT TERM

# Count beads in the stand-in before anything touches it.
count_before="$("$PROD_BD" -C "$PROD_DB" list --limit 0 --json 2>/dev/null \
    | grep -c '"id"' 2>/dev/null || printf '0')"

# ---- testdb_up fail-safe: SPIRA_DB must be unset after failure ----
#
# testdb_up's shared-fixture failure path calls exit(TESTDB_FAULT_EXIT) rather than
# returning, so the `if` cannot catch it — it terminates the current shell. Run it
# in a subshell so that exit is caught as a non-zero return code from the subshell.
# Capture SPIRA_DB's value via an EXIT trap, which fires after testdb_up's unset
# but before the subshell closes.
export SPIRA_DB="$PROD_DB" SPIRA_BD="$PROD_BD"
# Set up the forced-failure environment: shared fixture with an invalid baseline.
export TESTDB_SHARED=1 TESTDB_NAME=forced_fail TESTDB_MODE=embedded \
       TESTDB_DIR="$INVALID_BASELINE" TESTDB_BASELINE="$INVALID_BASELINE"

_spira_db_file="$(mktemp)"
(
    trap 'printf "%s" "${SPIRA_DB:-}" > '"$_spira_db_file" EXIT
    testdb_up forced_fail 2>/dev/null  # hermetic-ok: deliberate failure test — return captured via subshell exit code
) 2>/dev/null
up_rc=$?
if [ "$up_rc" -ne 0 ]; then
    ok "testdb_up returns non-zero with forced-failure baseline (rc=$up_rc)"
else
    bad "testdb_up should return non-zero with forced-failure baseline" "returned 0"
fi

# THE KEY ASSERTION: SPIRA_DB must be unset (not pointing at the production stand-in).
# The EXIT trap captured SPIRA_DB's value after testdb_up's unset ran.
_spira_db_after="$(cat "$_spira_db_file" 2>/dev/null)"
rm -f "$_spira_db_file"
if [ -z "$_spira_db_after" ]; then
    ok "SPIRA_DB is unset after testdb_up failure"
else
    bad "SPIRA_DB must be unset after testdb_up failure" "still set to: $_spira_db_after"
fi

# ---- four suites must exit non-zero and must not write to the stand-in ----
#
# Each suite is run in a subprocess with:
#   SPIRA_DB pointing at the stand-in (so any inadvertent write is detectable)
#   TESTDB_SHARED=1 with TESTDB_DIR existing (so testdb_require/testdb_available passes)
#   TESTDB_BASELINE pointing at a dir with no .beads (so testdb_reset fails)
#
# A suite exits non-zero either because it has `|| exit 1` after testdb_up (fix 2) or
# because a later `$SPIRA_DB` reference is unbound under `set -u` (fix 1 alone). Either
# way, no bd writes reach the stand-in.
suite_env=(
    env -i
    PATH="$PATH"
    HOME="$HOME"
    TERM="${TERM:-dumb}"
    SPIRA_DB="$PROD_DB"
    SPIRA_BD="$PROD_BD"
    TESTDB_SHARED=1
    TESTDB_NAME=forced_fail
    TESTDB_MODE=embedded
    TESTDB_DIR="$INVALID_BASELINE"
    TESTDB_BASELINE="$INVALID_BASELINE"
    TESTDB_BD="${TESTDB_BD:-bd-embedded}"
    TESTDB_SERVER_BD="${TESTDB_SERVER_BD:-bd}"
)

# test-landing.sh and test-timeout.sh use testdb with real bd writes and each guard
# with `testdb_up ... || exit 1`; they are included here to confirm the failsafe
# protects them when the shared-fixture reset fails and SPIRA_DB is left unset.
for suite in test-cockpit-landed test-cockpit-unlanded test-cockpit-unsent test-landing test-timeout; do
    out="$("${suite_env[@]}" bash "$HERE/$suite.sh" 2>&1)"; rc=$?
    if [ $rc -ne 0 ]; then
        ok "$suite exits non-zero when testdb_up fails (rc=$rc)"
    else
        bad "$suite must exit non-zero when testdb_up fails" "exited 0"
        printf '  output: %s\n' "$(printf '%s' "$out" | head -5)"
    fi
done

# test-loom-page.sh needs node; skip the stand-in write check for it if node is unavailable,
# but still verify that its testdb_up branch exits non-zero.
loom_rc=0
"${suite_env[@]}" bash "$HERE/test-loom-page.sh" 2>/dev/null; loom_rc=$?
if [ $loom_rc -ne 0 ]; then
    ok "test-loom-page.sh exits non-zero when testdb_up fails (rc=$loom_rc)"
else
    bad "test-loom-page.sh must exit non-zero when testdb_up fails" "exited 0"
fi

# After all four suites ran against the stand-in, its bead count must be unchanged.
count_after="$("$PROD_BD" -C "$PROD_DB" list --limit 0 --json 2>/dev/null \
    | grep -c '"id"' 2>/dev/null || printf '0')"
is "production stand-in bead count unchanged after all suite runs" \
   "$count_before" "$count_after"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
