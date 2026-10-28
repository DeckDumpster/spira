#!/usr/bin/env bash
#
# metrics-stack.sh <vm|logs|collector|grafana> — run one container of the metrics stack in the
# foreground, so the systemd unit that execs it owns (and fences) the process.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

part="${1:-}"
case "$part" in vm|logs|collector|grafana) ;; *) printf 'usage: metrics-stack.sh vm|logs|collector|grafana\n' >&2; exit 2 ;; esac

if [ -z "${SPIRA_METRICS_DATA:-}" ]; then
    printf 'metrics-stack: SPIRA_METRICS_DATA is empty — %s stays idle\n' "$part" >&2
    [ -z "${SPIRA_METRICS_IDLE_ONCE:-}" ] || exit 0
    _conf_file="$(spira_toml_write_target)"
    _conf_stamp_0="$(conf_stamp "$_conf_file")"
    # waits-on: the config file content; cap 1 day, then the unit restarts and re-checks
    _idle_end=$((SECONDS + 86400))
    while ! conf_changed "$_conf_file" "$_conf_stamp_0" && [ "$SECONDS" -lt "$_idle_end" ]; do sleep 5; done
    exit 0
fi
case "$SPIRA_METRICS_DATA" in /*) ;; *) printf 'metrics-stack: SPIRA_METRICS_DATA must be absolute\n' >&2; exit 2 ;; esac
command -v podman >/dev/null 2>&1 || { printf 'metrics-stack: podman not found on PATH (%s)\n' "$PATH" >&2; exit 2; }

. "$HERE/metrics/stack.env"
data="$SPIRA_METRICS_DATA"
mkdir -p "$data/vm" "$data/logs" "$data/collector/logs" "$data/grafana" || exit 2
timeout 5 podman network exists "$NETWORK" || timeout 5 podman network create "$NETWORK" >/dev/null || exit 2

common=(run --rm --replace --network "$NETWORK" --cgroups=split --security-opt no-new-privileges)

case "$part" in
vm)
    # batch-job: the container is the unit's long-running service process
    exec podman "${common[@]}" --name victoria-metrics \
        -p 127.0.0.1:8428:8428 -v "$data/vm:/storage" "$VM_IMAGE" \
        -storageDataPath=/storage -retentionPeriod="$RETENTION" -httpListenAddr=:8428
    ;;
logs)
    keep="${SPIRA_OTEL_KEEP_DAYS:?metrics-stack: SPIRA_OTEL_KEEP_DAYS unset}"
    case "$keep" in ''|*[!0-9]*|0) printf 'metrics-stack: SPIRA_OTEL_KEEP_DAYS=%q is not a positive number of days\n' "$keep" >&2; exit 2 ;; esac
    # batch-job: the container is the unit's long-running service process
    exec podman "${common[@]}" --name victoria-logs \
        -p 127.0.0.1:9428:9428 -v "$data/logs:/storage" "$VL_IMAGE" \
        -storageDataPath=/storage -retentionPeriod="${keep}d" -httpListenAddr=:9428
    ;;
collector)
    otel="${SPIRA_RUN:?metrics-stack: SPIRA_RUN unset}/otel"
    mkdir -p "$otel" || exit 2
    # batch-job: the container is the unit's long-running service process
    exec podman "${common[@]}" --name otel-collector \
        -p 127.0.0.1:4318:4318 \
        -v "$HERE/metrics/collector.yaml:/etc/otelcol/config.yaml:ro" \
        -v "$otel:/otel:ro" -v "$data/collector:/state" \
        "$COLLECTOR_IMAGE" --config=/etc/otelcol/config.yaml
    ;;
grafana)
    addr="${SPIRA_METRICS_GRAFANA_ADDR:-}"
    host="${addr%:*}"; port="${addr##*:}"
    case "$host" in ''|0.0.0.0|::|'[::]'|'*') printf 'metrics-stack: SPIRA_METRICS_GRAFANA_ADDR=%q is empty or a wildcard — grafana stays down\n' "$addr" >&2; exit 2 ;; esac
    case "$port" in ''|*[!0-9]*) printf 'metrics-stack: SPIRA_METRICS_GRAFANA_ADDR=%q has no numeric port\n' "$addr" >&2; exit 2 ;; esac
    cred="$data/grafana.credential"
    if [ ! -s "$cred" ]; then
        ( umask 077; head -c 24 /dev/urandom | base64 | tr -d '/+=\n' > "$cred" ) || exit 2
    fi
    chmod 600 "$cred" || exit 2
    # batch-job: the container is the unit's long-running service process
    exec podman "${common[@]}" --name grafana \
        --user 0:0 -p "$host:$port:3000" \
        -e GF_SECURITY_ADMIN_PASSWORD__FILE=/run/credential \
        -e GF_ANALYTICS_REPORTING_ENABLED=false -e GF_ANALYTICS_CHECK_FOR_UPDATES=false -e GF_USERS_ALLOW_SIGN_UP=false -e GF_AUTH_ANONYMOUS_ENABLED=false -e "GF_INSTALL_PLUGINS=$VL_PLUGIN" \
        -v "$cred:/run/credential:ro" -v "$data/grafana:/var/lib/grafana" \
        -v "$HERE/metrics/grafana/provisioning:/etc/grafana/provisioning:ro" \
        -v "$HERE/metrics/grafana/dashboards:/var/lib/grafana-dashboards:ro" \
        "$GRAFANA_IMAGE"
    ;;
esac
