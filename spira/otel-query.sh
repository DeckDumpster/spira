#!/usr/bin/env bash
#
# otel-query.sh — DuckDB views over the OTLP/JSON span files spira-otel writes
# ($SPIRA_RUN/otel/<service>.<UTC date>.jsonl, one ExportTraceServiceRequest per line).
#
# usage:
#   otel-query.sh report [<hours>]    verb stats and large_responses for the last <hours> (default 24)
#   otel-query.sh sql    [<hours>]    the view definitions, for `duckdb` to read
#
# Views: otel_calls (one row per span, every day), otel_window (the window), otel_verb_stats
# (per service, verb, caller), large_responses (any response over 400 KiB, by count x size).
# Nothing is keyed on service.name, so a binary that adopts spira-otel appears unchanged.
#
# SPIRA_OTEL_NOW (epoch seconds) pins the window's end, for fixtures.
#
# covers: spira/otel-query.sh spira-otel/src/* spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

NUM_RE='^[0-9]+$'
OTEL_DIR="${SPIRA_OTEL_DIR:-$SPIRA_RUN/otel}"
LARGE_BYTES=409600

_sql() {
    local hours="$1" now="${SPIRA_OTEL_NOW:-}"
    [ -n "$now" ] || now="$(date +%s)"
    [[ "$now" =~ $NUM_RE ]] || { printf 'otel-query: bad SPIRA_OTEL_NOW %q\n' "$now" >&2; return 2; }
    cat <<SQL
CREATE OR REPLACE VIEW otel_raw AS
SELECT * FROM read_json('$OTEL_DIR/*.jsonl', format='newline_delimited', ignore_errors=true, columns={
  resourceSpans: 'STRUCT(
    resource STRUCT(attributes STRUCT("key" VARCHAR, "value" STRUCT(stringValue VARCHAR))[]),
    scopeSpans STRUCT(spans STRUCT(
      name VARCHAR, startTimeUnixNano VARCHAR, endTimeUnixNano VARCHAR,
      attributes STRUCT("key" VARCHAR, "value" STRUCT(stringValue VARCHAR, intValue VARCHAR))[],
      status STRUCT(code BIGINT)
    )[])[]
  )[]'});

CREATE OR REPLACE VIEW otel_calls AS
WITH rs AS (SELECT unnest(resourceSpans) AS r FROM otel_raw),
ss AS (SELECT r.resource AS resource, unnest(r.scopeSpans) AS s FROM rs),
sp AS (SELECT resource, unnest(s.spans) AS span FROM ss)
SELECT
  list_filter(resource.attributes, a -> a."key" = 'service.name')[1]."value".stringValue AS service,
  list_filter(span.attributes, a -> a."key" = 'rpc.method')[1]."value".stringValue AS verb,
  list_filter(span.attributes, a -> a."key" = 'spira.caller')[1]."value".stringValue AS caller,
  TRY_CAST(list_filter(span.attributes, a -> a."key" = 'rpc.request.size')[1]."value".intValue AS BIGINT) AS request_bytes,
  TRY_CAST(list_filter(span.attributes, a -> a."key" = 'rpc.response.size')[1]."value".intValue AS BIGINT) AS response_bytes,
  TRY_CAST(list_filter(span.attributes, a -> a."key" = 'spira.exit_code')[1]."value".intValue AS BIGINT) AS exit_code,
  to_timestamp(TRY_CAST(span.startTimeUnixNano AS HUGEINT) / 1e9) AS start_ts,
  (TRY_CAST(span.endTimeUnixNano AS HUGEINT) - TRY_CAST(span.startTimeUnixNano AS HUGEINT)) / 1e6 AS latency_ms,
  coalesce(span.status.code, 0) = 2 AS is_error
FROM sp;

CREATE OR REPLACE VIEW otel_window AS
SELECT * FROM otel_calls
WHERE start_ts >= to_timestamp($now - $hours * 3600) AND start_ts <= to_timestamp($now);

CREATE OR REPLACE VIEW otel_verb_stats AS
SELECT service, verb, caller,
  count(*) AS calls,
  round(quantile_cont(latency_ms, 0.50), 3) AS p50_ms,
  round(quantile_cont(latency_ms, 0.95), 3) AS p95_ms,
  round(quantile_cont(latency_ms, 0.99), 3) AS p99_ms,
  round(sum(is_error::INTEGER) / count(*)::DOUBLE, 4) AS error_rate,
  max(response_bytes) AS max_response_bytes,
  quantile_disc(response_bytes, 0.99) AS p99_response_bytes
FROM otel_window GROUP BY service, verb, caller;

CREATE OR REPLACE VIEW large_responses AS
SELECT service, verb, caller,
  count(*) FILTER (WHERE response_bytes > $LARGE_BYTES) AS large_calls,
  max(response_bytes) AS max_response_bytes,
  sum(response_bytes) FILTER (WHERE response_bytes > $LARGE_BYTES) AS large_bytes
FROM otel_window GROUP BY service, verb, caller
HAVING large_calls > 0
ORDER BY large_bytes DESC, service, verb, caller;
SQL
}

hours=24
cmd="${1:-}"; shift || true
if [ $# -gt 0 ]; then hours="$1"; fi
[[ "$hours" =~ $NUM_RE ]] || { printf 'otel-query: bad hours %q — must be a non-negative integer\n' "$hours" >&2; exit 2; }

case "$cmd" in
    sql) _sql "$hours" ;;
    report)
        spira_require duckdb || exit 1
        sql="$(_sql "$hours")" || exit $?
        duckdb -c "$sql
SELECT 'verb stats, last $hours h' AS report;
SELECT * FROM otel_verb_stats ORDER BY service, verb, caller;
SELECT 'large_responses (> $LARGE_BYTES bytes)' AS report;
SELECT * FROM large_responses;"
        ;;
    *) printf 'usage: otel-query.sh report|sql [<hours>]\n' >&2; exit 2 ;;
esac
