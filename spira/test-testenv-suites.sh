#!/usr/bin/env bash
# test-testenv-suites.sh — testenv-batch.sh: --suites explicit selection flag.
#
# WHAT THIS PROVES
#   A. Flag parsing: unknown suite exits non-zero and the error names the suite.
#      Positive control: a known suite is not rejected as "unknown suite".
#   B. Container integration: --suites with an explicit two-suite list runs
#      exactly those two suites; the subset marker ("subset") is the 6th field
#      in every result file; a positive control proves that the same invocation
#      WITHOUT --suites selects more suites.
#   C. Subset marker is absent from result files produced by a full (diff-derived)
#      run — the 6th field is not present.
#
# SEEN TO FAIL AGAINST UNFIXED TREE (law-a-regression-test-must-be-seen-to-fail):
#   Against original testenv-batch.sh (before --suites support):
#     bash testenv-batch.sh --suites test-fx-a.sh topic fixture 2>&1
#     → "batch: unknown option: --suites"
#     → rc=2
#   So:
#   A1 would fail: error says "unknown option" not "unknown suite: test-nonexist.sh"
#   A2 would fail: a known suite still exits 2 with "unknown option" (not a validation pass)
#   B1 would fail: batch exits 2 before container, no result files, no subset marker
#   B2/C would fail: cannot demonstrate the positive control
#
# host-reason: Part A tests pure flag/validation logic before the container starts.
#              Part B-C requires podman for container integration.
# covers: spira/testenv-batch.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
isexit2() { [ "$2" = 2 ]    && ok "$1" || bad "$1" "expected 2, got $2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
isfile()  { [ -f "$2" ] && ok "$1" || bad "$1" "file not found: $2"; }
nofile()  { [ ! -f "$2" ] && ok "$1" || bad "$1" "unexpected file: $2"; }

find_results_dir() {
    find "$1" -maxdepth 2 -name batch.meta 2>/dev/null | head -1 | xargs dirname 2>/dev/null || true
}

count_results() {  # count_results <dir>  → number of .result files
    find "$1" -maxdepth 1 -name '*.result' 2>/dev/null | wc -l
}

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-testenv-suites.sh"

# ===========================================================================
# FIXTURE REPO — master-based remote (same pattern as test-testenv-batch.sh).
# Three suites in SUITE_HOST so that diff-derived selects more than one.
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

# SUITE_HOST: three suites — s1 and s2 cover changed.sh; s3 has no covers (always runs).
# Without --suites on the topic branch: all three are selected.
# With --suites test-fx-s1.sh: only one is selected.
SUITE_HOST="$TMP/suites-host"
mkdir -p "$SUITE_HOST"

cat > "$SUITE_HOST/test-fx-s1.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-s1 ran\n'; exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-s1.sh"

cat > "$SUITE_HOST/test-fx-s2.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-s2 ran\n'; exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-s2.sh"

cat > "$SUITE_HOST/test-fx-s3.sh" << 'EOF'
#!/usr/bin/env bash
printf '  ok    test-fx-s3 ran (no covers)\n'; exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-s3.sh"

# ===========================================================================
# PART A: FLAG VALIDATION — no container required.
# The script validates --suites names BEFORE starting the container, so these
# tests exercise validation without needing podman.
# ===========================================================================
echo
echo "Part A: --suites flag validation (no container)"

# A1: unknown suite name exits non-zero and the error output names the suite.
# The error must say "test-nonexistent-xyz.sh", not just "unknown option".
_err_a1=""
rc_a1=0
_err_a1="$(SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
    bash "$BATCH" --suites "test-nonexistent-xyz.sh" topic "$FIXTURE" 2>&1 >/dev/null \
    || true)"
SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
    bash "$BATCH" --suites "test-nonexistent-xyz.sh" topic "$FIXTURE" >/dev/null 2>&1 \
    || rc_a1=$?
[ "$rc_a1" -ne 0 ] \
    && ok "A1: unknown suite exits non-zero (rc=$rc_a1)" \
    || bad "A1: unknown suite exits non-zero" "expected non-zero, got 0"
want "A1: error names the unknown suite" "test-nonexistent-xyz.sh" "$_err_a1"
notwant "A1: error is not the generic 'unknown option' message" \
    "unknown option:" "$_err_a1"

# A2: POSITIVE CONTROL — a known suite passes validation and does NOT produce an
# "unknown suite" error.  Without --suites support the script would say
# "unknown option: --suites" here; with it, it passes validation and fails later
# only for a different reason (no podman / container unavailable).
_err_a2=""
_err_a2="$(SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
    bash "$BATCH" --suites "test-fx-s1.sh" topic "$FIXTURE" 2>&1 >/dev/null \
    || true)"
notwant "A2: known suite is not reported as 'unknown suite' (positive control)" \
    "unknown suite:" "$_err_a2"
notwant "A2: known suite does not trigger 'unknown option' (positive control)" \
    "unknown option:" "$_err_a2"

# ===========================================================================
# PART B–C: CONTAINER INTEGRATION
# ===========================================================================
echo
echo "Part B-C: container integration"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-suites.sh Part B-C: podman not on PATH\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

PRE_CNAME="spira-suites-preflight-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-testenv-suites.sh Part B-C: container did not start\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP test-testenv-suites.sh Part B-C: user systemd not available\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
ok "B0: pre-flight: container + user systemd available"

# Copy suites into the fixture so podman can run them inside the container.
cp "$SUITE_HOST/test-fx-s1.sh" "$FIXTURE/spira/test-fx-s1.sh"
cp "$SUITE_HOST/test-fx-s2.sh" "$FIXTURE/spira/test-fx-s2.sh"
cp "$SUITE_HOST/test-fx-s3.sh" "$FIXTURE/spira/test-fx-s3.sh"

# ---------------------------------------------------------------------------
# B1: --suites with an explicit two-suite list runs exactly those two suites.
# Subset marker ("subset") is the 6th field in every result file.
# ---------------------------------------------------------------------------
echo
echo "B1: --suites explicit two-suite list"

RESULTS_ROOT_B1="$TMP/results-B1"
rc_b1=0
SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B1" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="bs1-$$" \
    bash "$BATCH" --suites "test-fx-s1.sh,test-fx-s2.sh" topic "$FIXTURE" || rc_b1=$?

iszero "B1: --suites run exits 0 (both selected suites pass)" "$rc_b1"

RD_B1="$(find_results_dir "$RESULTS_ROOT_B1")"
[ -n "$RD_B1" ] && ok "B1: results directory created" \
                 || bad "B1: results directory created" "not found under $RESULTS_ROOT_B1"

if [ -n "$RD_B1" ]; then
    # Exactly the two named suites must have result files.
    isfile  "B1: s1 has result file"             "$RD_B1/test-fx-s1.sh.result"
    isfile  "B1: s2 has result file"             "$RD_B1/test-fx-s2.sh.result"
    nofile  "B1: s3 has NO result file (not in --suites list)" \
            "$RD_B1/test-fx-s3.sh.result"

    # The 6th field of each result file must be "explicit" (producer=explicit,
    # matching test-testenv-batch.sh B5b and test-testenv-stdin.sh C).
    if [ -f "$RD_B1/test-fx-s1.sh.result" ]; then
        _marker_s1="$(awk '{print $6}' "$RD_B1/test-fx-s1.sh.result")"
        [ "$_marker_s1" = explicit ] \
            && ok "B1: s1 result 6th field is 'explicit'" \
            || bad "B1: s1 result 6th field is 'explicit'" "got '$_marker_s1'"
    fi
    if [ -f "$RD_B1/test-fx-s2.sh.result" ]; then
        _marker_s2="$(awk '{print $6}' "$RD_B1/test-fx-s2.sh.result")"
        [ "$_marker_s2" = explicit ] \
            && ok "B1: s2 result 6th field is 'explicit'" \
            || bad "B1: s2 result 6th field is 'explicit'" "got '$_marker_s2'"
    fi

    # batch.meta must record selection=explicit.
    isfile "B1: batch.meta written" "$RD_B1/batch.meta"
    if [ -f "$RD_B1/batch.meta" ]; then
        want "B1: batch.meta has selection=explicit" "selection=explicit" "$(cat "$RD_B1/batch.meta")"
    fi
fi

# ---------------------------------------------------------------------------
# B2: POSITIVE CONTROL — without --suites, the same fixture (three suites,
# topic branch with changed.sh) selects all three: s1 and s2 cover changed.sh,
# s3 has no covers (always runs).  The result count must exceed the two selected
# by --suites, proving that --suites actually restricts the selection.
# ---------------------------------------------------------------------------
echo
echo "B2: positive control — without --suites selects more than --suites"

RESULTS_ROOT_B2="$TMP/results-B2"
rc_b2=0
SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B2" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="bs2-$$" \
    bash "$BATCH" topic "$FIXTURE" || rc_b2=$?

iszero "B2: full (diff-derived) run exits 0" "$rc_b2"

RD_B2="$(find_results_dir "$RESULTS_ROOT_B2")"
if [ -n "$RD_B2" ]; then
    # All three suites must have run.
    isfile "B2: s1 has result file" "$RD_B2/test-fx-s1.sh.result"
    isfile "B2: s2 has result file" "$RD_B2/test-fx-s2.sh.result"
    isfile "B2: s3 has result file" "$RD_B2/test-fx-s3.sh.result"

    # Count must be > 2 (the --suites selection count).
    _n_b2="$(count_results "$RD_B2")"
    [ "$_n_b2" -gt 2 ] \
        && ok "B2: full run selects more than 2 suites (positive control, got $_n_b2)" \
        || bad "B2: full run selects more than 2 suites" \
               "expected >2, got $_n_b2 (positive control must show --suites restricts selection)"
fi

# ---------------------------------------------------------------------------
# C: DIFF-DERIVED RUN'S RESULT FILES HAVE PRODUCER=diff IN THE 6TH FIELD.
# ---------------------------------------------------------------------------
echo
echo "C: diff-derived run result files record producer=diff in 6th field"

if [ -n "${RD_B2:-}" ] && [ -f "$RD_B2/test-fx-s1.sh.result" ]; then
    _marker_full="$(awk '{print $6}' "$RD_B2/test-fx-s1.sh.result")"
    [ "$_marker_full" = diff ] \
        && ok "C: full-run result file has 6th field 'diff'" \
        || bad "C: full-run result file has 6th field 'diff'" \
               "got '$_marker_full'"

    # batch.meta must record selection=diff (matching test-testenv-batch.sh B5a).
    if [ -f "$RD_B2/batch.meta" ]; then
        want "C: full run batch.meta has selection=diff" \
            "selection=diff" "$(cat "$RD_B2/batch.meta")"
    fi
fi

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
