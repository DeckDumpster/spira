#!/usr/bin/env bash
#
# test-wide-event-store.sh — a log record sent to the collector with an arbitrary attribute is
# queryable in VictoriaLogs by that attribute, and the wide-event dashboard's queries parse.
# Runs the real images under podman on a throwaway network; skips where podman cannot.
#
# tier: T1
# covers: spira/metrics/** spira/metrics-stack.sh systemd/spira-metrics-*.service
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
printf 'test-wide-event-store.sh\n'

timeout 20 podman info >/dev/null 2>&1 || skip "podman unavailable"
. "$HERE/metrics/stack.env"

T="$(mktemp -d)"; id="wes$$"
cleanup() { podman rm -f "$id-vl" "$id-oc" >/dev/null 2>&1; podman network rm -f "$id" >/dev/null 2>&1; rm -rf "$T"; }
trap cleanup EXIT INT TERM
mkdir -p "$T/otel" "$T/state/logs"
podman network create "$id" >/dev/null || { bad "network create"; tl_summary; exit 1; }

# the collector resolves the store by its production name
podman run -d --rm --name "$id-vl" --network "$id" --network-alias victoria-logs -p 127.0.0.1::9428 "$VL_IMAGE" -retentionPeriod=3d >/dev/null
podman run -d --rm --name "$id-oc" --network "$id" -p 127.0.0.1::4318 --user 0:0 \
    -v "$HERE/metrics/collector.yaml:/etc/otelcol/config.yaml:ro" -v "$T/otel:/otel:ro" -v "$T/state:/state" \
    "$COLLECTOR_IMAGE" --config=/etc/otelcol/config.yaml >/dev/null
port() { podman port "$1" "$2" | head -1 | sed 's/.*://'; }
vl="http://127.0.0.1:$(port "$id-vl" 9428)"; oc="http://127.0.0.1:$(port "$id-oc" 4318)"

needle="needle-$$-$RANDOM"
rec='{"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"t"}}]},"scopeLogs":[{"logRecords":[{"timeUnixNano":"'"$(date +%s)"'000000000","body":{"stringValue":"dispatch"},"attributes":[{"key":"handler","value":{"stringValue":"h1"}},{"key":"duration_ms","value":{"doubleValue":12.5}},{"key":"zz.arbitrary","value":{"stringValue":"'"$needle"'"}}]}]}]}]}'
sent=""
for _ in $(seq 1 30); do
    [ "$(curl -s -o /dev/null -w '%{http_code}' -H 'Content-Type: application/json' --data "$rec" "$oc/v1/logs")" = 200 ] && { sent=1; break; }
    sleep 1
done
is "collector accepts an OTLP log record" 1 "$sent"

q() { curl -s "$vl/select/logsql/query" --data-urlencode "query=$1"; }
got=""
for _ in $(seq 1 30); do got="$(q "zz.arbitrary:$needle")"; [ -n "$got" ] && break; sleep 1; done
want "record is queryable by its arbitrary attribute" "\"zz.arbitrary\":\"$needle\"" "$got"
is "positive control: a value never sent finds nothing" "" "$(q 'zz.arbitrary:never-sent-value')"

n=0
while IFS= read -r expr; do
    n=$((n + 1))
    out="$(q "$expr")"
    nowant "dashboard query parses: $expr" "cannot parse" "$out"
    nowant "dashboard query is accepted: $expr" "error" "$out"
done < <(python3 -I -c 'import json,sys
for p in json.load(open(sys.argv[1]))["panels"]:
    for t in p["targets"]: print(t["expr"])' "$HERE/metrics/grafana/dashboards/wide-events.json")
[ "$n" -ge 3 ] && ok "positive control: the dashboard queries were found" || bad "found $n dashboard queries"
want "positive control: a malformed query is rejected" "cannot parse" "$(q 'stats by (' )"

tl_summary
