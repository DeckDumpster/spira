#!/usr/bin/env bash
#
# test-ask-help.sh — ask.sh --help prints usage and creates nothing; flag-like titles refused.
#
#   ./test-ask-help.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. `ask.sh insight --help` treated --help as the insight's
# title and created a bead titled '--help', which was immediately closed (because insights are
# created closed) and appeared in the operator's dismissable list as a record titled "--help".
# The same defect on `add` and `decide` is worse: those verbs create OPEN beads labelled
# needs-ryan, so a mistyped help flag placed a live question in front of the operator.
#
# A check that verifies only the exit code would pass while the bead is still made — which is
# exactly what happened before this suite. The count before and after is the discriminating
# fact (law-absence-needs-a-positive-control).
#
# defect: sp-7r21
# covers: cockpit/verify-asks.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
. "$HERE/testdb.sh"
testdb_require test-ask-help
testdb_up ask-help || { echo "testdb_up failed"; exit 1; }

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

export COCKPIT_DB="$SPIRA_DB"
BD="${TESTDB_BD:-bd}"

# Count beads currently in the fixture database. Used to prove no bead was created.
count_beads() { "$BD" -C "$SPIRA_DB" list --all --limit 0 --json 2>/dev/null \
                    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d if isinstance(d,list) else d.get("issues",[])))' 2>/dev/null || echo 0; }

ask() { COCKPIT_DB="$SPIRA_DB" bash "$COCKPIT/ask.sh" "$@"; }

echo "test-ask-help.sh"

# ======================================================================================
echo
echo "positive control — the positive control can detect a bead creation"
# ======================================================================================
# Plant one bead and confirm the count moves, so a frozen count is not masking a broken counter.
before="$(count_beads)"
ask insight "a real insight for the positive control" >/dev/null 2>&1 || true
after="$(count_beads)"
[ "$after" -gt "$before" ] \
    && ok "count_beads detects a real creation (positive control)" \
    || bad "count_beads detects a real creation (positive control)" "count before=$before after=$after"

testdb_reset

# ======================================================================================
echo
echo "help flags print usage and create nothing"
# ======================================================================================

for invocation in \
    "--help" \
    "insight --help" \
    "add -h" \
    "decide --help" \
    "note -h"
do
    # shellcheck disable=SC2086
    before="$(count_beads)"
    out="$(ask $invocation 2>&1)"; rc=$?
    after="$(count_beads)"
    is  "exit 0: ask.sh $invocation"          "0"        "$rc"
    want "prints usage: ask.sh $invocation"   "ask.sh"   "$out"
    is  "no bead created: ask.sh $invocation" "$before"  "$after"
done

# ======================================================================================
echo
echo "bare ask.sh prints usage and creates nothing"
# ======================================================================================
before="$(count_beads)"
out="$(ask 2>&1)"; rc=$?
after="$(count_beads)"
is   "exit 0: bare ask.sh"        "0"       "$rc"
want "prints usage: bare ask.sh"  "ask.sh"  "$out"
is   "no bead created: bare ask.sh" "$before" "$after"

# ======================================================================================
echo
echo "a title beginning with - is refused before any bead is created"
# ======================================================================================
for invocation in \
    "add --flag-as-title" \
    "decide -x" \
    "insight --not-a-flag"
do
    # shellcheck disable=SC2086
    before="$(count_beads)"
    out="$(ask $invocation 2>&1)"; rc=$?
    after="$(count_beads)"
    [ "$rc" -ne 0 ] \
        && ok  "non-zero exit: ask.sh $invocation" \
        || bad "non-zero exit: ask.sh $invocation" "exit was 0"
    want "names the bad token: ask.sh $invocation" "${invocation##* }" "$out"
    is   "no bead created: ask.sh $invocation" "$before" "$after"
done

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
