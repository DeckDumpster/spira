#!/usr/bin/env bash
#
# test-runner-deps.sh — the host dependency check discriminates, and attributes.
#
#   ./test-runner-deps.sh
#
# WHAT THIS PROTECTS. runner-deps.sh decides whether a machine can run the suite
# corpus at all. Two ways for it to be wrong, and both are quiet:
#
#   It passes a machine that cannot run anything. Then testenv-batch.sh exits 2,
#   the gate maps that to 75 — "harness fault, run it again" — and every run
#   forever says retry while the real problem is a package nobody installed.
#
#   It fails a machine that can. Then a correct branch is refused for a reason
#   that is not in the branch.
#
# So the suite plants a broken runtime and requires the script to say so, and
# requires the same script to be silent on a machine that works. A check that
# only ever passes has demonstrated nothing.
#
# --check IS WHAT IS TESTED, never the installing path: that one calls apt-get
# and usermod, which a suite must not do to the machine it is running on.
#
# covers: spira/runner-deps.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
SCRIPT="$HERE/runner-deps.sh"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-runner-deps.sh"
echo

if [ ! -x "$SCRIPT" ]; then
    bad "runner-deps.sh is executable" "not found or not executable at $SCRIPT"
    printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "runner-deps.sh is executable"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo
echo "it refuses a machine whose container runtime cannot start:"
# THE POSITIVE CONTROL. A podman that exits non-zero for everything is exactly
# the shape of a runtime that is installed and broken — the case that would
# otherwise reach testenv-batch.sh and be reported as a transient harness fault.
mkdir -p "$TMP/brokenbin"
printf '#!/bin/sh\nexit 1\n' > "$TMP/brokenbin/podman"
chmod +x "$TMP/brokenbin/podman"
out="$(PATH="$TMP/brokenbin:$PATH" bash "$SCRIPT" --check 2>&1)"; rc=$?
if [ "$rc" -eq 0 ]; then
    bad "a broken runtime is refused" "exited 0"
else
    ok "a broken runtime is refused (rc=$rc)"
fi
case "$out" in
    *MISSING*) ok "it names what is missing" ;;
    *)         bad "it names what is missing" "no MISSING line in output" ;;
esac
# THE EXIT CODE IS THE WHOLE POINT. 75 means "the harness broke, retry"; a host
# that is missing a package will be missing it on every retry, so this must be a
# plain failure attributed to the step that owns it.
if [ "$rc" -eq 75 ]; then
    bad "a host gap is not reported as a retryable harness fault" "exited 75"
else
    ok "a host gap is not reported as a retryable harness fault"
fi
case "$out" in
    *"not a fault in the branch"*) ok "it says the branch is not at fault" ;;
    *) bad "it says the branch is not at fault" "no attribution line" ;;
esac

echo
echo "it accepts a machine that works:"
# Only meaningful where the machine genuinely has a working rootless runtime. On
# a box without one this asserts nothing and says so, rather than reporting a
# pass it did not earn.
if command -v podman >/dev/null 2>&1 && podman info >/dev/null 2>&1; then
    if bash "$SCRIPT" --check >/dev/null 2>&1; then
        ok "a working machine passes"
    else
        bad "a working machine passes" "refused a host with a working rootless podman"
    fi
else
    printf '  skip  a working machine passes (no usable podman on this host)\n'
fi

echo
echo "--check changes nothing:"
# An assertion-only mode that installs would make the gate's first step a
# mutation of the machine under test, and a suite could not run it at all.
if grep -qE 'apt-get|usermod|enable-linger' "$SCRIPT"; then
    if grep -q 'CHECK_ONLY' "$SCRIPT"; then
        ok "the mutating calls are behind the check-only guard"
    else
        bad "the mutating calls are behind the check-only guard" "no CHECK_ONLY guard"
    fi
else
    bad "positive control: the script has mutating calls to guard" "none found"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
