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

run logs SPIRA_METRICS_DATA="$T/data" SPIRA_OTEL_KEEP_DAYS=9
is "logs starts" 0 "$rc"
want "logs retention is the configured keep days" "-retentionPeriod=9d" "$(cat "$T/podman.log")"
nowant "logs retention is not the shipped default" "-retentionPeriod=14d" "$(cat "$T/podman.log")"
want "logs is published on loopback only" "127.0.0.1:9428:9428" "$(cat "$T/podman.log")"
for bad_keep in 0 x -3; do
    run logs SPIRA_METRICS_DATA="$T/data" SPIRA_OTEL_KEEP_DAYS="$bad_keep"
    is "logs refuses keep days '$bad_keep'" 2 "$rc"
    is "logs '$bad_keep' starts no container" "" "$(cat "$T/podman.log")"
done
nowant "no retention literal in the logs branch" "retentionPeriod=14" "$(cat "$HERE/metrics-stack.sh")"

run collector SPIRA_METRICS_DATA="$T/data"
want "collector accepts OTLP on loopback only" "127.0.0.1:4318:4318" "$(cat "$T/podman.log")"
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

for u in vm logs collector grafana; do
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

want "collector routes logs to VictoriaLogs" "victoria-logs:9428/insert/opentelemetry/v1/logs" "$col"
want "collector has a logs pipeline" "logs:" "$col"
run grafana SPIRA_METRICS_DATA="$T/data" SPIRA_METRICS_GRAFANA_ADDR="100.64.0.1:3000"
want "grafana installs the VictoriaLogs datasource plugin" "GF_INSTALL_PLUGINS=victoriametrics-logs-datasource " "$(cat "$T/podman.log")"
plugin_re='^[a-z0-9-]+( [0-9.]+)?(,[a-z0-9-]+( [0-9.]+)?)*$'
plugin_ok() { [[ "$1" =~ $plugin_re ]] && echo ok || echo bad; }
is "plugin matcher rejects id@version" bad "$(plugin_ok victoriametrics-logs-datasource@0.33.1)"
is "plugin matcher accepts id version" ok "$(plugin_ok 'victoriametrics-logs-datasource 0.33.1')"
rendered="$(grep -o "GF_INSTALL_PLUGINS=.*" "$T/podman.log" | sed "s/^GF_INSTALL_PLUGINS=//; s/ -[a-z-]* .*//")"
is "grafana GF_INSTALL_PLUGINS is in Grafana 11.2 form" ok "$(plugin_ok "$rendered")"
vl_ds="$(cat "$HERE/metrics/grafana/provisioning/datasources/victorialogs.yaml")"
want "VictoriaLogs datasource is provisioned" "type: victoriametrics-logs-datasource" "$vl_ds"
want "VictoriaLogs datasource points at the store" "url: http://victoria-logs:9428" "$vl_ds"

wide="$HERE/metrics/grafana/dashboards/wide-events.json"
wexprs="$(python3 -I -c 'import json,sys
d=json.load(open(sys.argv[1]))
assert d["uid"]=="spira-wide-events"
for p in d["panels"]:
    assert p["datasource"]["uid"]=="${DS_VL}", p["title"]
    for t in p["targets"]: print(t["expr"])' "$wide" 2>&1)"
want "wide-event dashboard counts dispatches per handler" "stats by (handler) count()" "$wexprs"
want "wide-event dashboard graphs p99 duration" "quantile(0.99, duration_ms)" "$wexprs"
want "wide-event dashboard counts quarantines" '_msg:"quarantine"' "$wexprs"
nowant "wide-event dashboard queries no PromQL" "rate(" "$wexprs"

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
