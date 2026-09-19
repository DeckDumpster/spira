#!/usr/bin/env bash
# trust-harvest.sh — turn a suite's captured output into one record per named assertion.
#
#   trust-harvest.sh <suite> <rc> <secs> <sha> <condition> < suite-output
#
# PROOF OF CONCEPT for sp-pmv67. Not wired into anything.
#
# The identity is <suite>::<label> with the label put through the same normalisation
# suites.sh fingerprint() applies, because a label that interpolates a mktemp path forks
# its identity on every run.
set -uo pipefail
SUITE="${1:?suite}"; RC="${2:?rc}"; SECS="${3:?secs}"; SHA="${4:-unknown}"; COND="${5:-default}"
LEDGER="${SPIRA_TRUST_LEDGER:?SPIRA_TRUST_LEDGER must name the ledger file}"

norm() {
    sed -e 's#/tmp/[A-Za-z0-9._-]*#/tmp/X#g' \
        -e 's#/[A-Za-z0-9._/-]*/sptest_[A-Za-z0-9_]*#/X#g' \
        -e 's/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9][.0-9]*Z\{0,1\}/TIMESTAMP/g' \
        -e 's/[0-9][0-9]:[0-9][0-9]:[0-9][0-9]/TIME/g' \
        -e 's/[0-9]\{3,\}/N/g'
}

now="$(date +%s)"
mkdir -p "$(dirname "$LEDGER")" 2>/dev/null || true
n=0
while IFS=$'\t' read -r verdict label; do
    [ -n "$label" ] || continue
    printf '{"t":%s,"suite":"%s","assert":"%s","v":"%s","rc":%s,"secs":%s,"sha":"%s","cond":"%s"}\n' \
        "$now" "$SUITE" "$(printf '%s' "$label" | sed 's/\\/\\\\/g; s/"/\\"/g')" \
        "$verdict" "$RC" "$SECS" "$SHA" "$COND" >> "$LEDGER"
    n=$(( n + 1 ))
done < <(sed -nE 's/^  ok +(— )?(.*)$/ok\t\2/p; s/^  FAIL +(— )?([^:]*):.*$/fail\t\2/p' | norm)

# ZERO ASSERTIONS IS NOT A PASS. A suite whose setup died emits no assertion lines, and an
# empty harvest is indistinguishable from a suite that has no assertions. Record the fact.
if [ "$n" -eq 0 ]; then
    printf '{"t":%s,"suite":"%s","assert":null,"v":"no-assertions","rc":%s,"secs":%s,"sha":"%s","cond":"%s"}\n' \
        "$now" "$SUITE" "$RC" "$SECS" "$SHA" "$COND" >> "$LEDGER"
fi
printf 'trust-harvest: %s: %d assertion record(s)\n' "$SUITE" "$n"
