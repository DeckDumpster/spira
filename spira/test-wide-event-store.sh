#!/usr/bin/env bash
#
# test-wide-event-store.sh — a log record carrying an arbitrary attribute, sent in the OTLP form the
# collector's VictoriaLogs exporter emits, is queryable in VictoriaLogs by that attribute, and the
# wide-event dashboard's queries parse. Runs the official release binary at the version pinned in
# stack.env; needs no container runtime.
#
# tier: T1
# covers: spira/metrics/** spira/metrics-stack.sh systemd/spira-metrics-*.service
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
printf 'test-wide-event-store.sh\n'
. "$HERE/metrics/stack.env"

ver="${VL_IMAGE##*:}"
case "$(uname -m)" in x86_64) arch=amd64 ;; aarch64) arch=arm64 ;; *) bad "no VictoriaLogs release for $(uname -m)"; tl_summary; exit 1 ;; esac
cache="${XDG_CACHE_HOME:-$HOME/.cache}/spira-test-victoria-logs/$ver"
bin="$cache/victoria-logs-prod"
if [ ! -x "$bin" ]; then
    mkdir -p "$cache" || { bad "cache dir"; tl_summary; exit 1; }
    url="https://github.com/VictoriaMetrics/VictoriaLogs/releases/download/$ver/victoria-logs-linux-$arch-$ver.tar.gz"
    curl -fsSL --retry 3 --connect-timeout 10 --max-time 120 -o "$cache/dl.tgz" "$url" && timeout 60 tar -xzf "$cache/dl.tgz" -C "$cache" victoria-logs-prod # batch-job: one-time fetch of the pinned release, bounded by curl --max-time
    rm -f "$cache/dl.tgz"
    [ -x "$bin" ] || { bad "cannot fetch $url"; tl_summary; exit 1; }
fi

T="$(mktemp -d)"
port="$(python3 -I -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
"$bin" -storageDataPath="$T/data" -retentionPeriod=3d -httpListenAddr="127.0.0.1:$port" >"$T/vl.log" 2>&1 &
vlpid=$!
trap 'kill "$vlpid" 2>/dev/null; wait "$vlpid" 2>/dev/null; rm -rf "$T"' EXIT INT TERM
vl="http://127.0.0.1:$port"

up=""
for _ in $(seq 1 30); do [ "$(curl --max-time 5 -s "$vl/health")" = OK ] && { up=1; break; }; sleep 1; done
is "victoria-logs starts" 1 "$up"

path="$(sed -n 's|.*logs_endpoint: http://victoria-logs:9428||p' "$HERE/metrics/collector.yaml")"
is "collector exports logs to the VictoriaLogs OTLP path" "/insert/opentelemetry/v1/logs" "$path"

needle="needle-$$-$RANDOM"
rec() { python3 -I -c 'import struct,sys,time
def v(n):
    o = b""
    while n > 0x7f: o += bytes([n & 0x7f | 0x80]); n >>= 7
    return o + bytes([n])
def f(no, b): return v(no << 3 | 2) + v(len(b)) + b
def kv(k, val): return f(1, k.encode()) + f(2, val)
def sv(x): return f(1, x.encode())
lr = v(1 << 3 | 1) + struct.pack("<Q", int(time.time()) * 10**9) + f(5, sv("dispatch"))
lr += b"".join(f(6, kv(k, val)) for k, val in [("handler", sv("h1")), ("duration_ms", v(4 << 3 | 1) + struct.pack("<d", 12.5)), ("zz.arbitrary", sv(sys.argv[1]))])
res = f(1, kv("service.name", sv("t")))
sys.stdout.buffer.write(f(1, f(1, res) + f(2, f(2, lr))))' "$1"; }
code="$(curl --max-time 5 -s -o /dev/null -w '%{http_code}' -H 'Content-Type: application/x-protobuf' --data-binary @- "$vl$path" < <(rec "$needle"))"
is "victoria-logs accepts an OTLP log record" 200 "$code"

q() { curl --max-time 5 -s "$vl/select/logsql/query" --data-urlencode "query=$1"; }
got=""
for _ in $(seq 1 30); do curl --max-time 5 -s -o /dev/null "$vl/internal/force_flush"; got="$(q "zz.arbitrary:$needle")"; [ -n "$got" ] && break; sleep 1; done
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
