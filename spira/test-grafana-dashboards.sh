#!/usr/bin/env bash
# tier: T1
# covers: grafana/** tsd/src/pass.rs spira-otel/src/** auron/src/main.rs inbox-triage/src/main.rs store-persist/src/main.rs
#
# The provisioned Grafana dashboards parse, read their data source from a variable, carry no
# annotations or text panels, and every cert-pass series they query is one the producer pushes.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-grafana-dashboards.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"
DASH="$ROOT/grafana/dashboards"

check() {   # check <file> -> prints problems, one per line
  python3 -I - "$1" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
bad = []
if d.get("annotations", {}).get("list"): bad.append("annotations")
for p in d["panels"]:
    if p["type"] in ("text", "news"): bad.append("text panel " + p["title"])
    if p["datasource"]["uid"] != "${DS_VM}": bad.append("hardcoded datasource " + p["title"])
    if not p["targets"]: bad.append("no targets " + p["title"])
if not d.get("uid"): bad.append("uid")
print("\n".join(bad))
PY
}

for want_f in rounds site-ci bead-store components gate-tooling runs; do
  f="$DASH/$want_f.json"
  is "$want_f.json is valid and clean" "" "$(check "$f" 2>&1)"
done

_planted="$(mktemp)"
printf '{"uid":"x","annotations":{"list":[{"name":"opinion"}]},"panels":[{"type":"text","title":"t","datasource":{"uid":"abc"},"targets":[]}]}' > "$_planted"
want "positive control: the check finds a planted offender" "annotations" "$(check "$_planted" 2>&1)"
rm -f "$_planted"

pushed="$(grep -o '"spira_cert_pass_[a-z_]*"' "$ROOT/tsd/src/pass.rs" | tr -d '"' | sort -u)"
queried="$(grep -o 'spira_cert_pass_[a-z_]*' "$DASH/rounds.json" | sed -E 's/_bucket$//' | sort -u)"
missing="$(comm -13 <(printf '%s\n' "$pushed") <(printf '%s\n' "$queried"))"
is "rounds.json queries only series cert-pass pushes" "" "$missing"
[ -n "$pushed" ] && ok "positive control: the producer's series were found" || bad "no pushed series found"

pushed_otel="$(grep -ho '"spira_[a-z_]*"' "$ROOT/spira-otel/src/metrics.rs" "$ROOT/auron/src/main.rs" "$ROOT/inbox-triage/src/main.rs" "$ROOT/store-persist/src/main.rs" | tr -d '"' | sort -u)"
queried_c="$(grep -o 'spira_[a-z_]*' "$DASH/components.json" | sort -u)"
missing_c="$(comm -13 <(printf '%s\n' "$pushed_otel") <(printf '%s\n' "$queried_c"))"
is "components.json queries only series the components push" "" "$missing_c"
want "positive control: the component series were found" "spira_component_invocations" "$pushed_otel"

run_pushed="$(grep -ohE '(RUNS|RUN_SECONDS|EVENTS): &str = "spira_component_[a-z_]*"' "$ROOT/spira-otel/src/run.rs" | grep -o 'spira_component_[a-z_]*' | sort -u)"
run_queried="$(grep -o 'spira_component_[a-z_]*' "$DASH/runs.json" | sed -E 's/_bucket$//' | sort -u)"
is "runs.json queries only series spira-otel's run module pushes" "" "$(comm -13 <(printf '%s\n' "$run_pushed") <(printf '%s\n' "$run_queried"))"
[ -n "$run_pushed" ] && ok "positive control: the run module's series were found" || bad "no run-module series found"

tl_summary
