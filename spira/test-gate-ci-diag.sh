#!/usr/bin/env bash
# test-gate-ci-diag.sh — gate-diag.sh emits FAIL lines, annotations, and summary.
# tier: T1
# covers: spira/gate-diag.sh .github/workflows/gate.yml spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
has()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2]"; esac; }
lacks() { case "$3" in *"$2"*) bad "$1" "must not contain [$2]" ;; *) ok "$1"; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
DIAG="$HERE/gate-diag.sh"

# set up a result dir: setup <dir> <suite>=<status>[/<rc>] ...
setup() {
    local d="$1"; shift; rm -rf "$d"; mkdir -p "$d/k"
    for kv in "$@"; do
        local suite="${kv%%=*}" rest="${kv#*=}"
        local status="${rest%%/*}" rc="${rest#*/}"
        [ "$rc" = "$rest" ] && rc="-"
        printf '%s 1741000000 42 fp parallel explicit %s\n' "$status" "$rc" > "$d/k/$suite.result"
        case "$status" in
            red)
                printf 'starting %s\nok 1 - setup\n  FAIL  assert_name: want [foo] got [bar]\nnot ok 2 - the check\n' \
                    "$suite" > "$d/k/$suite.out" ;;
            timeout)
                printf 'FAIL: %s was killed at 600s\n' "$suite" > "$d/k/$suite.out" ;;
            *)
                printf 'ok 1 - all good\n' > "$d/k/$suite.out" ;;
        esac
    done
}

echo "test-gate-ci-diag.sh"

# POSITIVE CONTROL: the script must exist before any other check is meaningful.
if [ ! -r "$DIAG" ]; then
    bad "gate-diag.sh exists at spira/gate-diag.sh" "not found"
    tl_summary; exit
fi
ok "gate-diag.sh exists at spira/gate-diag.sh"

echo
echo "1. red suite: FAIL line and summary row appear:"
setup "$TMP/r" a.sh=red/1 b.sh=ok/0
out="$(bash "$DIAG" "$TMP/r" 2>&1)"
has "FAIL line in output"            "FAIL"                       "$out"
has "--- last N lines --- separator" "--- last"                   "$out"
has "summary table header"           "| Suite"                    "$out"
has "red suite named in summary"     "a.sh"                       "$out"
lacks "green suite not in summary"   "b.sh"                       "$out"

echo
echo "2. CI mode: ::group:: and ::error annotation emitted:"
out="$(GITHUB_ACTIONS=true bash "$DIAG" "$TMP/r" 2>&1)"
has "::group:: for red suite"         "::group::a.sh"             "$out"
has "::endgroup::"                    "::endgroup::"              "$out"
has "::error annotation"              "::error file=spira/a.sh::" "$out"
has "annotation carries FAIL line"   "FAIL"                       "$out"
lacks "no annotation for green suite" "::error file=spira/b.sh"   "$out"

echo
echo "3. suite with no output shows (no output):"
setup "$TMP/r2" c.sh=red/1
: > "$TMP/r2/k/c.sh.out"
out="$(bash "$DIAG" "$TMP/r2" 2>&1)"
has "no-output message"              "(no output"                 "$out"
has "suite still in summary"         "c.sh"                       "$out"

echo
echo "4. green batch: no summary table, no annotation:"
setup "$TMP/green" a.sh=ok/0 b.sh=ok/0
out="$(bash "$DIAG" "$TMP/green" 2>&1)"
lacks "no summary table for green"   "| Suite"                    "$out"
rc=0; bash "$DIAG" "$TMP/green" >/dev/null 2>&1 || rc=$?
is "green batch exits 0"             "0" "$rc"

echo
echo "5. retry: red-green (flake) vs red-red classification:"
setup "$TMP/retry" x.sh=red/1 y.sh=red/1
mkdir -p "$TMP/retry-retry/rkey"
printf 'ok 1741000001 1 - serial explicit 0\n' > "$TMP/retry-retry/rkey/x.sh.result"
printf 'red 1741000001 1 fp2 serial explicit 1\n' > "$TMP/retry-retry/rkey/y.sh.result"
out="$(bash "$DIAG" "$TMP/retry" 2>&1)"
_xrow="$(printf '%s\n' "$out" | grep 'x\.sh')"
_yrow="$(printf '%s\n' "$out" | grep 'y\.sh')"
has "x.sh row shows flake"            "red-green (flake)"          "$_xrow"
has "y.sh row shows red-red"          "red-red"                    "$_yrow"

echo
echo "6. GITHUB_STEP_SUMMARY receives the markdown table:"
setup "$TMP/sum" a.sh=red/1
sfile="$TMP/summary.md"
GITHUB_STEP_SUMMARY="$sfile" bash "$DIAG" "$TMP/sum" >/dev/null 2>&1
if [ -f "$sfile" ]; then
    ok "GITHUB_STEP_SUMMARY written"
    has "markdown heading"             "## Red suite summary"       "$(cat "$sfile")"
    has "table row in markdown"        "a.sh"                       "$(cat "$sfile")"
else
    bad "GITHUB_STEP_SUMMARY written" "file not created: $sfile"
fi

echo
echo "7. rc field appears in summary:"
setup "$TMP/rc" d.sh=red/5
out="$(bash "$DIAG" "$TMP/rc" 2>&1)"
has "rc shown in summary"            "rc=5"                        "$out"

echo
echo "8. timeout suite appears in diagnostics:"
setup "$TMP/to" e.sh=timeout/124
out="$(bash "$DIAG" "$TMP/to" 2>&1)"
has "timeout suite in summary"       "e.sh"                        "$out"
has "FAIL from timeout output"       "FAIL"                        "$out"

echo
echo "9. gate.yml has artifact upload and diagnostics steps:"
GATE_YML="$HERE/../.github/workflows/gate.yml"
if [ ! -r "$GATE_YML" ]; then
    bad "gate.yml readable" "not found at .github/workflows/gate.yml"
else
    G="$(cat "$GATE_YML")"
    has "upload-artifact step present"     "upload-artifact"            "$G"
    has "artifact upload runs on always()" "always()"                   "$G"
    has "gate-diag.sh called in workflow"  "gate-diag.sh"               "$G"
    has "batch results directory uploaded" "batch"                      "$G"
fi

echo
tl_summary
