#!/usr/bin/env bash
#
# test-gate-check-stderr.sh — gate-check.sh writes nothing to stderr on a clean pass.
#
#   ./test-gate-check-stderr.sh
#
# A permanently noisy check cannot be distinguished from a broken one.
# This suite proves gate-check.sh is silent on stderr when there is nothing
# wrong, and that it WOULD produce output if a command it calls does not exist
# (positive control — without it a test that suppresses stderr looks the same
# as a test that captures and checks it).
#
# POSITIVE CONTROL FIRST. A stub that makes bd unavailable is injected; the
# script must produce at least one error line. The real check runs second with
# a working fixture: zero stderr lines required.
#
# tier: T1
# covers: spira/gate-check.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-gate-check-stderr
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

testdb_up gate_check_stderr || { echo "test-gate-check-stderr: could not build fixture"; exit 1; }

SH="$TMP/spira"
mkdir -p "$SH"
cp "$HERE/gate-check.sh" "$HERE/lib.sh" "$HERE/conf.sh" \
   "$HERE/suite-covers.sh" "$HERE/incident.sh" "$SH/"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL: confirm that a broken environment DOES produce stderr.
# A missing bd command is the simplest offender: gate-check calls bdq (which wraps bd)
# and will print an error if the binary is absent.
# --------------------------------------------------------------------------------------
echo "positive control — broken environment produces stderr:"

touch "$TMP/empty-map"
absent_err="$(SPIRA_HOME="$SH" SPIRA_REPO="$TMP" SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$TMP/empty-map" \
    SPIRA_CONF="$TMP/no.conf" SPIRA_BD="$TMP/no-such-binary" \
    bash "$SH/gate-check.sh" 2>&1 >/dev/null || true)"

if [ -n "$absent_err" ]; then
    ok "broken environment produces stderr (positive control)"
else
    bad "broken environment produces stderr" "expected stderr output, got none — the test cannot distinguish clean from broken"
fi

# --------------------------------------------------------------------------------------
# REAL CHECK: a clean pass with a working fixture and no open gates must be silent.
# --------------------------------------------------------------------------------------
echo
echo "clean pass produces no stderr:"

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira","plan"]}
JSONL

clean_err="$(SPIRA_HOME="$SH" SPIRA_REPO="$TMP" SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$TMP/empty-map" \
    SPIRA_CONF="$TMP/no.conf" \
    bash "$SH/gate-check.sh" 2>&1 >/dev/null)"

if [ -z "$clean_err" ]; then
    ok "gate-check.sh clean pass is silent on stderr"
else
    bad "gate-check.sh clean pass is silent on stderr" "got: $clean_err"
fi
tl_summary
