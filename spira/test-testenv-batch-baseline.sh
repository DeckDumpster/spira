#!/usr/bin/env bash
# covers: spira/testenv-batch.sh spira/testdb.sh
# Sequential pair test: bead created in suite A absent from suite B.
# Proves that testenv-batch.sh's shared baseline gives each suite an
# independent fixture copy — TESTDB_PRIVATE_DIR is per-suite, not shared.
#
# POSITIVE CONTROL: bead is visible within the same fixture session, so A3's
# absence is not vacuous silence (law-absence-needs-a-positive-control).

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testdb.sh"
testdb_require testdb-pair

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-testenv-batch-baseline.sh"

TMP="$(mktemp -d)"
trap 'TESTDB_SHARED=0 testdb_drop 2>/dev/null || true; rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Build a shared baseline — one bd init, captures TESTDB_BASELINE.
# Simulates what testenv-batch.sh does before starting suites.
# ---------------------------------------------------------------------------
unset TESTDB_SHARED TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_MODE \
      TESTDB_BIN TESTDB_PRIVATE_DIR 2>/dev/null || true
testdb_up baseline-owner || { bad "setup: baseline testdb_up failed" ""; exit 1; }
ok "setup: shared baseline built at $TESTDB_BASELINE"

export TESTDB_SHARED=1 TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_MODE \
       TESTDB_BD TESTDB_BIN TESTDB_FAULT_EXIT=75 SPIRA_BD

_marker="pair-$$"

# ---------------------------------------------------------------------------
# Suite A: copy from baseline, create a bead, list (to prove bead visible), drop.
# ---------------------------------------------------------------------------
_a_rc=0
(
    . "$HERE/testdb.sh"
    testdb_up suite-a 2>/dev/null || exit "${TESTDB_FAULT_EXIT:-75}"
    printf '%s\n' "$SPIRA_DB" > "$TMP/a-db"
    bd -C "$SPIRA_DB" create "${_marker}" --type task \
        --body "pair-test-a" --silent 2>/dev/null || true
    bd -C "$SPIRA_DB" list --limit 0 --json 2>/dev/null > "$TMP/a-list" \
        || printf '[]' > "$TMP/a-list"
    testdb_drop
) || _a_rc=$?

[ "$_a_rc" = 0 ] && ok "A: suite-A testdb cycle ok" \
                  || bad "A: suite-A testdb cycle ok" "rc=$_a_rc"

_a_db="$(cat "$TMP/a-db" 2>/dev/null | tr -d '[:space:]' || true)"
_a_list="$(cat "$TMP/a-list" 2>/dev/null || true)"

# A1 (positive control): the bead IS visible in suite-A's own list.
if [[ "$_a_list" == *"${_marker}"* ]]; then
    ok "A1 positive-control: bead visible in suite-A's own list"
else
    bad "A1 positive-control: bead visible in suite-A's own list" \
        "marker '${_marker}' not found — A3 cannot be trusted if this fails"
fi

# ---------------------------------------------------------------------------
# Suite B: copy from the same baseline, list, verify suite-A's bead absent.
# ---------------------------------------------------------------------------
_b_rc=0
(
    . "$HERE/testdb.sh"
    testdb_up suite-b 2>/dev/null || exit "${TESTDB_FAULT_EXIT:-75}"
    printf '%s\n' "$SPIRA_DB" > "$TMP/b-db"
    bd -C "$SPIRA_DB" list --limit 0 --json 2>/dev/null > "$TMP/b-list" \
        || printf '[]' > "$TMP/b-list"
    testdb_drop
) || _b_rc=$?

[ "$_b_rc" = 0 ] && ok "B: suite-B testdb cycle ok" \
                  || bad "B: suite-B testdb cycle ok" "rc=$_b_rc"

_b_db="$(cat "$TMP/b-db" 2>/dev/null | tr -d '[:space:]' || true)"
_b_list="$(cat "$TMP/b-list" 2>/dev/null || true)"

# A2: each suite got a distinct private database directory.
if [ -n "$_a_db" ] && [ -n "$_b_db" ] && [ "$_a_db" != "$_b_db" ]; then
    ok "A2: suite-A and suite-B got distinct private databases"
else
    bad "A2: suite-A and suite-B got distinct private databases" \
        "a=[$_a_db] b=[$_b_db]"
fi

# A3: suite-B does not see suite-A's bead.
if [[ "$_b_list" != *"${_marker}"* ]]; then
    ok "A3: bead created in suite-A absent from suite-B (fixture isolation holds)"
else
    bad "A3: bead created in suite-A absent from suite-B" \
        "marker '${_marker}' found in suite-B — fixture is shared, not copied"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
