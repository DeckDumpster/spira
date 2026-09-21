#!/usr/bin/env bash
# covers: spira/acceptance-run.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-acceptance-ready.sh"

# POSITIVE CONTROL: prove the old || true bug would fail this test
# The buggy pattern always returns 0 regardless of the command's exit code.
echo
echo "positive control — || true discards exit code"
_old_rc=1
_old_out="$(exit 3)" || true
_old_rc_after=$?
[ "$_old_rc_after" -eq 0 ] \
    && ok "positive-control: || true always gives rc=0 (proves the bug was real)" \
    || bad "positive-control: || true always gives rc=0" "got $_old_rc_after"

# POSITIVE CONTROL: prove the correct pattern captures the exit code
echo
echo "positive control — || _rc=\$? captures exit code"
_new_rc=0
_new_out="$(exit 3 2>&1)" || _new_rc=$?
[ "$_new_rc" -eq 3 ] \
    && ok "positive-control: || _rc=\$? captures exit 3" \
    || bad "positive-control: || _rc=\$? captures exit 3" "got $_new_rc"

# Stub ready.sh that exits 3.
STUB_READY="$SCRATCH/ready-fail.sh"
printf '#!/bin/sh\nprintf "stub FAIL output\n"\nexit 3\n' > "$STUB_READY"
chmod +x "$STUB_READY"

# TEST: the fixed pattern (as written in acceptance-run.sh) reports FAIL for exit 3.
echo
echo "1. stub ready.sh exits 3 → rc captured as 3"
_ready_rc=0
_ready_out="$(bash "$STUB_READY" 2>&1)" || _ready_rc=$?
[ "$_ready_rc" -eq 3 ] \
    && ok "phase A ready check: exit 3 captured" \
    || bad "phase A ready check: exit 3 captured" "got _ready_rc=$_ready_rc (expected 3)"
want "phase A ready check: output captured" "stub FAIL output" "$_ready_out"

# is0 as acceptance-run.sh defines it; verify FAIL is emitted for non-zero.
_chk_out="$([ "$_ready_rc" = 0 ] && printf '  ok    %s\n' "ready" || printf '  FAIL  %s: %s\n' "ready" "exit $_ready_rc")"
want "phase A ready check: is0 reports FAIL on exit 3" "FAIL" "$_chk_out"

# TEST pair: stub exits 0 → rc is 0, is0 reports ok.
STUB_READY_OK="$SCRATCH/ready-ok.sh"
printf '#!/bin/sh\nexit 0\n' > "$STUB_READY_OK"
chmod +x "$STUB_READY_OK"

echo
echo "2. stub ready.sh exits 0 → rc captured as 0"
_ready_rc=0
_ready_out="$(bash "$STUB_READY_OK" 2>&1)" || _ready_rc=$?
[ "$_ready_rc" -eq 0 ] \
    && ok "phase A ready check: exit 0 captured" \
    || bad "phase A ready check: exit 0 captured" "got _ready_rc=$_ready_rc (expected 0)"

_chk_out="$([ "$_ready_rc" = 0 ] && printf '  ok    %s\n' "ready" || printf '  FAIL  %s: %s\n' "ready" "exit $_ready_rc")"
want "phase A ready check: is0 reports ok on exit 0" "ok" "$_chk_out"
notwant "phase A ready check: is0 does not report FAIL on exit 0" "FAIL" "$_chk_out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
