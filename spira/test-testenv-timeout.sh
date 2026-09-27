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
#   C. A suite that leaks a background child (backgrounds a long sleep, exits 0
#      itself) does not wedge the batch, and the suite after it still runs. Each
#      suite runs under `timeout podman exec ...`, not a bare host pipe: `podman
#      exec` returns as soon as the directly-exec'd process exits, regardless of
#      what it backgrounded, so the failure this suite's UC-30 originally names
#      (sp-a8c5, a host-side pipe held open by an orphan) does not reach this
#      runner. This proves only "does not wedge" — the leak itself is not
#      detected or classified here; see docs/test-plan/test-infrastructure.md G17.
#
# SEEN TO FAIL AGAINST UNFIXED TREE (law-a-regression-test-must-be-seen-to-fail):
#   Against original testenv-batch.sh (before per-suite timeout):
#     SPIRA_SUITE_TIMEOUT=2 bash testenv-batch.sh --suites test-fx-slow.sh,...
#     The batch runs indefinitely (no timeout), test A1 never produces a result,
#     or the suite finishes after the test has already failed the timeout assertion.
#     A1 would fail: status field is "ok", not "timeout".
#     A2 would fail: the after-suite has no result file (slow suite ate the run).
#   Part C has no prior broken version to seen-red against — testenv-batch.sh has
#   always run suites through `podman exec`, never the host pipe sp-a8c5 fixed.
#   Its own outer `timeout` guard is what turns a future regression into a loud
#   failure instead of a silent hang (law-absence-needs-a-positive-control).
#
# host-reason: The per-suite timeout is applied by the host's `timeout` command
#              around each podman exec; Parts A and C require a container.
# tier: T0
# covers: spira/testenv-batch.sh UC-test-infrastructure-30

# covers: spira/testenv-batch.sh UC-test-infrastructure-30
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

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
# BRANCH_WT (what testenv-batch.sh actually bind-mounts as /workspace) is a
# `git worktree add` of the target branch — untracked files in $FIXTURE never
# reach it, only what the branch tip has committed — so each fixture suite
# must be committed to `topic`, not just written into the working tree.
cp "$SUITE_HOST/test-fx-slow.sh"  "$FIXTURE/spira/test-fx-slow.sh"
cp "$SUITE_HOST/test-fx-after.sh" "$FIXTURE/spira/test-fx-after.sh"
git -C "$FIXTURE" add spira/test-fx-slow.sh spira/test-fx-after.sh
git -C "$FIXTURE" commit -q -m "add timeout/after fixture suites"

# ===========================================================================
# CONTAINER AVAILABILITY CHECK
# ===========================================================================
echo
echo "Container availability check"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-timeout.sh: podman not on PATH\n' >&2
    [ "$_TL_FAIL" -gt 0 ] && exit 1; exit 77
}

PRE_CNAME="spira-timeout-preflight-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-testenv-timeout.sh: container did not start\n' >&2
    [ "$_TL_FAIL" -gt 0 ] && exit 1; exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP test-testenv-timeout.sh: user systemd not available\n' >&2
    [ "$_TL_FAIL" -gt 0 ] && exit 1; exit 77
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
# PART C: a leaked background child does not wedge the batch (UC-30).
# SPIRA_SUITE_TIMEOUT=600 — far longer than this test's own outer guard —
# isolates this from Part A's kill path: nothing here should need the
# per-suite timeout to fire for the batch to come back.
# ===========================================================================
echo
echo "Part C: leaked background child does not wedge the batch"

cat > "$SUITE_HOST/test-fx-leaky.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
sleep 300 &
printf '  ok    test-fx-leaky ran\n'
exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-leaky.sh"
cp "$SUITE_HOST/test-fx-leaky.sh" "$FIXTURE/spira/test-fx-leaky.sh"
git -C "$FIXTURE" add spira/test-fx-leaky.sh
git -C "$FIXTURE" commit -q -m "add leaky fixture suite"

RESULTS_ROOT_C="$TMP/results-C"
c_t0="$(date +%s)"
rc_c=0
# The outer `timeout` is this check's own positive control: if podman exec ever
# regressed into waiting on a leaked child's stdout, this would hang and fail
# loudly at 40s instead of the suite silently reporting a pass it never checked.
SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_C" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="bto-c-$$" \
SPIRA_SUITE_TIMEOUT=600 \
    timeout 40 bash "$BATCH" --mode serial --suites "test-fx-leaky.sh,test-fx-after.sh" \
         topic "$FIXTURE" || rc_c=$?
c_elapsed=$(( $(date +%s) - c_t0 ))

[ "$c_elapsed" -lt 40 ] \
    && ok "C1: batch returned in ${c_elapsed}s — not wedged on the leaked child (outer guard was 40s)" \
    || bad "C1: batch returned well under the outer guard" "took ${c_elapsed}s, rc=$rc_c"

RD_C="$(find_results_dir "$RESULTS_ROOT_C")"
[ -n "$RD_C" ] \
    && ok "C2: results directory created" \
    || bad "C2: results directory created" "not found under $RESULTS_ROOT_C"

if [ -n "$RD_C" ]; then
    isfile "C3: leaky suite has a result file (podman exec returned)" \
           "$RD_C/test-fx-leaky.sh.result"
    if [ -f "$RD_C/test-fx-leaky.sh.result" ]; then
        _leaky_status="$(awk '{print $1}' "$RD_C/test-fx-leaky.sh.result")"
        [ "$_leaky_status" = ok ] \
            && ok "C3: leaky suite's own exit (0) is recorded, not read as a hang" \
            || bad "C3: leaky suite's own exit (0) is recorded" "got status '$_leaky_status'"
    fi
    isfile "C4: suite after the leaker has a result file (batch continued)" \
           "$RD_C/test-fx-after.sh.result"
    if [ -f "$RD_C/test-fx-after.sh.result" ]; then
        _after_c_status="$(awk '{print $1}' "$RD_C/test-fx-after.sh.result")"
        [ "$_after_c_status" = ok ] \
            && ok "C4: suite after the leaker ran and passed" \
            || bad "C4: suite after the leaker ran and passed" "got status '$_after_c_status'"
    fi
fi

# ===========================================================================
# SUMMARY
# ===========================================================================
echo
tl_summary
