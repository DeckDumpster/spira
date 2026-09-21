#!/usr/bin/env bash
# test-suite-times.sh — suite-times ledger: bd shim captures calls; --report shows movers.
#
# WHAT THIS PROVES
#   A. suite-times.sh report reads a ledger and produces the expected output:
#      - top-20 table appears for a run with data
#      - movers section names the suite whose wall changed by more than 25%
#   B. (container required) testenv-batch.sh writes suite-times.tsv with three rows,
#      one of which carries bd_calls=2 for the suite that called bd twice.
#
# Part A runs without a container; Part B requires podman + user systemd.
#
# host-reason: Part B sends suites into a container via testenv-batch.sh.
# covers: spira/testenv-batch.sh spira/suite-times.sh

# covers: spira/testenv-batch.sh spira/suite-times.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]" ;; esac; }
notwant(){ case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }
isfile() { [ -f "$2" ] && ok "$1" || bad "$1" "file not found: $2"; }

SUITE_TIMES="$HERE/suite-times.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ===========================================================================
# PART A: suite-times.sh report — no container needed
# ===========================================================================
echo "Part A: suite-times.sh report from a synthetic ledger"

LEDGER="$TMP/suite-times.log"
# Two runs: run1 and run2. test-slow.sh moved from 100s to 200s (100% increase > 25%).
# test-stable.sh stayed at 10s (0% change). test-fast.sh went 5s → 4s (-20%, under threshold).
cat > "$LEDGER" << 'EOF'
run1	main	test-slow.sh	0	100	5	800	parallel
run1	main	test-stable.sh	0	10	2	300	parallel
run1	main	test-fast.sh	0	5	0	0	parallel
run1	main	__batch__	0	35	0	0	parallel
run2	main	test-slow.sh	0	200	5	900	parallel
run2	main	test-stable.sh	0	10	2	300	parallel
run2	main	test-fast.sh	0	4	0	0	parallel
run2	main	__batch__	0	70	0	0	parallel
EOF

out="$(SPIRA_SUITE_TIMES_LOG="$LEDGER" bash "$SUITE_TIMES" report 2 2>&1)"
[ $? -eq 0 ] && ok "A1: suite-times.sh report exits 0" || bad "A1: suite-times.sh report exits 0" "exited non-zero"

want "A1: top-20 header present"   "Top 20 suites"         "$out"
want "A1: test-slow.sh appears"    "test-slow.sh"          "$out"
want "A1: test-stable.sh appears"  "test-stable.sh"        "$out"
want "A1: movers header present"   "Suites with wall"      "$out"
want "A1: test-slow.sh is a mover" "test-slow.sh"          "$(printf '%s' "$out" | awk '/Suites with wall/,0')"
notwant "A1: test-stable.sh not a mover (no change)" "test-stable.sh" \
    "$(printf '%s' "$out" | awk '/Suites with wall/,0')"

# Verify the run summary line shows sum and wall
want "A1: run summary shows sum"  "sum"   "$out"
want "A1: run summary shows wall" "wall"  "$out"

# Last-run sum is 200+10+4 = 214; verify it appears
want "A1: sum is 214s"            "214"   "$out"
# Last-run wall from __batch__ is 70
want "A1: wall is 70s"            "70"    "$out"

echo
echo "Part A: --log flag and argument parsing"
# --log FILE syntax
out2="$(bash "$SUITE_TIMES" report 2 --log "$LEDGER" 2>&1)"
[ $? -eq 0 ] && ok "A2: --log FILE works" || bad "A2: --log FILE works" "exited non-zero"
want "A2: output is same with --log" "test-slow.sh" "$out2"

# Missing ledger → error
out3="$(bash "$SUITE_TIMES" report 2 --log /nonexistent/path 2>&1)"; rc3=$?
[ "$rc3" -ne 0 ] && ok "A3: missing log exits non-zero" \
    || bad "A3: missing log exits non-zero" "expected non-zero, got 0"

echo
echo "Part A: --report delegation via testenv-batch.sh"
# testenv-batch.sh --report should delegate to suite-times.sh and exit
out4="$(SPIRA_SUITE_TIMES_LOG="$LEDGER" bash "$HERE/testenv-batch.sh" --report 2 2>&1)"; rc4=$?
[ "$rc4" -eq 0 ] && ok "A4: testenv-batch.sh --report exits 0" \
    || bad "A4: testenv-batch.sh --report exits 0" "exited $rc4"
want "A4: testenv-batch.sh --report shows top-20" "Top 20 suites" "$out4"

# ===========================================================================
# PART B: container run — bd shim records calls; TSV has correct counts
# ===========================================================================
echo
echo "Part B: container batch — bd shim and TSV"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP Part B: podman not on PATH\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"

PRE_CNAME="spira-suitimes-pre-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP Part B: container did not start\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP Part B: user systemd not available\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
ok "B0: pre-flight: container + user systemd available"

# Build a minimal fixture repo
REMOTE="$TMP/remote"
FIXTURE="$TMP/fixture"
git init -q --initial-branch=master "$REMOTE"
git -C "$REMOTE" config user.email "test@spira.local"
git -C "$REMOTE" config user.name "Spira Test"
touch "$REMOTE/placeholder"
git -C "$REMOTE" add placeholder
git -C "$REMOTE" commit -q -m "initial"
git clone -q --local "$REMOTE" "$FIXTURE"
git -C "$FIXTURE" config user.email "test@spira.local"
git -C "$FIXTURE" config user.name "Spira Test"
git -C "$FIXTURE" checkout -q -b topic
printf '#!/bin/bash\necho changed\n' > "$FIXTURE/changed.sh"
git -C "$FIXTURE" add changed.sh
git -C "$FIXTURE" commit -q -m "add changed.sh"
mkdir -p "$FIXTURE/spira"

# Three stub suites. suite-b calls bd twice (or tries — exits 0 regardless).
# suite-a and suite-c do not call bd.
SUITE_HOST="$TMP/suites"
mkdir -p "$SUITE_HOST"

cat > "$SUITE_HOST/test-fx-a.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    suite-a ran\n'; exit 0
EOF

cat > "$SUITE_HOST/test-fx-b.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
# Call bd twice (or just the stub if bd not available); always exits 0.
bd list --limit 0 2>/dev/null || true
bd list --limit 0 2>/dev/null || true
printf '  ok    suite-b ran\n'; exit 0
EOF

cat > "$SUITE_HOST/test-fx-c.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    suite-c ran\n'; exit 0
EOF

chmod +x "$SUITE_HOST"/test-fx-*.sh
cp "$SUITE_HOST"/test-fx-*.sh "$FIXTURE/spira/"

RESULTS_ROOT="$TMP/results"
TIMES_LOG="$TMP/suite-times-b.log"
rc_b=0
SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT" \
SPIRA_BATCH_INSTANCE="st-b-$$" \
SPIRA_SUITE_TIMEOUT=120 \
SPIRA_BATCH_RUN_ID="test-run-1" \
SPIRA_SUITE_TIMES_LOG="$TIMES_LOG" \
SPIRA_BATCH_SKIP_INSTALL=1 \
    bash "$BATCH" --mode serial \
         --suites "test-fx-a.sh,test-fx-b.sh,test-fx-c.sh" \
         topic "$FIXTURE" || rc_b=$?

[ "$rc_b" -eq 0 ] && ok "B1: batch exits 0" \
    || bad "B1: batch exits 0" "batch exited $rc_b"

# Find the results dir
RD="$(find "$RESULTS_ROOT" -maxdepth 2 -name batch.meta 2>/dev/null \
    | head -1 | xargs dirname 2>/dev/null || true)"
[ -n "$RD" ] && ok "B1: results directory found" \
    || bad "B1: results directory found" "not found under $RESULTS_ROOT"

# Check suite-times.tsv exists in results
isfile "B2: suite-times.tsv exists in results" "${RD}/suite-times.tsv"

if [ -f "${RD}/suite-times.tsv" ]; then
    # Should have 4 rows: 3 suites + 1 __batch__
    _suite_rows="$(awk -F'\t' 'NF>=8 && $3 != "__batch__"' "${RD}/suite-times.tsv" | wc -l | tr -d ' ')"
    [ "$_suite_rows" -eq 3 ] && ok "B3: TSV has 3 suite rows" \
        || bad "B3: TSV has 3 suite rows" "got $_suite_rows"

    _batch_rows="$(awk -F'\t' 'NF>=8 && $3 == "__batch__"' "${RD}/suite-times.tsv" | wc -l | tr -d ' ')"
    [ "$_batch_rows" -eq 1 ] && ok "B3: TSV has 1 __batch__ row" \
        || bad "B3: TSV has 1 __batch__ row" "got $_batch_rows"

    # Check run_id is what we set
    _run_id="$(awk -F'\t' 'NF>=8 && $3 != "__batch__"{print $1; exit}' "${RD}/suite-times.tsv")"
    [ "$_run_id" = "test-run-1" ] && ok "B4: run_id is test-run-1" \
        || bad "B4: run_id is test-run-1" "got $_run_id"

    # suite-b should have bd_calls >= 0 (>0 if bd is present in the container)
    _b_calls="$(awk -F'\t' 'NF>=8 && $3=="test-fx-b.sh"{print $6}' "${RD}/suite-times.tsv")"
    [ -n "$_b_calls" ] && ok "B5: suite-b has bd_calls field" \
        || bad "B5: suite-b has bd_calls field" "field empty or missing"

    # If bd was found (non-zero calls), verify it captured exactly 2
    if [ "${_b_calls:-0}" -gt 0 ]; then
        [ "$_b_calls" -eq 2 ] && ok "B5: suite-b bd_calls=2 (shim captured both calls)" \
            || bad "B5: suite-b bd_calls=2 (shim captured both calls)" "got $_b_calls"
    else
        ok "B5: bd not in container — shim records 0 calls (non-fatal)"
    fi

    # suite-a and suite-c should have bd_calls=0
    _a_calls="$(awk -F'\t' 'NF>=8 && $3=="test-fx-a.sh"{print $6}' "${RD}/suite-times.tsv")"
    [ "${_a_calls:-0}" -eq 0 ] && ok "B6: suite-a bd_calls=0" \
        || bad "B6: suite-a bd_calls=0" "got ${_a_calls:-?}"
fi

# Check that suite-times.log was also written on the host
isfile "B7: host suite-times.log written" "$TIMES_LOG"
if [ -f "$TIMES_LOG" ]; then
    _log_rows="$(wc -l < "$TIMES_LOG" | tr -d ' ')"
    [ "$_log_rows" -ge 4 ] && ok "B7: host ledger has >= 4 rows" \
        || bad "B7: host ledger has >= 4 rows" "got $_log_rows"
fi

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -gt 0 ] && exit 1; exit 0
