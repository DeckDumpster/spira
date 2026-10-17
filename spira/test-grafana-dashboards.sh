#!/usr/bin/env bash
# tier: T1
# covers: grafana/** tsd/src/pass.rs spira-otel/src/**
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

for want_f in rounds site-ci bead-store; do
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

tl_summary
