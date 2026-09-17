#!/usr/bin/env bash
#
# test-governor-host-cores.sh — governor reads host CPU count, not the calling unit's quota.
#
# WHAT THIS CATCHES. governor.sh used nproc, which honours CPUQuota in the calling unit.
# On a 16-core host with CPUQuota=40%, nproc returns 1, so SP_CORES='1' and the governor
# sizes budgets for a single-core machine while 55-69% of the host's 16 cores sit idle.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail)
#   An nproc stub that outputs 1 is placed on PATH before governor.sh runs. The unfixed
#   code (nproc on line 65) records SP_CORES=1. The fixed code calls host_cores(), which
#   uses getconf and returns the real online count regardless of the stub.
#
# covers: spira/governor.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-governor-host-cores.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Stub nproc that always outputs 1 — the value a CPUQuota=40% unit produces on this host.
mkdir -p "$TMP/bin"
printf '#!/bin/sh\necho 1\n' > "$TMP/bin/nproc"
chmod +x "$TMP/bin/nproc"

# Confirm the stub is reachable. Without this, absence in Part B proves nothing.
_stub_out="$(PATH="$TMP/bin:$PATH" nproc)"
[ "$_stub_out" = "1" ] && ok "stub nproc returns 1 (positive control)" \
                        || { bad "stub nproc returns 1" "got '$_stub_out'"; exit 1; }

# Real host count: getconf reads the kernel's online CPU bitmap, not the cgroup quota.
_real_cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null)"
{ [ -n "$_real_cores" ] && [ "$_real_cores" -gt 0 ]; } 2>/dev/null \
    && ok "getconf _NPROCESSORS_ONLN is readable ($_real_cores cores)" \
    || { bad "getconf _NPROCESSORS_ONLN is readable" "got '$_real_cores'"; exit 1; }

echo
echo "Part A: host_cores() returns the physical count despite stub nproc"

# Evaluate host_cores in a fresh bash with stub nproc first on PATH.
_hc="$(
    PATH="$TMP/bin:$PATH" \
    SPIRA_RUN="$TMP/run-a" \
    SPIRA_CONF=/nonexistent \
    bash -c "mkdir -p '$TMP/run-a'; . '$HERE/lib.sh' 2>/dev/null; host_cores"
)"
is "A1: host_cores equals getconf (not stub)" "$_real_cores" "$_hc"

echo
echo "Part B: governor records SP_CORES from host_cores, not nproc"

# Run governor.sh with stub nproc on PATH in an isolated scratch environment.
_run="$TMP/spira-run"
mkdir -p "$_run"

PATH="$TMP/bin:$PATH" \
    SPIRA_RUN="$_run" \
    SPIRA_CONF=/nonexistent \
    SPIRA_REPO_MAP=/dev/null \
    SPIRA_WORKSPACES="$TMP" \
    bash "$HERE/governor.sh" >/dev/null 2>&1 || true

_budget="$_run/budget.env"
if [ ! -f "$_budget" ]; then
    bad "B1: governor wrote budget.env" "file absent: $_budget"
else
    ok "B1: governor wrote budget.env"
    _sp_cores="$(grep '^SP_CORES=' "$_budget" | sed "s/^SP_CORES='//;s/'$//")"
    is "B2: SP_CORES equals host count (not nproc stub output of 1)" "$_real_cores" "$_sp_cores"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
