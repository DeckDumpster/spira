#!/usr/bin/env bash
#
# tsd-query.sh — DuckDB CLI over run/tsd/'s append-only JSONL families: the queries the
# reconciler's flow invariants need (trailing baselines, rates, dwell percentiles). DuckDB
# reads the files in place with no daemon and nothing loaded ahead of time; the files
# themselves, not this script, are the durable contract.
#
# usage:
#   tsd-query.sh baseline <family> <field> <hours>        avg(field) over the trailing window
#   tsd-query.sh rate     <family> <hours>                 rows/hour over the trailing window
#   tsd-query.sh dwell    <family> <field> <p> [<hours>]   p-quantile of field (p in 0..1),
#                                                           over the trailing window, or all rows
#
# <family>/<field> are read straight from a shell command line and interpolated into SQL, so
# both are restricted to a tight identifier charset before they ever reach a query string —
# not sanitized, refused, the same choice tsd::valid_family makes for a path component.
#
# covers: tsd/src/lib.rs tsd/src/main.rs spira/deps.toml spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

spira_require duckdb || exit 1

FAMILY_RE='^[a-z][a-z0-9-]*$'
FIELD_RE='^[a-z_][a-z0-9_]*$'
NUM_RE='^[0-9]+$'
FRAC_RE='^(0(\.[0-9]+)?|1(\.0+)?)$'

usage() {
    cat >&2 <<'USAGE'
usage:
  tsd-query.sh baseline <family> <field> <hours>
  tsd-query.sh rate     <family> <hours>
  tsd-query.sh dwell    <family> <field> <p> [<hours>]
USAGE
}

_family_path() {
    printf '%s/tsd/%s.jsonl' "$SPIRA_RUN" "$1"
}

# _check_family <name> -> path on stdout, or a message on stderr and a non-zero return.
# Called through $(...), so this returns rather than exits — exiting here would only kill
# the subshell command substitution runs in, leaving the caller none the wiser.
_check_family() {
    [[ "$1" =~ $FAMILY_RE ]] || { printf 'tsd-query: bad family %q\n' "$1" >&2; return 2; }
    local path; path="$(_family_path "$1")"
    [ -f "$path" ] || {
        printf 'tsd-query: no rows yet for family %q (%s does not exist)\n' "$1" "$path" >&2
        return 3
    }
    printf '%s' "$path"
}

_check_field() {
    [[ "$1" =~ $FIELD_RE ]] || { printf 'tsd-query: bad field %q\n' "$1" >&2; exit 2; }
}

_check_hours() {
    [[ "$1" =~ $NUM_RE ]] || {
        printf 'tsd-query: bad hours %q — must be a non-negative integer\n' "$1" >&2
        exit 2
    }
}

cmd="${1:-}"; shift || true
case "$cmd" in
    baseline)
        family="${1:?family required}"; field="${2:?field required}"; hours="${3:?hours required}"
        _check_field "$field"; _check_hours "$hours"
        path="$(_check_family "$family")" || exit $?
        duckdb -json -c "
            SELECT avg(\"$field\") AS baseline, count(*) AS n
            FROM read_ndjson_auto('$path')
            WHERE CAST(ts AS TIMESTAMP) >= now() - INTERVAL '$hours hours';
        "
        ;;
    rate)
        family="${1:?family required}"; hours="${2:?hours required}"
        _check_hours "$hours"
        path="$(_check_family "$family")" || exit $?
        duckdb -json -c "
            SELECT count(*) AS n, count(*) / GREATEST($hours, 1)::DOUBLE AS rate_per_hour
            FROM read_ndjson_auto('$path')
            WHERE CAST(ts AS TIMESTAMP) >= now() - INTERVAL '$hours hours';
        "
        ;;
    dwell)
        family="${1:?family required}"; field="${2:?field required}"; p="${3:?percentile required}"
        hours="${4:-}"
        _check_field "$field"
        [[ "$p" =~ $FRAC_RE ]] || { printf 'tsd-query: bad percentile %q — must be 0..1\n' "$p" >&2; exit 2; }
        path="$(_check_family "$family")" || exit $?
        where=""
        if [ -n "$hours" ]; then
            _check_hours "$hours"
            where="WHERE CAST(ts AS TIMESTAMP) >= now() - INTERVAL '$hours hours'"
        fi
        duckdb -json -c "
            SELECT quantile_cont(\"$field\", $p) AS \"p$p\", count(*) AS n
            FROM read_ndjson_auto('$path')
            $where;
        "
        ;;
    *)
        usage; exit 2 ;;
esac
