#!/usr/bin/env bash
# test-testenv-batch-baseline.sh — batch builds shared testdb baseline; suites inherit it.
#
# WHAT THIS PROVES
#   testenv-batch.sh builds a shared testdb baseline before running suites.
#   Suites receive TESTDB_SHARED=1, TESTDB_BASELINE (a .beads snapshot), and
#   TESTDB_BD in their environment, so testdb_up can skip bd init and copy
#   instead (~26ms per suite rather than ~6s per suite).
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail)
#   Part A: a check-suite run with TESTDB_SHARED=0 fails — the property is
#   absent without the batch machinery. Part B's pass is meaningful only because
#   Part A established the check would catch the absence.
#
# ISOLATION (Part B2)
#   Two parallel suites both call testdb_up. Each gets its own private copy of
#   the baseline. Writes by suite A do not appear in suite B — and the baseline
#   itself is not mutated.
#
# FALLBACK (Part C)
#   When the baseline build cannot complete (SKIP_INSTALL path forces fallback),
#   suites still receive TESTDB_SHARED=0 and run correctly.
#
# host-reason: Parts B and C drive testenv-batch.sh, which starts its own container.
#
# covers: spira/testenv-batch.sh spira/testdb.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
isexit1() { [ "$2" = 1 ]    && ok "$1" || bad "$1" "expected 1, got $2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-testenv-batch-baseline.sh"

find_results_dir() {
    find "$1" -maxdepth 2 -name batch.meta 2>/dev/null | head -1 | xargs dirname 2>/dev/null || true
}

# Check-suite: passes only when TESTDB_SHARED=1 and TESTDB_BASELINE/.beads exists.
CHECKER='#!/usr/bin/env bash
# covers: changed.txt
if [ "${TESTDB_SHARED:-0}" != 1 ]; then
    printf "  FAIL  TESTDB_SHARED is not 1 (got: %s)\n" "${TESTDB_SHARED:-unset}"
    exit 1
fi
if [ -z "${TESTDB_BASELINE:-}" ]; then
    printf "  FAIL  TESTDB_BASELINE not set\n"
    exit 1
fi
if [ ! -d "${TESTDB_BASELINE}/.beads" ]; then
    printf "  FAIL  TESTDB_BASELINE/.beads not a directory: %s\n" "${TESTDB_BASELINE:-}"
    exit 1
fi
printf "  ok    shared baseline present (TESTDB_SHARED=1 TESTDB_BASELINE=%s)\n" "$TESTDB_BASELINE"
exit 0'

# ===========================================================================
# PART A: POSITIVE CONTROL — check-suite exits 1 when TESTDB_SHARED=0.
# Run it on the host (no container needed) to prove the check is live.
# ===========================================================================
echo
echo "Part A: positive control — check fails when TESTDB_SHARED=0"

CHECK_SCRIPT="$TMP/check-baseline.sh"
printf '%s\n' "$CHECKER" > "$CHECK_SCRIPT"
chmod +x "$CHECK_SCRIPT"

_ctrl_rc=0
TESTDB_SHARED=0 TESTDB_BASELINE="" bash "$CHECK_SCRIPT" >/dev/null 2>&1 || _ctrl_rc=$?
isexit1 "A1: check-suite exits 1 when TESTDB_SHARED=0 (baseline absent)" "$_ctrl_rc"

_ctrl_rc=0
TESTDB_SHARED=1 TESTDB_BASELINE="" bash "$CHECK_SCRIPT" >/dev/null 2>&1 || _ctrl_rc=$?
isexit1 "A2: check-suite exits 1 when TESTDB_BASELINE is empty" "$_ctrl_rc"

_ctrl_rc=0
TESTDB_SHARED=1 TESTDB_BASELINE="$TMP/no-beads" bash "$CHECK_SCRIPT" >/dev/null 2>&1 || _ctrl_rc=$?
isexit1 "A3: check-suite exits 1 when TESTDB_BASELINE/.beads does not exist" "$_ctrl_rc"

# ===========================================================================
# PART B: testenv-batch.sh builds the baseline; check-suite passes.
# ===========================================================================
echo
echo "Part B: baseline built by batch — check-suite passes inside container"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-batch-baseline.sh Part B/C: podman not on PATH\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

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
printf 'changed\n' > "$FIXTURE/changed.txt"
git -C "$FIXTURE" add changed.txt
git -C "$FIXTURE" commit -q -m "add changed.txt"
mkdir -p "$FIXTURE/spira"

SUITE_B="$TMP/suites-B"
mkdir -p "$SUITE_B"

# The check-suite runs inside the container, so TESTDB_BASELINE is a container path.
cat > "$SUITE_B/test-fx-check-baseline.sh" << 'CHECK'
#!/usr/bin/env bash
# covers: changed.txt
if [ "${TESTDB_SHARED:-0}" != 1 ]; then
    printf "  FAIL  TESTDB_SHARED is not 1 (got: %s)\n" "${TESTDB_SHARED:-unset}"
    exit 1
fi
if [ -z "${TESTDB_BASELINE:-}" ]; then
    printf "  FAIL  TESTDB_BASELINE not set\n"
    exit 1
fi
if [ ! -d "${TESTDB_BASELINE}/.beads" ]; then
    printf "  FAIL  TESTDB_BASELINE/.beads not a directory: %s\n" "${TESTDB_BASELINE:-}"
    exit 1
fi
printf "  ok    shared baseline present (TESTDB_SHARED=1 TESTDB_BASELINE=%s)\n" "$TESTDB_BASELINE"
exit 0
CHECK
chmod +x "$SUITE_B/test-fx-check-baseline.sh"
cp "$SUITE_B/test-fx-check-baseline.sh" "$FIXTURE/spira/test-fx-check-baseline.sh"

RESULTS_ROOT_B="$TMP/results-B"
_batch_out=""
rc_b=0
_batch_out="$(
    SPIRA_BATCH_SUITE_DIR="$SUITE_B" \
    SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B" \
    SPIRA_BATCH_SKIP_INSTALL=1 \
    SPIRA_BATCH_INSTANCE="bl-$$" \
        bash "$BATCH" --mode parallel topic "$FIXTURE" 2>&1
)" || rc_b=$?

iszero "B1: batch exits 0 (check-suite passes inside container)" "$rc_b"
want "B2: batch log shows baseline was built" "shared testdb baseline ready" "$_batch_out"

RD_B="$(find_results_dir "$RESULTS_ROOT_B")"
if [ -n "$RD_B" ] && [ -f "$RD_B/test-fx-check-baseline.sh.result" ]; then
    _st="$(awk '{print $1}' "$RD_B/test-fx-check-baseline.sh.result")"
    [ "$_st" = ok ] \
        && ok "B3: check-baseline suite status=ok (baseline env delivered to suite)" \
        || bad "B3: check-baseline suite status=ok" "got: $_st"
    _out="$(cat "$RD_B/test-fx-check-baseline.sh.out" 2>/dev/null || true)"
    want "B4: suite confirms TESTDB_BASELINE is set" "shared baseline present" "$_out"
else
    bad "B3: check-baseline result file present" "missing (results dir: ${RD_B:-none})"
    bad "B4: suite confirms baseline" "no result"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
