#!/usr/bin/env bash
# test-observe-flake-threshold.sh — threshold must be > 1; threshold 1 is forbidden.
# covers: spira/suites.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

# Check 1: threshold > 1 is accepted
SUT="bash $HERE/suites.sh"
TMP="$(mktemp -d)"
trap "rm -rf '$TMP'" EXIT
export SPIRA_SUITES_STATE="$TMP"

# Verify the check catches threshold <= 1 and rejects it
out="$($SUT observe-flake test-dummy.sh 2>&1; true)" || rc=$?
[ -z "${rc:-}" ] && rc=0
echo "Threshold 2 (default): rc=$rc, output=$out"
[ "$rc" -eq 0 ] || { printf 'FAIL: default threshold should be accepted\n' >&2; exit 1; }

# Test with threshold 1 — must be rejected
out2="$(SPIRA_FLAKE_QUARANTINE_AT=1 $SUT observe-flake test-dummy.sh 2>&1; true)" || rc2=$?
[ -z "${rc2:-}" ] && rc2=0
echo "Threshold 1: rc=$rc2, output=$out2"
case "$out2" in
    *"threshold"*"<= 1 is forbidden"*) printf 'PASS: threshold 1 rejected as expected\n' ;;
    *) printf 'FAIL: threshold 1 should be rejected\n' >&2; exit 1 ;;
esac
[ "$rc2" -eq 1 ] || { printf 'FAIL: threshold 1 should return non-zero\n' >&2; exit 1; }

printf 'test-observe-flake-threshold.sh: PASS\n'
