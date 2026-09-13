#!/usr/bin/env bash
# test-testenv-stdin.sh — testenv-batch.sh: --suites - reads suite names from stdin.
#
# WHAT THIS PROVES
#   A. Empty stdin exits 0 and says "nothing to run" — not an error.
#   B. Unknown suite name on stdin exits non-zero and names the suite.
#   C. Container integration: piping two names via --suites - runs exactly those two;
#      the subset marker ("subset") is the 6th field in every result file.
#   D. Positive control: a different stdin list selects differently, proving stdin
#      drives selection rather than the diff or a fixed default.
#
# SEEN TO FAIL AGAINST UNFIXED TREE (law-a-regression-test-must-be-seen-to-fail):
#   Before stdin support, "--suites -" treated "-" as a suite name and ran it through
#   the comma-list parser. With SUITE_DIR="$TMP/suites", there is no file named "-",
#   so the script emitted:
#     batch: unknown suite: -
#   and exited 2.
#   Therefore:
#   A would fail: empty stdin still produces "unknown suite: -", not "nothing to run"
#   B would fail: the error names "-" not the suite name on stdin
#   C would fail: batch exits 2 before container, no result files produced
#   D would fail: cannot show differential selection if C fails
#
# host-reason: Part A-B tests pure validation logic before the container starts.
#              Part C-D requires podman for container integration.
# covers: spira/testenv-batch.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
isfile()  { [ -f "$2" ] && ok "$1" || bad "$1" "file not found: $2"; }
nofile()  { [ ! -f "$2" ] && ok "$1" || bad "$1" "unexpected file: $2"; }

find_results_dir() {
    find "$1" -maxdepth 2 -name batch.meta 2>/dev/null | head -1 | xargs dirname 2>/dev/null || true
}

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-testenv-stdin.sh"

# ===========================================================================
# FIXTURE REPO — master-based remote; same pattern as test-testenv-suites.sh.
# Three suites: s1 and s2 cover changed.sh; s3 has no covers (always runs).
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
# PART A: EMPTY STDIN — exits 0 saying "nothing to run".
# No container required; the empty-stdin exit is in the selection block, before
# the container starts.
# ===========================================================================
echo
echo "Part A: empty stdin exits 0"

rc_a=99
_out_a="$(printf '' | SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
    bash "$BATCH" --suites - topic "$FIXTURE" 2>&1)" || rc_a=$?
[ "$rc_a" = 99 ] && rc_a=0  # command succeeded without setting rc_a
iszero "A: empty stdin exits 0" "$rc_a"
want "A: says nothing to run" "nothing to run" "$_out_a"

# ===========================================================================
# PART B: UNKNOWN SUITE ON STDIN — exits non-zero and names the suite.
# ===========================================================================
echo
echo "Part B: unknown suite on stdin exits non-zero and names it"

rc_b=0
_err_b="$(printf 'test-nonexistent-xyz.sh\n' | SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
    bash "$BATCH" --suites - topic "$FIXTURE" 2>&1 >/dev/null)" || rc_b=$?
[ "$rc_b" -ne 0 ] \
    && ok "B: unknown suite on stdin exits non-zero (rc=$rc_b)" \
    || bad "B: unknown suite on stdin exits non-zero" "expected non-zero, got 0"
want "B: error names the suite" "test-nonexistent-xyz.sh" "$_err_b"
notwant "B: error is not 'unknown suite: -' (old literal-dash error)" \
    "unknown suite: -" "$_err_b"

# ===========================================================================
# PART C–D: CONTAINER INTEGRATION
# ===========================================================================
echo
echo "Part C-D: container integration"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-stdin.sh Part C-D: podman not on PATH\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

PRE_CNAME="spira-stdin-preflight-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-testenv-stdin.sh Part C-D: container did not start\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP test-testenv-stdin.sh Part C-D: user systemd not available\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
ok "C0: pre-flight: container + user systemd available"

# Copy suites into the fixture so podman can run them inside the container.
cp "$SUITE_HOST/test-fx-s1.sh" "$FIXTURE/spira/test-fx-s1.sh"
cp "$SUITE_HOST/test-fx-s2.sh" "$FIXTURE/spira/test-fx-s2.sh"
cp "$SUITE_HOST/test-fx-s3.sh" "$FIXTURE/spira/test-fx-s3.sh"

# ---------------------------------------------------------------------------
# C: Two names piped via --suites - run exactly those two suites.
# Subset marker ("subset") must be the 6th field in every result file.
# ---------------------------------------------------------------------------
echo
echo "C: two-suite list via stdin"

RESULTS_ROOT_C="$TMP/results-C"
rc_c=0
printf 'test-fx-s1.sh\ntest-fx-s2.sh\n' \
    | SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
      SPIRA_BATCH_RESULTS="$RESULTS_ROOT_C" \
      SPIRA_BATCH_SKIP_INSTALL=1 \
      SPIRA_BATCH_INSTANCE="bs-stdin-c-$$" \
      bash "$BATCH" --suites - topic "$FIXTURE" || rc_c=$?

iszero "C: --suites - run with two suites exits 0" "$rc_c"

RD_C="$(find_results_dir "$RESULTS_ROOT_C")"
[ -n "$RD_C" ] && ok "C: results directory created" \
               || bad "C: results directory created" "not found under $RESULTS_ROOT_C"

if [ -n "$RD_C" ]; then
    isfile "C: s1 has result file"  "$RD_C/test-fx-s1.sh.result"
    isfile "C: s2 has result file"  "$RD_C/test-fx-s2.sh.result"
    nofile "C: s3 has NO result file (not in stdin list)" "$RD_C/test-fx-s3.sh.result"

    if [ -f "$RD_C/test-fx-s1.sh.result" ]; then
        _marker_c1="$(awk '{print $6}' "$RD_C/test-fx-s1.sh.result")"
        [ "$_marker_c1" = explicit ] \
            && ok "C: s1 result 6th field is 'explicit'" \
            || bad "C: s1 result 6th field is 'explicit'" "got '$_marker_c1'"
    fi
    if [ -f "$RD_C/test-fx-s2.sh.result" ]; then
        _marker_c2="$(awk '{print $6}' "$RD_C/test-fx-s2.sh.result")"
        [ "$_marker_c2" = explicit ] \
            && ok "C: s2 result 6th field is 'explicit'" \
            || bad "C: s2 result 6th field is 'explicit'" "got '$_marker_c2'"
    fi

    isfile "C: batch.meta written" "$RD_C/batch.meta"
    if [ -f "$RD_C/batch.meta" ]; then
        want "C: batch.meta has selection=explicit" "selection=explicit" "$(cat "$RD_C/batch.meta")"
    fi
fi

# ---------------------------------------------------------------------------
# D: POSITIVE CONTROL — a different stdin list selects differently.
# Pipe only s2 this time; only s2 must have a result file.
# ---------------------------------------------------------------------------
echo
echo "D: positive control — different stdin list selects differently"

RESULTS_ROOT_D="$TMP/results-D"
rc_d=0
printf 'test-fx-s2.sh\n' \
    | SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
      SPIRA_BATCH_RESULTS="$RESULTS_ROOT_D" \
      SPIRA_BATCH_SKIP_INSTALL=1 \
      SPIRA_BATCH_INSTANCE="bs-stdin-d-$$" \
      bash "$BATCH" --suites - topic "$FIXTURE" || rc_d=$?

iszero "D: single-suite stdin run exits 0" "$rc_d"

RD_D="$(find_results_dir "$RESULTS_ROOT_D")"
if [ -n "$RD_D" ]; then
    nofile "D: s1 has NO result file (not in stdin list — positive control)" \
           "$RD_D/test-fx-s1.sh.result"
    isfile "D: s2 has result file (was in stdin list)" "$RD_D/test-fx-s2.sh.result"
    nofile "D: s3 has NO result file (not in stdin list)" "$RD_D/test-fx-s3.sh.result"
fi

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
