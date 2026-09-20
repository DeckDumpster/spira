#!/usr/bin/env bash
#
# test-aeon-launch-grammar.sh — --system-prompt-snapshot requires on|off; the bare flag
#   swallows the next argument as its value and the CLI refuses.
#
# WHAT IS UNDER TEST
# ------------------
# aeon.sh and archivist.sh pass --system-prompt-snapshot on to the agent. The installed
# CLI requires an explicit value; a bare flag (no value) causes it to consume the following
# argument (e.g. --system-prompt-file) as the value and exit with "invalid choice". This
# suite confirms the grammar with the real binary: on is accepted, the bare flag is refused.
#
# SKIP when the agent binary is absent. The check cannot be done against a stub — the
# whole point is that the stub does not exercise the real CLI's argument parser.
#
# POSITIVE CONTROL: the binary with the bare flag must fail before the passing check means
# anything. Without it, a binary that ignores all flags would pass both assertions vacuously.
#
# defect: sp-w8l21
# covers: spira/aeon.sh spira/archivist.sh spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "expected exit 0, got $2"; }
notzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "expected non-zero exit, got 0"; }

echo "test-aeon-launch-grammar.sh"

AGENT="${SPIRA_AGENT:-claude}"
AGENT_BIN="$(command -v "$AGENT" 2>/dev/null || true)"
if [ -z "$AGENT_BIN" ]; then
    printf 'SKIP test-aeon-launch-grammar: %s not found on PATH\n' "$AGENT"
    exit 77
fi
# Verify the binary can actually execute — in a test container the host's claude may
# appear on PATH but fail immediately (wrong arch, missing libraries, no session).
"$AGENT_BIN" --version >/dev/null 2>&1; _agent_ver_rc=$?
if [ "$_agent_ver_rc" -ne 0 ]; then
    printf 'SKIP test-aeon-launch-grammar: %s is on PATH but --version exits %d — cannot test grammar\n' \
        "$AGENT" "$_agent_ver_rc"
    exit 77
fi
unset _agent_ver_rc

echo
echo "POSITIVE CONTROL — bare --system-prompt-snapshot (no value) must fail"
# Without this the passing check below means nothing: a binary that ignores all flags
# would also exit 0 with the correct invocation.
"$AGENT_BIN" --system-prompt-snapshot --version >/dev/null 2>&1; _rc=$?
notzero "bare --system-prompt-snapshot (no value) is refused by the CLI" "$_rc"

echo
echo "correct invocation — --system-prompt-snapshot on must succeed"
"$AGENT_BIN" --system-prompt-snapshot on --version >/dev/null 2>&1; _rc=$?
iszero "--system-prompt-snapshot on is accepted" "$_rc"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
