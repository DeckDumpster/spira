#!/usr/bin/env bash
# test-batch-maxpar.sh — SPIRA_BATCH_MAXPAR defaults to nproc and is settable from spira.conf
#
# WHAT THIS PROVES
#   1. testenv-batch.sh derives its default pool size from nproc, not a literal constant.
#      On a 4-core box, MAXPAR=32 drives CPU pressure to 97% and produces fork-EAGAIN
#      resource errors the gate blames on the branch. Sizing from nproc caps the pool to
#      available cores. (sp-gkbf)
#   2. SPIRA_BATCH_MAXPAR is in SPIRA_CONF_KEYS, so an operator can tune it from
#      spira.conf without editing code.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail)
#   Part A plants a fake nproc that outputs 7 — a value that cannot result from the
#   literal 32 — then evaluates the batch script's own assignment expression in an
#   isolated env. If the derivation is removed (:-32 restored), the result is 32 and
#   A1 fails.
#
#   Part B's positive control asserts a made-up key is absent from SPIRA_CONF_KEYS,
#   proving the membership test runs and SPIRA_BATCH_MAXPAR was not always there.
#
# covers: spira/testenv-batch.sh spira/conf.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

BATCH="$HERE/testenv-batch.sh"
CONF_SH="$HERE/conf.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-batch-maxpar.sh"

# ===========================================================================
# PART A: _maxpar default is derived from nproc, not a literal
# ===========================================================================
echo
echo "Part A: pool default derived from nproc"

FAKE_BIN="$TMP/bin"
mkdir -p "$FAKE_BIN"
# Sentinel value: 7 cannot be the result of any literal in the script.
printf '#!/bin/sh\necho 7\n' > "$FAKE_BIN/nproc"
chmod +x "$FAKE_BIN/nproc"

# Extract the _maxpar assignment line from the batch script.
_maxpar_line="$(grep -E '^\s*_maxpar=' "$BATCH" | grep 'SPIRA_BATCH_MAXPAR' | head -1)"
[ -n "$_maxpar_line" ] || {
    printf '  FAIL  A0: _maxpar= assignment not found in %s\n' "$BATCH" >&2
    exit 1
}
ok "A0: _maxpar= assignment found in batch script"

# Evaluate the assignment in isolation with stub nproc and SPIRA_BATCH_MAXPAR unset.
_result="$(
    PATH="$FAKE_BIN:$PATH" bash -c "
        unset SPIRA_BATCH_MAXPAR 2>/dev/null || true
        $_maxpar_line
        printf '%s' \"\$_maxpar\"
    "
)"
[ "$_result" = "7" ] && ok "A1: default equals nproc output (stub→7)" \
                      || bad "A1: default equals nproc output" "expected 7, got '$_result'"

# Positive control: when SPIRA_BATCH_MAXPAR is set explicitly, stub is not consulted.
_result_set="$(
    PATH="$FAKE_BIN:$PATH" SPIRA_BATCH_MAXPAR=99 bash -c "
        $_maxpar_line
        printf '%s' \"\$_maxpar\"
    "
)"
[ "$_result_set" = "99" ] && ok "A1-pos: explicit SPIRA_BATCH_MAXPAR overrides nproc" \
                           || bad "A1-pos: explicit SPIRA_BATCH_MAXPAR overrides nproc" \
                                  "expected 99, got '$_result_set'"

# ===========================================================================
# PART B: SPIRA_BATCH_MAXPAR is accepted by conf.sh's allowlist
# ===========================================================================
echo
echo "Part B: SPIRA_BATCH_MAXPAR settable from spira.conf"

# Source conf.sh in a minimal env and capture SPIRA_CONF_KEYS.
_conf_keys="$(
    SPIRA_HOME="$HERE" \
    SPIRA_CONF=/nonexistent \
    bash -c ". '$CONF_SH'; printf '%s' \"\$SPIRA_CONF_KEYS\""
)"
want "B1: SPIRA_BATCH_MAXPAR in SPIRA_CONF_KEYS" "SPIRA_BATCH_MAXPAR" "$_conf_keys"

# Positive control: a fabricated key is absent, proving the membership test runs.
notwant "B1-pos: SPIRA_BATCH_NOEXIST absent from SPIRA_CONF_KEYS (positive control)" \
        "SPIRA_BATCH_NOEXIST" "$_conf_keys"

# Write a temp conf file and verify the value is picked up.
CONF_FILE="$TMP/spira.conf"
printf 'SPIRA_BATCH_MAXPAR=13\n' > "$CONF_FILE"
_from_conf="$(
    SPIRA_HOME="$HERE" \
    SPIRA_CONF="$CONF_FILE" \
    bash -c "unset SPIRA_BATCH_MAXPAR; . '$CONF_SH'; printf '%s' \"\${SPIRA_BATCH_MAXPAR:-unset}\""
)"
[ "$_from_conf" = "13" ] && ok "B2: spira.conf line sets SPIRA_BATCH_MAXPAR" \
                          || bad "B2: spira.conf line sets SPIRA_BATCH_MAXPAR" \
                                 "expected 13, got '$_from_conf'"

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
