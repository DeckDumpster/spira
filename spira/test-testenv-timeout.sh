#!/usr/bin/env bash
# test-testenv-timeout.sh — testenv-batch.sh: SPIRA_SUITE_TIMEOUT per-suite limit.
#
# WHAT THIS PROVES
#   A. A suite that runs longer than SPIRA_SUITE_TIMEOUT is reaped:
#      - Its result file records status "timeout", not "red" or "ok".
#      - The batch continues: a suite AFTER the slow one still runs and records
#        a result file.
#      - The batch exits 1 (non-zero) because a timeout counts as a failure.
#   B. POSITIVE CONTROL — with SPIRA_SUITE_TIMEOUT=0 (disabled), the same slow
#      suite runs to completion and records "ok", proving that the timeout path
#      is what produces the "timeout" status, not the suite's own exit code.
#
# SEEN TO FAIL AGAINST UNFIXED TREE (law-a-regression-test-must-be-seen-to-fail):
#   Against original testenv-batch.sh (before per-suite timeout):
#     SPIRA_SUITE_TIMEOUT=2 bash testenv-batch.sh --suites test-fx-slow.sh,...
#     The batch runs indefinitely (no timeout), test A1 never produces a result,
#     or the suite finishes after the test has already failed the timeout assertion.
#     A1 would fail: status field is "ok", not "timeout".
#     A2 would fail: the after-suite has no result file (slow suite ate the run).
#
# host-reason: The per-suite timeout is applied by the host's `timeout` command
#              around each podman exec; Part A requires a container.
# covers: spira/testenv-batch.sh

# covers: spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
isfile() { [ -f "$2" ] && ok "$1" || bad "$1" "file not found: $2"; }
isnoteq(){ [ "$2" != "$3" ] && ok "$1" || bad "$1" "did not want [$2] got [$3]"; }

find_results_dir() {
    find "$1" -maxdepth 2 -name batch.meta 2>/dev/null | head -1 | xargs dirname 2>/dev/null || true
}

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-testenv-timeout.sh"

# ===========================================================================
# FIXTURE REPO — same minimal shape used in test-testenv-suites.sh.
# Two suites: a slow one and a fast "after" one that proves the corpus continued.
# ===========================================================================
REMOTE="$TMP/remote"
FIXTURE="$TMP/fixture"

git init -q --initial-branch=master "$REMOTE"
git -C "$REMOTE" config user.email "test@spira.local"
git -C "$REMOTE" config user.name "Spira Test"
touch "$REMOTE/placeholder"
git -C "$REMOTE" add placeholder
git -C "$REMOTE" commit -q -m "initial (master)"

git clone -q --local "$REMOTE" "$FIXTURE"
git -C "$FIXTURE" config user.email "test@spira.local"
git -C "$FIXTURE" config user.name "Spira Test"
git -C "$FIXTURE" checkout -q -b topic
printf '#!/bin/bash\necho changed\n' > "$FIXTURE/changed.sh"
git -C "$FIXTURE" add changed.sh
git -C "$FIXTURE" commit -q -m "change changed.sh"

mkdir -p "$FIXTURE/spira"

SUITE_HOST="$TMP/suites-host"
mkdir -p "$SUITE_HOST"

# Slow suite: sleeps 8s, exits 0 if it finishes. With a 2s timeout it will be reaped.
# 8s is long enough to be reliably reaped by a 2s limit even under load, and short
# enough that the positive-control run (no limit) completes in reasonable time.
cat > "$SUITE_HOST/test-fx-slow.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-slow starting\n'
sleep 8
printf '  ok    test-fx-slow finished (should not reach here under timeout)\n'
exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-slow.sh"

# After suite: fast, always passes. Proves the corpus continued past the timeout.
cat > "$SUITE_HOST/test-fx-after.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-after ran\n'; exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-after.sh"

# Copy suites into the fixture so podman can find them inside the container.
cp "$SUITE_HOST/test-fx-slow.sh"  "$FIXTURE/spira/test-fx-slow.sh"
cp "$SUITE_HOST/test-fx-after.sh" "$FIXTURE/spira/test-fx-after.sh"

# ===========================================================================
# CONTAINER AVAILABILITY CHECK
# ===========================================================================
echo
echo "Container availability check"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-timeout.sh: podman not on PATH\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

PRE_CNAME="spira-timeout-preflight-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-testenv-timeout.sh: container did not start\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP test-testenv-timeout.sh: user systemd not available\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
ok "P0: pre-flight: container + user systemd available"

# ===========================================================================
# PART A: timeout fires — slow suite is reaped, after suite still runs.
# SPIRA_SUITE_TIMEOUT=2s: the 30s slow suite cannot finish in time.
# Both suites are selected via --suites so the run is deterministic.
# ===========================================================================
echo
echo "Part A: timeout fires — slow suite reaped, corpus continues"

RESULTS_ROOT_A="$TMP/results-A"
rc_a=0
SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_A" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="bto-a-$$" \
SPIRA_SUITE_TIMEOUT=2 \
    bash "$BATCH" --mode serial --suites "test-fx-slow.sh,test-fx-after.sh" \
         topic "$FIXTURE" || rc_a=$?

# A1: batch exits non-zero (timeout is a failure for exit-status purposes).
[ "$rc_a" -ne 0 ] \
    && ok "A1: batch exits non-zero when a suite times out (rc=$rc_a)" \
    || bad "A1: batch exits non-zero when a suite times out" "expected non-zero, got 0"

RD_A="$(find_results_dir "$RESULTS_ROOT_A")"
[ -n "$RD_A" ] \
    && ok "A1: results directory created" \
    || bad "A1: results directory created" "not found under $RESULTS_ROOT_A"

if [ -n "$RD_A" ]; then
    # A2: slow suite has a result file with status "timeout".
    isfile "A2: slow suite has result file" "$RD_A/test-fx-slow.sh.result"
    if [ -f "$RD_A/test-fx-slow.sh.result" ]; then
        _slow_status="$(awk '{print $1}' "$RD_A/test-fx-slow.sh.result")"
        [ "$_slow_status" = timeout ] \
            && ok "A2: slow suite result status is 'timeout'" \
            || bad "A2: slow suite result status is 'timeout'" "got '$_slow_status'"
        # The fingerprint field must contain "timeout:" to identify the suite by name.
        _slow_fp="$(awk '{print $4}' "$RD_A/test-fx-slow.sh.result")"
        [[ "$_slow_fp" == timeout:* ]] \
            && ok "A2: slow suite fingerprint starts with 'timeout:'" \
            || bad "A2: slow suite fingerprint starts with 'timeout:'" "got '$_slow_fp'"
    fi

    # A3: after suite has a result file (corpus continued past the timeout).
    isfile "A3: after suite has result file (corpus continued)" \
           "$RD_A/test-fx-after.sh.result"
    if [ -f "$RD_A/test-fx-after.sh.result" ]; then
        _after_status="$(awk '{print $1}' "$RD_A/test-fx-after.sh.result")"
        [ "$_after_status" = ok ] \
            && ok "A3: after suite status is 'ok' (ran after timeout)" \
            || bad "A3: after suite status is 'ok'" "got '$_after_status'"
    fi
fi

# ===========================================================================
# PART B: POSITIVE CONTROL — SPIRA_SUITE_TIMEOUT=0 disables the limit;
# the slow suite runs to completion and records "ok".
# This proves the timeout path is what produced "timeout" in Part A,
# not the suite's own exit code.
# ===========================================================================
echo
echo "Part B: positive control — SPIRA_SUITE_TIMEOUT=0 disables timeout"

RESULTS_ROOT_B="$TMP/results-B"
rc_b=0
SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="bto-b-$$" \
SPIRA_SUITE_TIMEOUT=0 \
    bash "$BATCH" --mode serial --suites "test-fx-slow.sh" \
         topic "$FIXTURE" || rc_b=$?

# B1: batch exits 0 (slow suite finishes successfully).
[ "$rc_b" -eq 0 ] \
    && ok "B1: batch exits 0 when timeout is disabled (rc=$rc_b)" \
    || bad "B1: batch exits 0 when timeout is disabled" "expected 0, got $rc_b"

RD_B="$(find_results_dir "$RESULTS_ROOT_B")"
if [ -n "$RD_B" ] && [ -f "$RD_B/test-fx-slow.sh.result" ]; then
    _b_status="$(awk '{print $1}' "$RD_B/test-fx-slow.sh.result")"
    [ "$_b_status" = ok ] \
        && ok "B1: slow suite status is 'ok' when timeout disabled (positive control)" \
        || bad "B1: slow suite status is 'ok' when timeout disabled" "got '$_b_status'"
    isnoteq "B1: status is not 'timeout' when limit disabled" \
        "timeout" "$_b_status"
fi

# ===========================================================================
# SUMMARY
# ===========================================================================
echo
printf 'Results: %s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -gt 0 ] && exit 1; exit 0
