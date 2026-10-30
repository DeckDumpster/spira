#!/usr/bin/env bash
# tier: T1
# covers: spira/metrics/grafana/** pilgrimage/src/** tsd/src/pass.rs spira-otel/src/** heimdall/src/main.rs inbox-triage/src/main.rs store-persist/src/main.rs queue/src/telemetry.rs landing-pass/src/telemetry.rs rebase-stale/src/telemetry.rs round-vm/src/telemetry.rs sccache-dav/src/telemetry.rs release/src/telemetry.rs habits/src/lib.rs exec-census/src/** spira-lc/src/cycle.rs
#
# The provisioned Grafana dashboards parse, read their data source from a variable, carry no
# annotations or text panels, and every cert-pass series they query is one the producer pushes.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-grafana-dashboards.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"
DASH="$ROOT/spira/metrics/grafana/dashboards"

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

for want_f in rounds site-ci bead-store components gate-tooling runs queue-landing pilgrimages habits exec-census; do
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

pushed_otel="$(grep -ho '"spira_[a-z_]*"' "$ROOT/spira-otel/src/metrics.rs" "$ROOT/heimdall/src/main.rs" "$ROOT/inbox-triage/src/main.rs" "$ROOT/store-persist/src/main.rs" "$ROOT"/round-vm/src/telemetry.rs "$ROOT"/sccache-dav/src/telemetry.rs "$ROOT"/release/src/telemetry.rs | tr -d '"' | sort -u)"
queried_c="$(grep -o 'spira_[a-z_]*' "$DASH/components.json" | sort -u)"
missing_c="$(comm -13 <(printf '%s\n' "$pushed_otel") <(printf '%s\n' "$queried_c"))"
is "components.json queries only series the components push" "" "$missing_c"
want "positive control: the component series were found" "spira_component_invocations" "$pushed_otel"

run_pushed="$(grep -ohE '(RUNS|RUN_SECONDS|EVENTS): &str = "spira_component_[a-z_]*"' "$ROOT/spira-otel/src/run.rs" | grep -o 'spira_component_[a-z_]*' | sort -u)"
run_queried="$(grep -o 'spira_component_[a-z_]*' "$DASH/runs.json" | sed -E 's/_bucket$//' | sort -u)"
is "runs.json queries only series spira-otel's run module pushes" "" "$(comm -13 <(printf '%s\n' "$run_pushed") <(printf '%s\n' "$run_queried"))"
[ -n "$run_pushed" ] && ok "positive control: the run module's series were found" || bad "no run-module series found"

ec_pushed="$(grep -ohE 'pub const [A-Z]+: &str = "spira_exec_census_[a-z_]*"' "$ROOT/exec-census/src/lib.rs" | grep -o 'spira_exec_census_[a-z_]*' | sort -u)"
ec_queried="$(grep -o 'spira_exec_census_[a-z_]*' "$DASH/exec-census.json" | sort -u)"
is "exec-census.json queries only series exec-census pushes" "" "$(comm -13 <(printf '%s\n' "$ec_pushed") <(printf '%s\n' "$ec_queried"))"
[ -n "$ec_pushed" ] && ok "positive control: exec-census series were found" || bad "no exec-census series found"

ql="$(grep -o 'spira_[a-z_]*' "$DASH/queue-landing.json" | sed -E 's/_bucket$//' | sort -u)"
undeclared=""
for m in $ql; do grep -rqF "\"$m\"" "$ROOT"/{queue,landing-pass,rebase-stale}/src/telemetry.rs "$ROOT/spira-otel/src/metrics.rs" "$ROOT/spira-lc/src/cycle.rs" || undeclared="$undeclared $m"; done
is "queue-landing.json queries only series the components push" "" "$undeclared"
[ -n "$ql" ] && ok "positive control: queue-landing series were found" || bad "no queue-landing series found"

pg_pushed="$(grep -o '^pub const [A-Z_]*: &str = "spira_pilgrimage_[a-z_]*"' "$ROOT/pilgrimage/src/lib.rs" | grep -o 'spira_pilgrimage_[a-z_]*' | sort -u)"
pg_queried="$(grep -o 'spira_pilgrimage_[a-z_]*' "$DASH/pilgrimages.json" | sort -u)"
is "pilgrimages.json queries only series the pilgrimage crate pushes" "" "$(comm -13 <(printf '%s\n' "$pg_pushed") <(printf '%s\n' "$pg_queried"))"
[ -n "$pg_pushed" ] && ok "positive control: the pilgrimage series were found" || bad "no pilgrimage series found"
is "every series the pilgrimage crate pushes is on the panel" "" "$(comm -23 <(printf '%s\n' "$pg_pushed") <(printf '%s\n' "$pg_queried"))"
hq="$(grep -o 'habit_[a-z_]*' "$DASH/habits.json" | sort -u)"
hundeclared=""
for m in $hq; do grep -qF "\"$m\"" "$ROOT/habits/src/lib.rs" || hundeclared="$hundeclared $m"; done
is "habits.json queries only series the habits binary pushes" "" "$hundeclared"
[ -n "$hq" ] && ok "positive control: habit series were found" || bad "no habit series found"
for m in $(grep -o 'spira_[a-z_]*' "$DASH/habits.json" | sort -u); do
  grep -rqF "\"$m\"" "$ROOT"/tsd/src/pass.rs "$ROOT"/landing-pass/src/telemetry.rs || bad "habits.json queries $m, which nothing pushes"
done

mount_src="$(grep -o '"\$HERE/[^"]*:/var/lib/grafana-dashboards' "$HERE/metrics-stack.sh" | sed -E 's#^"\$HERE/##; s#:.*##')"
want "metrics-stack.sh mounts the dashboards directory" "metrics/grafana/dashboards" "$mount_src"
unmounted="$(cd "$ROOT" && find . -path ./target -prune -o -name '*.json' -path '*grafana*' -print | sed 's#^\./##' | grep -vF "spira/$mount_src/" | grep -v '/provisioning/')"
is "every grafana dashboard JSON sits under the mounted directory" "" "$unmounted"
prov_path="$(grep -h 'path:' "$HERE"/metrics/grafana/provisioning/dashboards/*.yaml | awk '{print $2}')"
want "the provisioning provider reads the mounted path" "/var/lib/grafana-dashboards" "$prov_path"
[ -z "$(find "$ROOT" -path "$ROOT/target" -prune -o -name 'provisioning-dashboards.yaml' -print)" ] && ok "no second provisioning file" || bad "dead provisioning file present"

tl_summary
