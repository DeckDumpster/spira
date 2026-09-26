#!/usr/bin/env bash
# tier: T2
# covers: spira/testdb.sh
# Positive control: concurrent borrowers must get DISTINCT SPIRA_DBs. Old code gave each
# borrower TESTDB_DIR; both equal → this assertion fails (law-absence-needs-a-positive-control).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require testdb-concurrent


# Build a fresh shared fixture as the owner (TESTDB_SHARED not set).
unset TESTDB_SHARED TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_MODE \
      TESTDB_BIN TESTDB_PRIVATE_DIR 2>/dev/null || true
testdb_up concurrent || { printf 'SKIP testdb-concurrent: could not build fixture\n' >&2; exit 77; }
OWNER_DIR="$TESTDB_DIR"
OWNER_BL="$TESTDB_BASELINE"
OWNER_NAME="$TESTDB_NAME"
OWNER_MODE="$TESTDB_MODE"

TMP="$(mktemp -d)"
trap 'TESTDB_SHARED=0 testdb_drop; rm -rf "$TMP"' EXIT INT TERM

# Export shared-fixture variables so subprocesses inherit them.
export TESTDB_SHARED=1 TESTDB_NAME="$OWNER_NAME" TESTDB_DIR="$OWNER_DIR" \
       TESTDB_BASELINE="$OWNER_BL" TESTDB_MODE="$OWNER_MODE" \
       TESTDB_BD TESTDB_BIN TESTDB_FAULT_EXIT=75

DB1_FILE="$TMP/db1"; DB2_FILE="$TMP/db2"

(
    . "$HERE/testdb.sh"
    testdb_up borrower1 2>/dev/null || exit "${TESTDB_FAULT_EXIT:-75}"
    printf '%s' "$SPIRA_DB" > "$DB1_FILE"
    testdb_drop
) &
P1=$!

(
    . "$HERE/testdb.sh"
    testdb_up borrower2 2>/dev/null || exit "${TESTDB_FAULT_EXIT:-75}"
    printf '%s' "$SPIRA_DB" > "$DB2_FILE"
    testdb_drop
) &
P2=$!

wait "$P1"; rc1=$?
wait "$P2"; rc2=$?

is "borrower1 testdb_up succeeded" "0" "$rc1"
is "borrower2 testdb_up succeeded" "0" "$rc2"

DB1="$(cat "$DB1_FILE" 2>/dev/null || true)"
DB2="$(cat "$DB2_FILE" 2>/dev/null || true)"

if [ -n "$DB1" ] && [ -n "$DB2" ] && [ "$DB1" != "$DB2" ]; then
    ok "concurrent borrowers got distinct SPIRA_DBs (no shared state)"
elif [ -n "$DB1" ] && [ "$DB1" = "$DB2" ]; then
    bad "concurrent borrowers must get distinct SPIRA_DBs" "both got [$DB1]"
else
    bad "concurrent borrowers must get distinct SPIRA_DBs" "one or both empty: [$DB1] [$DB2]"
fi

PRIV_FILE="$TMP/priv"
(
    . "$HERE/testdb.sh"
    testdb_up cleanup-test 2>/dev/null || exit "${TESTDB_FAULT_EXIT:-75}"
    printf '%s' "$SPIRA_DB" > "$PRIV_FILE"
    testdb_drop
)
PRIV_DIR="$(cat "$PRIV_FILE" 2>/dev/null || true)"
if [ -n "$PRIV_DIR" ] && [ ! -d "$PRIV_DIR" ]; then
    ok "testdb_drop removes the borrower's private directory"
elif [ -z "$PRIV_DIR" ]; then
    bad "borrower testdb_up must set SPIRA_DB" "empty"
else
    bad "testdb_drop must remove borrower's private directory" "still exists: $PRIV_DIR"
fi

# Gone-baseline check: a missing TESTDB_BASELINE must exit fixture-fault (75), not produce
# an ambiguous empty result. Capture $? from the subshell directly — testdb_up calls exit
# (not return) on fault, so a file written after the call is never reached. Stderr goes to
# a capture file so a future failure diagnoses itself.
GONE_BL="$TMP/gone-baseline"
STDERR_FILE="$TMP/detect_stderr"
(
    export TESTDB_BASELINE="$GONE_BL"
    . "$HERE/testdb.sh"
    testdb_up detect
) 2>"$STDERR_FILE"
detect_rc=$?
if [ "${detect_rc}" = "75" ]; then
    ok "gone baseline exits fixture-fault (75), not assertion failure"
else
    detect_err="$(cat "$STDERR_FILE" 2>/dev/null || true)"
    bad "gone baseline must exit fixture-fault (75)" "got rc=[${detect_rc}]${detect_err:+ stderr=[$detect_err]}"
fi
tl_summary
