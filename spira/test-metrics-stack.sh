#!/usr/bin/env bash
#
# test-metrics-stack.sh — the metrics stack's wrapper, units and provisioned files: idle
# until a data directory is configured, Grafana refuses any address but a concrete one,
# every unit is CPU-fenced, and the dashboard queries only labels the collector emits.
#
# tier: T0
# covers: spira/metrics-stack.sh spira/metrics/* systemd/spira-metrics-*.service spira/conf.d/SPIRA_METRICS_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

printf 'test-metrics-stack.sh\n'
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/bin" "$T/run" "$T/data"
cat > "$T/bin/podman" <<STUB
#!/usr/bin/env bash
case "\$1 \$2" in "network exists") exit 0 ;; esac
printf '%s\n' "\$*" >> "$T/podman.log"
exit 0
STUB
chmod +x "$T/bin/podman"

run() { # part [KEY=VAL...] -> rc in \$rc, stderr in \$err
    local part="$1"; shift
    : > "$T/podman.log"
    err="$(env -i HOME="$T" PATH="$T/bin:/usr/bin:/bin" SPIRA_TOML="${SPIRA_TOML:-}" SPIRA_RUN="$T/run" SPIRA_METRICS_IDLE_ONCE=1 "$@" bash "$HERE/metrics-stack.sh" "$part" 2>&1 >/dev/null)"
    rc=$?
}

run vm
is "unset data dir: vm exits 0" 0 "$rc"
want "unset data dir: says it is idle" "stays idle" "$err"
is "unset data dir: starts no container" "" "$(cat "$T/podman.log")"

run vm SPIRA_METRICS_DATA="$T/data"
is "vm starts" 0 "$rc"
want "vm retains 30 days" "-retentionPeriod=30d" "$(cat "$T/podman.log")"
want "vm is published on loopback only" "127.0.0.1:8428:8428" "$(cat "$T/podman.log")"
want "container cgroup is split under the unit" "--cgroups=split" "$(cat "$T/podman.log")"

run collector SPIRA_METRICS_DATA="$T/data"
want "collector reads the span directory read-only" "$T/run/otel:/otel:ro" "$(cat "$T/podman.log")"

for bad_addr in "" "0.0.0.0:3000" "[::]:3000" "100.64.0.1" "100.64.0.1:x"; do
    run grafana SPIRA_METRICS_DATA="$T/data" SPIRA_METRICS_GRAFANA_ADDR="$bad_addr"
    is "grafana refuses '$bad_addr'" 2 "$rc"
    is "grafana '$bad_addr' starts no container" "" "$(cat "$T/podman.log")"
done

run grafana SPIRA_METRICS_DATA="$T/data" SPIRA_METRICS_GRAFANA_ADDR="100.64.0.1:3000"
is "grafana starts on a tailnet address" 0 "$rc"
want "grafana publishes only on that address" "-p 100.64.0.1:3000:3000" "$(cat "$T/podman.log")"
is "admin credential is 0600" 600 "$(stat -c %a "$T/data/grafana.credential")"
cred1="$(cat "$T/data/grafana.credential")"
run grafana SPIRA_METRICS_DATA="$T/data" SPIRA_METRICS_GRAFANA_ADDR="100.64.0.1:3000"
is "admin credential survives a restart" "$cred1" "$(cat "$T/data/grafana.credential")"
nowant "credential never on the command line" "$cred1" "$(cat "$T/podman.log")"

for u in vm collector grafana; do
    f="$ROOT/systemd/spira-metrics-$u.service"
    want "$u unit is CPU-fenced" "CPUQuota=" "$(cat "$f")"
    want "$u unit execs the wrapper" "metrics-stack.sh $u" "$(cat "$f")"
done

col="$(cat "$HERE/metrics/collector.yaml")"
want "collector reads span files" "otlpjsonfile" "$col"
want "collector derives spanmetrics" "spanmetrics" "$col"
want "collector pushes to VictoriaMetrics" "victoria-metrics:8428/api/v1/write" "$col"
want "collector keys spans by verb" "rpc.method" "$col"
want "collector keys spans by caller" "spira.caller" "$col"

dash="$HERE/metrics/grafana/dashboards/bead-store.json"
is "dashboard is valid JSON" 0 "$(python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$dash" >/dev/null 2>&1; echo $?)"
exprs="$(python3 -c 'import json,sys
for p in json.load(open(sys.argv[1]))["panels"]:
    for t in p["targets"]: print(t["expr"])' "$dash")"
want "dashboard graphs call rate" "spira_bead_store_calls" "$exprs"
want "dashboard graphs latency quantiles" "quantile_over_time(0.9" "$exprs"
want "dashboard groups by verb label" "by (verb)" "$exprs"
want "datasource uid matches the dashboard" "uid: spira-vm" "$(cat "$HERE/metrics/grafana/provisioning/datasources/victoriametrics.yaml")"

tl_summary
