#!/usr/bin/env bash
# tier: T1
# covers: spira/testlib.sh UC-testlib-01 UC-testlib-02 UC-testlib-03 UC-testlib-04 UC-testlib-05 UC-testlib-06 UC-testlib-07
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/testlib.sh"

TESTLIB="$HERE/testlib.sh"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

# _run <script-body-on-stdin> -> writes it to a scratch file (sourcing the real testlib.sh
# first) and runs it, leaving _RUN_OUT and _RUN_RC for the caller to assert on. This is a
# subprocess boundary on purpose: the object under test prints its own TAP stream and calls
# exit(), so testing it in-process would corrupt this suite's own TAP stream and exit code.
_run() {
    local f="$SCRATCH/case-$$-$RANDOM.sh"
    { printf '. "%s"\n' "$TESTLIB"; cat; } > "$f"
    _RUN_OUT="$(bash "$f" 2>&1)"; _RUN_RC=$?
}

echo "test-testlib.sh"

# --- ok(): a lone pass ------------------------------------------------------------
_run <<'EOF'
ok "case one"
tl_summary
EOF
want   "ok(): emits a TAP ok line numbered 1" "ok 1 - case one" "$_RUN_OUT"
want   "ok(): tl_summary reports the pass" "1 passed, 0 failed, 0 skipped" "$_RUN_OUT"
want   "ok(): tl_summary emits the ASSERTIONS trailer suites.sh greps for" "ASSERTIONS 1" "$_RUN_OUT"
wantrc "ok(): suite exits 0 when every case passes" 0 "$_RUN_RC"

# --- bad(): a lone failure, with and without a detail -----------------------------
# POSITIVE CONTROL: an offender must actually be reported before its absence means
# anything (law-absence-needs-a-positive-control applied to this library itself).
_run <<'EOF'
bad "case two" "wanted [x] got [y]"
tl_summary
EOF
want   "bad(): emits a TAP not-ok line" "not ok 1 - case two" "$_RUN_OUT"
want   "bad(): detail rides a # diagnostic line" "# wanted [x] got [y]" "$_RUN_OUT"
want   "bad(): tl_summary reports the failure" "0 passed, 1 failed, 0 skipped" "$_RUN_OUT"
wantrc "bad(): suite exits 1 when a case fails" 1 "$_RUN_RC"

_run <<'EOF'
bad "case three"
tl_summary
EOF
nowant "bad(): no detail means no diagnostic line printed" "# " "$_RUN_OUT"

# --- is(): exact-string equality, pass and fail -----------------------------------
_run <<'EOF'
is "equal" "same" "same"
tl_summary
EOF
want "is(): passes when the strings are equal" "ok 1 - equal" "$_RUN_OUT"

_run <<'EOF'
is "unequal" "foo" "foobar"
tl_summary
EOF
want "is(): fails on a prefix match — exact equality, not substring" "not ok 1 - unequal" "$_RUN_OUT"
want "is(): failure detail names both sides" "wanted [foo] got [foobar]" "$_RUN_OUT"

# --- want(): substring found and not found ----------------------------------------
_run <<'EOF'
want "found" "needle" "a needle in a haystack"
tl_summary
EOF
want   "want(): passes when the haystack contains the needle" "ok 1 - found" "$_RUN_OUT"
wantrc "want(): passing case exits 0" 0 "$_RUN_RC"

_run <<'EOF'
want "not found" "needle" "an empty field"
tl_summary
EOF
want   "want(): fails when the haystack lacks the needle" "not ok 1 - not found" "$_RUN_OUT"
want   "want(): failure detail names both sides" "wanted [needle] in [an empty field]" "$_RUN_OUT"
wantrc "want(): failing case exits 1" 1 "$_RUN_RC"

# --- nowant(): absence found and not found ----------------------------------------
_run <<'EOF'
nowant "absent" "FAIL" "everything is fine"
tl_summary
EOF
want   "nowant(): passes when the haystack lacks the needle" "ok 1 - absent" "$_RUN_OUT"

_run <<'EOF'
nowant "present" "FAIL" "FAIL: everything is broken"
tl_summary
EOF
want   "nowant(): fails when the haystack contains the needle" "not ok 1 - present" "$_RUN_OUT"
want   "nowant(): failure detail names both sides" "did not want [FAIL] in [FAIL: everything is broken]" "$_RUN_OUT"

# --- wantrc(): exit-code comparison ------------------------------------------------
_run <<'EOF'
wantrc "matches" 3 3
tl_summary
EOF
want "wantrc(): passes when codes match" "ok 1 - matches" "$_RUN_OUT"

_run <<'EOF'
wantrc "mismatches" 0 3
tl_summary
EOF
want "wantrc(): fails when codes differ" "not ok 1 - mismatches" "$_RUN_OUT"
want "wantrc(): failure detail names both codes" "wanted rc=0 got rc=3" "$_RUN_OUT"

# --- plan(): explicit vs trailing ---------------------------------------------------
_run <<'EOF'
plan 2
ok "a"
ok "b"
tl_summary
EOF
want "plan(): an explicit plan prints before the first case" "1..2
ok 1 - a" "$_RUN_OUT"

_run <<'EOF'
ok "a"
ok "b"
tl_summary
EOF
want "plan(): an omitted plan trails the last case instead" "ok 2 - b
1..2" "$_RUN_OUT"

# --- skip(): a skip is never a pass -------------------------------------------------
_run <<'EOF'
skip "no docker on this host"
EOF
want   "skip(): TAP skip-all form" "1..0 # SKIP no docker on this host" "$_RUN_OUT"
wantrc "skip(): exits 77, the automake-skip code — never 0" 77 "$_RUN_RC"
nowant "skip(): rc=77 is not rc=0 (the bug this library exists to end)" "$_RUN_RC" "0"

_run <<'EOF'
ok "already ran"
skip "changed its mind"
EOF
want   "skip(): after a case already ran, bails rather than hiding it" "Bail out!" "$_RUN_OUT"
wantrc "skip(): the bail-on-partial-skip path exits 2" 2 "$_RUN_RC"

# --- bail(): stops immediately, distinct from both pass and skip -------------------
_run <<'EOF'
bail "fixture did not come up"
EOF
want   "bail(): TAP Bail out! line" "Bail out! fixture did not come up" "$_RUN_OUT"
wantrc "bail(): exits 2" 2 "$_RUN_RC"

# --- JSONL: silent with no sink, one row per case with a sink ----------------------
_run <<'EOF'
ok "no sink configured"
tl_summary
EOF
want "jsonl: with SPIRA_TESTLIB_JSONL unset, TAP output is unaffected" "ok 1 - no sink configured" "$_RUN_OUT"

JSONL_OUT="$SCRATCH/results.jsonl"
: > "$JSONL_OUT"
f="$SCRATCH/jsonl-case.sh"
cat > "$f" <<EOF
# tier: T2
# covers: spira/testlib.sh UC-testlib-09
. "$TESTLIB"
ok "row one"
bad "row two" "detail here"
tl_summary
EOF
SPIRA_TESTLIB_JSONL="$JSONL_OUT" bash "$f" >/dev/null 2>&1 || true
JSONL_BODY="$(cat "$JSONL_OUT" 2>/dev/null)"
wantrc "jsonl: one row per case (2 cases, 2 lines)" 2 "$(printf '%s\n' "$JSONL_BODY" | grep -c .)"
want   "jsonl: a pass row carries status pass" '"case":"row one","status":"pass"' "$JSONL_BODY"
want   "jsonl: a fail row carries status fail" '"case":"row two","status":"fail"' "$JSONL_BODY"
want   "jsonl: a fail row carries its detail" '"detail":"detail here"' "$JSONL_BODY"
want   "jsonl: the suite name is the sourcing script's basename" '"suite":"jsonl-case.sh"' "$JSONL_BODY"
want   "jsonl: the tier header is read into every row" '"tier":"T2"' "$JSONL_BODY"
want   "jsonl: UC ids are parsed off the covers line by their UC- prefix" '"uc":["UC-testlib-09"]' "$JSONL_BODY"

if command -v python3 >/dev/null 2>&1; then
    _PYRC=0
    printf '%s\n' "$JSONL_BODY" | python3 -c "
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    json.loads(line)
" || _PYRC=1
    wantrc "jsonl: every row is valid JSON" 0 "$_PYRC"
fi

# --- case ids: the name argument is stable across the TAP and JSONL views ---------
JSONL_OUT2="$SCRATCH/results2.jsonl"
f2="$SCRATCH/caseid.sh"
cat > "$f2" <<EOF
. "$TESTLIB"
ok "the stable case id"
tl_summary
EOF
SPIRA_TESTLIB_JSONL="$JSONL_OUT2" bash "$f2" >/dev/null 2>&1 || true
want "case ids: the same string names the case in TAP and in JSONL" \
    '"case":"the stable case id"' "$(cat "$JSONL_OUT2" 2>/dev/null)"

tl_summary
