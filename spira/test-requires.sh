#!/usr/bin/env bash
#
# test-requires.sh — # requires: declared preconditions become skips with a reason.
#
# WHAT THIS PROVES
#   A. Parser: suite_requires_of extracts tokens from a # requires: line.
#      No declaration returns empty.  Commas are treated as delimiters.
#
#   B. Runner integration (container): a suite declaring # requires: <token>
#      where <token> is not on the container's PATH is recorded as skip-req
#      with the token named in the fingerprint field — not as red, and not as
#      the exit-77 self-skip.
#
#   C. Positive control: a suite declaring # requires: bash (always met) runs
#      normally and records ok, not skip-req.  Without this, a requirements
#      check that always skips would look identical to one that works.
#
#   D. Distinction from exit-77 skip: a suite that self-skips (exit 77) records
#      status "skip"; a suite that is pre-empted for an unmet requirement records
#      status "skip-req".  The two are counted separately.
#
# SEEN TO FAIL AGAINST UNFIXED TREE (law-a-regression-test-must-be-seen-to-fail):
#   Against the tree before suite_requires_of was added and testenv-batch.sh was
#   patched:
#     Part A: "suite_requires_of is defined" — FAIL: function not found
#     Part B-D: fixture suite with # requires: spira-nonexistent-xyz ran normally
#               inside the container and exited 0; result status was "ok" not
#               "skip-req". Assertion "skip-req status" failed: got "ok".
#
# host-reason: Part A tests the parser on the host.
#              Part B-D requires podman for container integration.
# covers: spira/testenv-batch.sh spira/suite-covers.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()      { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
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

echo "test-requires.sh"

# ===========================================================================
# PART A: PARSER — no container required.
# ===========================================================================
echo
echo "Part A: suite_requires_of parser (no container)"

# shellcheck disable=SC1090
. "$HERE/suite-covers.sh"

if declare -f suite_requires_of >/dev/null 2>&1; then
    ok "A0: suite_requires_of is defined in suite-covers.sh"
else
    bad "A0: suite_requires_of is defined in suite-covers.sh" "function not found"
    printf 'SKIP test-requires.sh Part A: parser not implemented\n' >&2
    # Continue to Part B so its failures are also recorded.
fi

# A1: a suite with a # requires: line returns the tokens.
cat > "$TMP/fx-has-req.sh" <<'EOF'
#!/usr/bin/env bash
# requires: spira-nonexistent-xyz, bash
exit 0
EOF

if declare -f suite_requires_of >/dev/null 2>&1; then
    _reqs_a1="$(suite_requires_of "$TMP/fx-has-req.sh")"
    want "A1: # requires: returns the tokens" "spira-nonexistent-xyz" "$_reqs_a1"
    want "A1: both tokens returned"           "bash"                  "$_reqs_a1"
else
    bad "A1: # requires: returns the tokens"   "suite_requires_of not defined"
    bad "A1: both tokens returned"             "suite_requires_of not defined"
fi

# A2: a suite with no # requires: line returns empty.
cat > "$TMP/fx-no-req.sh" <<'EOF'
#!/usr/bin/env bash
# covers: some/file.sh
exit 0
EOF
if declare -f suite_requires_of >/dev/null 2>&1; then
    _reqs_a2="$(suite_requires_of "$TMP/fx-no-req.sh")"
    is "A2: no # requires: line returns empty" "" "$_reqs_a2"
else
    bad "A2: no # requires: line returns empty" "suite_requires_of not defined"
fi

# A3: commas are stripped — "claude, bd" yields two tokens, not one.
cat > "$TMP/fx-comma-req.sh" <<'EOF'
#!/usr/bin/env bash
# requires: claude, bd
exit 0
EOF
if declare -f suite_requires_of >/dev/null 2>&1; then
    _reqs_a3="$(suite_requires_of "$TMP/fx-comma-req.sh")"
    want "A3: comma-separated tokens parsed" "claude" "$_reqs_a3"
    want "A3: second comma-token present"    "bd"     "$_reqs_a3"
    notwant "A3: comma not present as literal char" "," "$_reqs_a3"
else
    bad "A3: comma-separated tokens parsed" "suite_requires_of not defined"
    bad "A3: second comma-token present"    "suite_requires_of not defined"
    bad "A3: comma not present as literal char" "suite_requires_of not defined"
fi

# ===========================================================================
# PART B-D: CONTAINER INTEGRATION
# ===========================================================================
echo
echo "Part B-D: container integration"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-requires.sh Part B-D: podman not on PATH\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

# Pre-flight: spin up a container to verify the image and user systemd are available.
PRE_CNAME="spira-req-preflight-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-requires.sh Part B-D: container did not start\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP test-requires.sh Part B-D: user systemd not available\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
ok "B0: pre-flight: container + user systemd available"

# ---------------------------------------------------------------------------
# FIXTURE REPO — same master-base pattern as test-testenv-batch.sh.
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# FIXTURE SUITES — on host for validation and in FIXTURE/spira for the container.
# ---------------------------------------------------------------------------
SUITE_HOST="$TMP/suites-host"
mkdir -p "$SUITE_HOST"

# test-fx-reqmiss.sh: declares a requirement that is NOT on the container PATH.
# Expected: skip-req with "spira-nonexistent-xyz" named.
cat > "$SUITE_HOST/test-fx-reqmiss.sh" <<'EOF'
#!/usr/bin/env bash
# requires: spira-nonexistent-xyz
# covers: changed.sh
printf '  ok    test-fx-reqmiss ran (should have been skip-req)\n'
exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-reqmiss.sh"

# test-fx-reqmet.sh: declares # requires: bash — bash is always on the container PATH.
# Expected: ok (runs normally, positive control that met requirements still run).
cat > "$SUITE_HOST/test-fx-reqmet.sh" <<'EOF'
#!/usr/bin/env bash
# requires: bash
# covers: changed.sh
printf '  ok    test-fx-reqmet ran (requirement bash is met)\n'
exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-reqmet.sh"

# test-fx-exit77.sh: self-skips with exit 77 — no # requires: declaration.
# Expected: status "skip" (exit-77), NOT "skip-req".
cat > "$SUITE_HOST/test-fx-exit77.sh" <<'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf 'SKIP test-fx-exit77.sh: self-skip for comparison\n' >&2
exit 77
EOF
chmod +x "$SUITE_HOST/test-fx-exit77.sh"

# Copy suites into the fixture repo so podman can run them inside the container.
cp "$SUITE_HOST/test-fx-reqmiss.sh" "$FIXTURE/spira/test-fx-reqmiss.sh"
cp "$SUITE_HOST/test-fx-reqmet.sh"  "$FIXTURE/spira/test-fx-reqmet.sh"
cp "$SUITE_HOST/test-fx-exit77.sh"  "$FIXTURE/spira/test-fx-exit77.sh"

# ---------------------------------------------------------------------------
# B: RUN — all three fixture suites via testenv-batch.sh.
# skip-req does NOT count as red; the batch should exit 0.
# ---------------------------------------------------------------------------
echo
echo "B: run fixture suites through testenv-batch.sh"

RESULTS_ROOT="$TMP/results"
rc_b=0
SPIRA_BATCH_SUITE_DIR="$SUITE_HOST" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="req-$$" \
    bash "$BATCH" --suites "test-fx-reqmiss.sh,test-fx-reqmet.sh,test-fx-exit77.sh" \
        topic "$FIXTURE" || rc_b=$?

is "B: batch exits 0 (skip-req does not count as red)" 0 "$rc_b"

RD="$(find_results_dir "$RESULTS_ROOT")"
[ -n "$RD" ] && ok "B: results directory created" \
             || bad "B: results directory created" "not found under $RESULTS_ROOT"

# ---------------------------------------------------------------------------
# C: RESULT STATUS ASSERTIONS.
# ---------------------------------------------------------------------------
echo
echo "C: result status checks"

if [ -n "$RD" ]; then
    # C1: test-fx-reqmiss.sh must be skip-req, not ok, not skip.
    isfile "C1: reqmiss has a result file" "$RD/test-fx-reqmiss.sh.result"
    if [ -f "$RD/test-fx-reqmiss.sh.result" ]; then
        _st_miss="$(awk '{print $1}' "$RD/test-fx-reqmiss.sh.result")"
        _fp_miss="$(awk '{print $4}' "$RD/test-fx-reqmiss.sh.result")"
        is   "C1: reqmiss status is skip-req"        "skip-req" "$_st_miss"
        want "C1: reqmiss fingerprint names the token" "spira-nonexistent-xyz" "$_fp_miss"
        notwant "C1: reqmiss status is not plain 'skip'" "skip " "$_st_miss "
    fi

    # C2: test-fx-reqmet.sh must be ok (bash IS on the container PATH).
    # This is the positive control: requirements that ARE met must not skip.
    isfile "C2: reqmet has a result file" "$RD/test-fx-reqmet.sh.result"
    if [ -f "$RD/test-fx-reqmet.sh.result" ]; then
        _st_met="$(awk '{print $1}' "$RD/test-fx-reqmet.sh.result")"
        is "C2: reqmet status is ok (positive control: met requirement runs)" "ok" "$_st_met"
    fi

    # C3: test-fx-exit77.sh must be the plain "skip" (exit 77), not "skip-req".
    isfile "C3: exit77 has a result file" "$RD/test-fx-exit77.sh.result"
    if [ -f "$RD/test-fx-exit77.sh.result" ]; then
        _st_77="$(awk '{print $1}' "$RD/test-fx-exit77.sh.result")"
        is "C3: exit-77 status is plain skip (not skip-req)" "skip" "$_st_77"
    fi
fi

# ---------------------------------------------------------------------------
# D: DISTINCTION — skip-req ≠ skip. Reading the first field of the result file
# is how callers count each kind; the two must be distinct strings.
# ---------------------------------------------------------------------------
echo
echo "D: skip-req is distinct from plain skip"

if [ -n "$RD" ] && \
   [ -f "$RD/test-fx-reqmiss.sh.result" ] && \
   [ -f "$RD/test-fx-exit77.sh.result" ]; then
    _d_req="$(awk '{print $1}' "$RD/test-fx-reqmiss.sh.result")"
    _d_77="$( awk '{print $1}' "$RD/test-fx-exit77.sh.result")"
    [ "$_d_req" != "$_d_77" ] \
        && ok "D: skip-req status '$_d_req' is distinct from plain skip status '$_d_77'" \
        || bad "D: skip-req and plain skip must be distinct" \
               "both were '$_d_req'"
fi

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
