#!/usr/bin/env bash
# tier: T1
# covers: spira/metrics/grafana/** work/src/main.rs habits/src/lib.rs gate/src/telemetry.rs spira/testlib.sh
#
# The gate-tooling dashboard's check panels (the yield meter and the unit-tests check seconds)
# parse clean and query only series something pushes.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-grafana-dashboards.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"
DASH="$ROOT/spira/metrics/grafana/dashboards/gate-tooling.json"

# series <dashboard> <panel-id-list> -> the metric names those panels' expressions use
series() {
  python3 -I - "$1" "$2" <<'PY'
import json, re, sys
d = json.load(open(sys.argv[1]))
want = set(int(i) for i in sys.argv[2].split(","))
names = set()
for p in d["panels"]:
    if p["id"] in want:
        for t in p["targets"]:
            expr = re.sub(r'"[^"]*"', '', t["expr"])
            names |= set(re.findall(r'\b((?:check|unit_red|habit|spira_gate)_[a-z_]*)', expr))
print("\n".join(sorted(names)))
PY
}

# undeclared <names> <source-file>... -> the names no source file declares as a pushed series
undeclared() {
  local names="$1" m miss=""; shift
  for m in $names; do grep -qF "\"$m\"" "$@" || miss="$miss $m"; done
  printf '%s' "${miss# }"
}

SRC=("$ROOT/work/src/main.rs" "$ROOT/habits/src/lib.rs" "$ROOT/gate/src/telemetry.rs")
queried="$(series "$DASH" 9,10)"

want "positive control: the check panels' series were found" "check_seconds" "$queried"
want "the yield meter graphs unit reds caught at submit" "unit_red_at_submit" "$queried"
want "the yield meter graphs the round's unit reds" "habit_unit_tests_red_in_round" "$queried"
is "the check panels query only series something pushes" "" "$(undeclared "$queried" "${SRC[@]}")"

planted="$(mktemp)"
sed 's/check_seconds/check_secs_nobody_pushes/' "$DASH" > "$planted"
want "positive control: a series nothing pushes is found" "check_secs_nobody_pushes" "$(undeclared "$(series "$planted" 9,10)" "${SRC[@]}")"
rm -f "$planted"

is "gate-tooling.json carries no annotations" "0" "$(python3 -I -c 'import json,sys; print(len(json.load(open(sys.argv[1])).get("annotations",{}).get("list",[])))' "$DASH")"
tl_summary
