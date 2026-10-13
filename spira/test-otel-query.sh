#!/usr/bin/env bash
#
# test-otel-query.sh — otel-query.sh's DuckDB views over spira-otel's OTLP/JSON span files:
# counts, percentiles, error rate and size per (service, verb, caller), and large_responses.
#
# covers: spira/otel-query.sh spira-otel/src/* spira-otel/tests/fixtures/* spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

printf 'test-otel-query.sh\n'
command -v duckdb >/dev/null 2>&1 || skip "duckdb is not on PATH"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

span() { # service verb caller start_s latency_ms response_bytes code
    printf '{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"%s"}}]},"scopeSpans":[{"scope":{"name":"spira-otel"},"spans":[{"name":"%s","startTimeUnixNano":"%s","endTimeUnixNano":"%s","attributes":[{"key":"rpc.method","value":{"stringValue":"%s"}},{"key":"spira.caller","value":{"stringValue":"%s"}},{"key":"rpc.request.size","value":{"intValue":"10"}},{"key":"rpc.response.size","value":{"intValue":"%s"}}],"status":{"message":"","code":%s}}]}]}]}\n' \
        "$1" "$2" "$(($4 * 1000000000))" "$(($4 * 1000000000 + $5 * 1000000))" "$2" "$3" "$6" "$7"
}

NOW=1700003600
SYN="$T/syn"; mkdir -p "$SYN"
{
    span spira-lc show callerA 1700000000 10 100 1
    span spira-lc show callerA 1700000001 20 200 1
    span spira-lc show callerA 1700000002 30 409600 1
    span spira-lc list callerB 1700000003 50 439002 1
    span spira-lc show callerB 1700000004 5 96 2
    span other-svc ping callerC 1700000005 7 12 1
    printf 'not json\n'
} > "$SYN/spira-lc.2023-11-14.jsonl"
span spira-lc list callerB 1600000000 99 900000 1 > "$SYN/spira-lc.2020-09-13.jsonl"

q() { # dir hours sql
    local sql
    sql="$(env -i PATH="$PATH" HOME="$T" SPIRA_OTEL_DIR="$1" SPIRA_OTEL_NOW="$NOW" SPIRA_RUN="$T" \
        SPIRA_HOME="$T" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent "$HERE/otel-query.sh" sql "$2" 2>&1)" || { printf '%s\n' "$sql"; return 1; }
    duckdb -csv -noheader -c "$sql $3" 2>&1
}

out="$(q "$SYN" 1 "SELECT service, verb, caller, calls, p50_ms, error_rate, max_response_bytes, p99_response_bytes FROM otel_verb_stats ORDER BY ALL;")"
is "synthetic verb stats: exact rows, old file and junk line excluded" \
"other-svc,ping,callerC,1,7.0,0.0,12,12
spira-lc,list,callerB,1,50.0,0.0,439002,439002
spira-lc,show,callerA,3,20.0,0.0,409600,409600
spira-lc,show,callerB,1,5.0,1.0,96,96" "$out"

out="$(q "$SYN" 1 "SELECT service, verb, caller, large_calls, max_response_bytes FROM large_responses;")"
is "large_responses lists the 439002-byte row and not the 409600 one" "spira-lc,list,callerB,1,439002" "$out"

out="$(q "$SYN" 1 "SELECT count(*) FROM otel_calls;")"
is "otel_calls spans every day: 6 + 1 rows" "7" "$out"

out="$(q "$SYN" 1 "SELECT p99_ms FROM otel_verb_stats WHERE caller='callerA';")"
is "p99 latency interpolates (10,20,30 ms -> 29.8)" "29.8" "$out"

out="$(q "$T/none" 1 "SELECT count(*) FROM otel_calls;")"
nowant "a missing directory yields an error, not an empty answer" "0" "$out"

FIX="$T/fix"; mkdir -p "$FIX"
cp "$HERE/../spira-otel/tests/fixtures/spira-lc.ndjson" "$FIX/spira-lc.2023-11-14.jsonl"
out="$(q "$FIX" 1 "SELECT count(*), sum(is_error::INT), max(response_bytes) FROM otel_window;")"
is "checked-in fixture: 4 calls, 1 error, 439002 max" "4,1,439002" "$out"
out="$(q "$FIX" 1 "SELECT verb, caller FROM large_responses;")"
is "checked-in fixture: large_responses is the list call" "list,work/list" "$out"

tl_summary
