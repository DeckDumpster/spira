#!/usr/bin/env bash
# tier: T1
# covers: spira/metrics/grafana/** spira-config/src/build_dir.rs work/src/main.rs habits/src/lib.rs gate/src/telemetry.rs queue/src/ops/round.rs spira/testlib.sh
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
            names |= set(re.findall(r'\b((?:check|unit_red|habit|spira_gate|build_dir)_[a-z_]*)', expr))
print("\n".join(sorted(names)))
PY
}

# undeclared <names> <source-file>... -> the names no source file declares as a pushed series
undeclared() {
  local names="$1" m miss=""; shift
  for m in $names; do grep -qF "\"$m\"" "$@" || miss="$miss $m"; done
  printf '%s' "${miss# }"
}

SRC=("$ROOT/spira-config/src/build_dir.rs" "$ROOT/work/src/main.rs" "$ROOT/queue/src/ops/round.rs" "$ROOT/gate/src/telemetry.rs")
queried="$(series "$DASH" 9,10)"

pool="$(series "$DASH" 11)"
for m in build_dir_pool_bytes build_dir_budget_bytes build_dir_base_bytes build_dir_evicted; do
  want "the build-directory panel graphs $m" "$m" "$pool"
done
is "the build-directory panel queries only series something pushes" "" "$(undeclared "$pool" "$ROOT/spira-config/src/build_dir.rs")"

want "positive control: the check panels' series were found" "check_seconds" "$queried"
want "the yield meter graphs unit reds caught at submit" "unit_red_at_submit" "$queried"
want "the yield meter graphs the round's unit reds" "habit_unit_tests_red_in_round" "$queried"
is "the check panels query only series something pushes" "" "$(undeclared "$queried" "${SRC[@]}")"

planted="$(mktemp)"
sed 's/check_seconds/check_secs_nobody_pushes/' "$DASH" > "$planted"
want "positive control: a series nothing pushes is found" "check_secs_nobody_pushes" "$(undeclared "$(series "$planted" 9,10)" "${SRC[@]}")"
rm -f "$planted"

is "gate-tooling.json carries no annotations" "0" "$(python3 -I -c 'import json,sys; print(len(json.load(open(sys.argv[1])).get("annotations",{}).get("list",[])))' "$DASH")"
ALERT="$ROOT/spira/metrics/grafana/provisioning/alerting/gate-recompile.yaml"
alert_series() { grep -o 'spira_gate_[a-z_]*' "$1" | sort -u; }
alerted="$(alert_series "$ALERT")"
want "positive control: the alert's series were found" "spira_gate_unit_test_recompiled_total" "$alerted"
want "the alert's summary names the repo and the recompile seconds" "1" "$(grep -c 'summary:.*\$labels.repo.*\$values.B.Value' "$ALERT")"
want "the alert names the recompile seconds" "spira_gate_unit_test_recompile_seconds" "$alerted"
rcv="$(sed -n 's/^ *receiver: //p' "$ALERT")"
CP="$ROOT/spira/metrics/grafana/provisioning/alerting/contact-points.yaml"
want "the alert's receiver is a provisioned contact point" "name: $rcv" "$(grep -x " *name: $rcv" "$CP" | sed 's/^ *//')"
want "positive control: an unprovisioned receiver is not found" "" "$(grep -x ' *name: no-such-receiver' "$CP")"
want "the alert's datasource uid is the provisioned one" "spira-vm" "$(grep -o 'datasourceUid: spira-vm' "$ALERT" | head -1 | cut -d' ' -f2)"
is "that uid is declared by the datasource" "uid: spira-vm" "$(grep -h "uid: spira-vm" "$ROOT/spira/metrics/grafana/provisioning/datasources/victoriametrics.yaml" | sed "s/^ *//")"
want "the alert pages by the repo label" "group_by: [repo]" "$(grep group_by "$ALERT" | sed 's/^ *//')"
want "the alert parses as YAML" "ok" "$(python3 -I -c 'import yaml,sys; yaml.safe_load(open(sys.argv[1])); print("ok")' "$ALERT" 2>&1 | tail -1)"
tl_summary
