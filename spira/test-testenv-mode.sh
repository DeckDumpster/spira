#!/usr/bin/env bash
# test-testenv-mode.sh — --mode parallel|serial flag on testenv-batch.sh.
#
# WHAT THIS PROVES
#   A. Flag parsing: bad mode exits 2; default is parallel.
#   B. Mode in results: the 5th field of each .result file matches --mode.
#   C. Parallel isolation: each parallel suite gets a distinct SPIRA_INSTANCE
#      and SPIRA_RUN — asserted by reading the suite's stdout, not assumed.
#   D. Positive control (cross-suite leak): a suite that reads shared state
#      written by an earlier suite passes under serial (shared SPIRA_RUN) and
#      fails under parallel (each suite's SPIRA_RUN is isolated).
#      This is the proof that parallel is actually exercising shared-state
#      isolation, not just running suites concurrently in the same environment.
#   E. batch.meta carries mode= field.
#
# host-reason: Part A tests flag parsing on the host (no container needed).
#              Part B-E requires podman for the container integration.
# covers: spira/testenv-batch.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
isexit1() { [ "$2" = 1 ]    && ok "$1" || bad "$1" "expected 1, got $2"; }
isexit2() { [ "$2" = 2 ]    && ok "$1" || bad "$1" "expected 2, got $2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
isfile()  { [ -f "$2" ] && ok "$1" || bad "$1" "file not found: $2"; }

find_results_dir() {
    find "$1" -maxdepth 2 -name batch.meta 2>/dev/null | head -1 | xargs dirname 2>/dev/null || true
}

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-testenv-mode.sh"

# ===========================================================================
# FIXTURE REPO — same structure as test-testenv-batch.sh: master-based remote.
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

# ===========================================================================
# PART A: FLAG PARSING — no container required.
# ===========================================================================
echo
echo "Part A: flag parsing (no container)"

# A1: unknown mode exits 2.
rc_a1=0
bash "$BATCH" --mode bogus topic "$FIXTURE" >/dev/null 2>&1 || rc_a1=$?
isexit2 "A1: --mode bogus exits 2" "$rc_a1"

# A2: missing branch arg exits 2 even with a valid --mode.
rc_a2=0
bash "$BATCH" --mode serial >/dev/null 2>&1 || rc_a2=$?
isexit2 "A2: no branch arg exits 2" "$rc_a2"

# A3: --mode= form is accepted (no error from flag parsing itself; container
# absence produces exit 2 for a different reason — that's acceptable here).
rc_a3=0
bash "$BATCH" --mode=parallel topic "$FIXTURE" >/dev/null 2>&1 || rc_a3=$?
# We just want it not to exit 2 due to a bad --mode value; container absence
# or no-podman will still produce 2, which is fine — we check the error output.
err_a3="$(bash "$BATCH" --mode=parallel topic "$FIXTURE" 2>&1 >/dev/null || true)"
notwant "A3: --mode=parallel is not rejected as an unknown mode" \
    "--mode must be parallel or serial" "$err_a3"

# ===========================================================================
# PART B–E: CONTAINER INTEGRATION
# ===========================================================================
echo
echo "Part B-E: container integration"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-mode.sh Part B-E: podman not on PATH\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

# Pre-flight: container + user systemd available.
PRE_CNAME="spira-mode-preflight-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-testenv-mode.sh Part B-E: container did not start\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP test-testenv-mode.sh Part B-E: user systemd not available\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
ok "B0: pre-flight: container + user systemd available"

# ---------------------------------------------------------------------------
# B: MODE IN RESULT FILES — 5th field of each .result matches --mode.
# ---------------------------------------------------------------------------
echo
echo "B: mode field in result files"

SUITE_B="$TMP/suites-B"
mkdir -p "$SUITE_B"

cat > "$SUITE_B/test-fx-m.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-m ran\n'; exit 0
EOF
chmod +x "$SUITE_B/test-fx-m.sh"
cp "$SUITE_B/test-fx-m.sh" "$FIXTURE/spira/test-fx-m.sh"

# B1: --mode serial records "serial" as the 5th field.
RESULTS_ROOT_B1="$TMP/results-B1"
rc_b1=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B1" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="bm1-$$" \
    bash "$BATCH" --mode serial topic "$FIXTURE" || rc_b1=$?
iszero "B1: --mode serial exits 0 (all green)" "$rc_b1"
RD_B1="$(find_results_dir "$RESULTS_ROOT_B1")"
if [ -n "$RD_B1" ] && [ -f "$RD_B1/test-fx-m.sh.result" ]; then
    _mode_b1="$(awk '{print $5}' "$RD_B1/test-fx-m.sh.result")"
    [ "$_mode_b1" = serial ] && ok "B1: result 5th field is 'serial'" \
                              || bad "B1: result 5th field is 'serial'" "got '$_mode_b1'"
    isfile "B1: batch.meta written" "$RD_B1/batch.meta"
    if [ -f "$RD_B1/batch.meta" ]; then
        want "B1: batch.meta has mode=serial" "mode=serial" "$(cat "$RD_B1/batch.meta")"
    fi
fi

# B2: default mode (no --mode flag) is parallel.
RESULTS_ROOT_B2="$TMP/results-B2"
rc_b2=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B2" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="bm2-$$" \
    bash "$BATCH" topic "$FIXTURE" || rc_b2=$?
iszero "B2: default (no --mode) exits 0" "$rc_b2"
RD_B2="$(find_results_dir "$RESULTS_ROOT_B2")"
if [ -n "$RD_B2" ] && [ -f "$RD_B2/test-fx-m.sh.result" ]; then
    _mode_b2="$(awk '{print $5}' "$RD_B2/test-fx-m.sh.result")"
    [ "$_mode_b2" = parallel ] && ok "B2: default mode is parallel (5th field is 'parallel')" \
                                || bad "B2: default mode is parallel (5th field is 'parallel')" \
                                       "got '$_mode_b2'"
    if [ -f "$RD_B2/batch.meta" ]; then
        want "B2: batch.meta has mode=parallel" "mode=parallel" "$(cat "$RD_B2/batch.meta")"
    fi
fi

# ---------------------------------------------------------------------------
# C: PARALLEL ISOLATION — each parallel suite gets a distinct SPIRA_INSTANCE
# and SPIRA_RUN. Suites write their env values to stdout; we read .out files.
# ---------------------------------------------------------------------------
echo
echo "C: parallel isolation (distinct SPIRA_INSTANCE and SPIRA_RUN per suite)"

SUITE_C="$TMP/suites-C"
mkdir -p "$SUITE_C"

# Two suites, each printing their SPIRA_INSTANCE and SPIRA_RUN to stdout.
cat > "$SUITE_C/test-fx-p1.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf 'INSTANCE:%s\n' "${SPIRA_INSTANCE:-UNSET}"
printf 'RUN:%s\n'      "${SPIRA_RUN:-UNSET}"
exit 0
EOF
chmod +x "$SUITE_C/test-fx-p1.sh"
cp "$SUITE_C/test-fx-p1.sh" "$FIXTURE/spira/test-fx-p1.sh"

cat > "$SUITE_C/test-fx-p2.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf 'INSTANCE:%s\n' "${SPIRA_INSTANCE:-UNSET}"
printf 'RUN:%s\n'      "${SPIRA_RUN:-UNSET}"
exit 0
EOF
chmod +x "$SUITE_C/test-fx-p2.sh"
cp "$SUITE_C/test-fx-p2.sh" "$FIXTURE/spira/test-fx-p2.sh"

RESULTS_ROOT_C="$TMP/results-C"
rc_c=0
SPIRA_BATCH_SUITE_DIR="$SUITE_C" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_C" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="cp-$$" \
    bash "$BATCH" --mode parallel topic "$FIXTURE" || rc_c=$?
iszero "C: --mode parallel exits 0 (both green)" "$rc_c"

RD_C="$(find_results_dir "$RESULTS_ROOT_C")"
if [ -n "$RD_C" ]; then
    isfile "C: p1 has result file" "$RD_C/test-fx-p1.sh.result"
    isfile "C: p2 has result file" "$RD_C/test-fx-p2.sh.result"
    isfile "C: p1 has output file" "$RD_C/test-fx-p1.sh.out"
    isfile "C: p2 has output file" "$RD_C/test-fx-p2.sh.out"

    _inst_p1=""; _run_p1=""
    if [ -f "$RD_C/test-fx-p1.sh.out" ]; then
        _inst_p1="$(grep '^INSTANCE:' "$RD_C/test-fx-p1.sh.out" | cut -d: -f2-)"
        _run_p1="$(grep  '^RUN:'      "$RD_C/test-fx-p1.sh.out" | cut -d: -f2-)"
    fi
    _inst_p2=""; _run_p2=""
    if [ -f "$RD_C/test-fx-p2.sh.out" ]; then
        _inst_p2="$(grep '^INSTANCE:' "$RD_C/test-fx-p2.sh.out" | cut -d: -f2-)"
        _run_p2="$(grep  '^RUN:'      "$RD_C/test-fx-p2.sh.out" | cut -d: -f2-)"
    fi

    # Both must be non-empty (i.e., SPIRA_INSTANCE and SPIRA_RUN were set).
    [ -n "$_inst_p1" ] && [ "$_inst_p1" != UNSET ] \
        && ok "C: p1 SPIRA_INSTANCE is set" \
        || bad "C: p1 SPIRA_INSTANCE is set" "got '${_inst_p1:-empty}'"
    [ -n "$_inst_p2" ] && [ "$_inst_p2" != UNSET ] \
        && ok "C: p2 SPIRA_INSTANCE is set" \
        || bad "C: p2 SPIRA_INSTANCE is set" "got '${_inst_p2:-empty}'"

    # They must be distinct from each other.
    if [ -n "$_inst_p1" ] && [ -n "$_inst_p2" ]; then
        [ "$_inst_p1" != "$_inst_p2" ] \
            && ok "C: p1 and p2 have distinct SPIRA_INSTANCE ($( printf '%s vs %s' "$_inst_p1" "$_inst_p2"))" \
            || bad "C: p1 and p2 have distinct SPIRA_INSTANCE" \
                   "both got '$_inst_p1'"
    fi
    if [ -n "$_run_p1" ] && [ -n "$_run_p2" ]; then
        [ "$_run_p1" != "$_run_p2" ] \
            && ok "C: p1 and p2 have distinct SPIRA_RUN ($( printf '%s vs %s' "$_run_p1" "$_run_p2"))" \
            || bad "C: p1 and p2 have distinct SPIRA_RUN" \
                   "both got '$_run_p1'"
    fi
fi

# ---------------------------------------------------------------------------
# D: POSITIVE CONTROL — cross-suite leak.
#
# Suite la writes $SPIRA_RUN/shared-state.
# Suite lb reads $SPIRA_RUN/shared-state and passes only if it finds it.
#
# Under serial (same SPIRA_RUN, la runs before lb): lb finds the file → PASS.
# Under parallel (distinct SPIRA_RUN per suite): lb looks in its own RUN dir,
# which la never wrote to → FAIL.
#
# This confirms that --mode parallel actually isolates suites, not merely
# runs them concurrently in the same environment.
# ---------------------------------------------------------------------------
echo
echo "D: positive control — cross-suite leak (serial passes, parallel catches)"

SUITE_D="$TMP/suites-D"
mkdir -p "$SUITE_D"

# la: alphabetically first — runs first in serial; in parallel, concurrent.
# Writes to $SPIRA_RUN/shared-state.
cat > "$SUITE_D/test-fx-la.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
_run="${SPIRA_RUN:-/tmp/spira-batch-missing}"
mkdir -p "$_run"
printf 'written\n' > "$_run/shared-state"
printf '  ok    la: wrote shared-state to %s\n' "$_run"
exit 0
EOF
chmod +x "$SUITE_D/test-fx-la.sh"
cp "$SUITE_D/test-fx-la.sh" "$FIXTURE/spira/test-fx-la.sh"

# lb: reads $SPIRA_RUN/shared-state; passes only if present.
cat > "$SUITE_D/test-fx-lb.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
_run="${SPIRA_RUN:-/tmp/spira-batch-missing}"
if [ -f "$_run/shared-state" ]; then
    printf '  ok    lb: found shared-state in %s\n' "$_run"
    exit 0
else
    printf '  FAIL  lb: shared-state not found in %s\n' "$_run"
    exit 1
fi
EOF
chmod +x "$SUITE_D/test-fx-lb.sh"
cp "$SUITE_D/test-fx-lb.sh" "$FIXTURE/spira/test-fx-lb.sh"

# D1: serial — la runs first (alphabetical), lb finds shared-state → both pass.
RESULTS_ROOT_D1="$TMP/results-D1"
rc_d1=0
SPIRA_BATCH_SUITE_DIR="$SUITE_D" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_D1" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="dl-$$" \
    bash "$BATCH" --mode serial topic "$FIXTURE" || rc_d1=$?
iszero "D1: serial — both suites pass (la writes, lb reads shared SPIRA_RUN)" "$rc_d1"
RD_D1="$(find_results_dir "$RESULTS_ROOT_D1")"
if [ -n "$RD_D1" ]; then
    if [ -f "$RD_D1/test-fx-la.sh.result" ]; then
        _st_la="$(awk '{print $1}' "$RD_D1/test-fx-la.sh.result")"
        [ "$_st_la" = ok ] && ok "D1: la status is ok" \
                            || bad "D1: la status is ok" "got '$_st_la'"
    fi
    if [ -f "$RD_D1/test-fx-lb.sh.result" ]; then
        _st_lb="$(awk '{print $1}' "$RD_D1/test-fx-lb.sh.result")"
        [ "$_st_lb" = ok ] && ok "D1: lb status is ok (found la's shared-state)" \
                            || bad "D1: lb status is ok (found la's shared-state)" \
                                   "got '$_st_lb'"
    fi
fi

# D2: parallel — lb has its own SPIRA_RUN; la's write is invisible to lb → lb RED.
RESULTS_ROOT_D2="$TMP/results-D2"
rc_d2=0
SPIRA_BATCH_SUITE_DIR="$SUITE_D" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_D2" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="dp-$$" \
    bash "$BATCH" --mode parallel topic "$FIXTURE" || rc_d2=$?
isexit1 "D2: parallel — batch exits 1 (lb cannot see la's isolated SPIRA_RUN)" "$rc_d2"
RD_D2="$(find_results_dir "$RESULTS_ROOT_D2")"
if [ -n "$RD_D2" ]; then
    if [ -f "$RD_D2/test-fx-la.sh.result" ]; then
        _st_la2="$(awk '{print $1}' "$RD_D2/test-fx-la.sh.result")"
        [ "$_st_la2" = ok ] && ok "D2: la status is ok (still passes in parallel)" \
                              || bad "D2: la status is ok" "got '$_st_la2'"
    fi
    if [ -f "$RD_D2/test-fx-lb.sh.result" ]; then
        _st_lb2="$(awk '{print $1}' "$RD_D2/test-fx-lb.sh.result")"
        [ "$_st_lb2" = red ] && ok "D2: lb status is red (isolated SPIRA_RUN, leak not found)" \
                              || bad "D2: lb status is red" "got '$_st_lb2'"
        # The result file must record the mode as "parallel".
        _mode_lb2="$(awk '{print $5}' "$RD_D2/test-fx-lb.sh.result")"
        [ "$_mode_lb2" = parallel ] \
            && ok "D2: lb result records mode=parallel" \
            || bad "D2: lb result records mode=parallel" "got '$_mode_lb2'"
    fi
fi

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
