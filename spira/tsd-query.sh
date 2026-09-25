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
#   tsd-query.sh by-group <family> <group> <field> [<hours>]
#                                                           avg(field) per distinct value of
#                                                           <group>, longest-first, tab-separated
#                                                           "<group>\t<avg>" with no header —
#                                                           meant for a shell caller, not a human
#   tsd-query.sh suite-p50      <suite> <n>                p50 wall_secs over <suite>'s last
#                                                           <n> suite-timing rows, every host
#                                                           (local and CI) counted together
#   tsd-query.sh suite-medians  <n>                        the same, for every suite in one
#                                                           query: suite, median, n
#   tsd-query.sh last-run                                  most recent run_id: sum(wall_secs)
#                                                           over its suites, and its __batch__
#                                                           row's wall_secs
#   tsd-query.sh slow-in-branch <branch> <n>                the <n> slowest suite-timing rows
#                                                           (raw, not deduped) for <branch>
#
# <family>/<field> are read straight from a shell command line and interpolated into SQL, so
# both are restricted to a tight identifier charset before they ever reach a query string —
# not sanitized, refused, the same choice tsd::valid_family makes for a path component. A
# suite name is not an identifier (it carries dots and slashes-as-hyphens from a path), so it
# gets its own charset and is quoted as a SQL string literal, single quotes doubled.
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
SUITE_RE='^[A-Za-z0-9._-]+$'
BRANCH_RE='^[A-Za-z0-9._/-]+$'

usage() {
    cat >&2 <<'USAGE'
usage:
  tsd-query.sh baseline      <family> <field> <hours>
  tsd-query.sh rate          <family> <hours>
  tsd-query.sh dwell         <family> <field> <p> [<hours>]
  tsd-query.sh by-group      <family> <group> <field> [<hours>]
  tsd-query.sh suite-p50     <suite> <n>
  tsd-query.sh suite-medians <n>
  tsd-query.sh last-run
  tsd-query.sh slow-in-branch <branch> <n>
USAGE
}

_check_suite() {
    [[ "$1" =~ $SUITE_RE ]] || { printf 'tsd-query: bad suite %q\n' "$1" >&2; exit 2; }
}

_check_branch() {
    [[ "$1" =~ $BRANCH_RE ]] || { printf 'tsd-query: bad branch %q\n' "$1" >&2; exit 2; }
}

_check_n() {
    [[ "$1" =~ $NUM_RE ]] && [ "$1" -ge 1 ] || {
        printf 'tsd-query: bad n %q — must be a positive integer\n' "$1" >&2
        exit 2
    }
}

# SQL-quote: double every single quote, then wrap. Belt-and-suspenders alongside SUITE_RE —
# the charset alone already refuses anything a quote could do damage with.
_sqlstr() { printf "'%s'" "${1//\'/\'\'}"; }

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
    by-group)
        family="${1:?family required}"; group="${2:?group required}"; field="${3:?field required}"
        hours="${4:-}"
        _check_field "$group"; _check_field "$field"
        path="$(_check_family "$family")" || exit $?
        where=""
        if [ -n "$hours" ]; then
            _check_hours "$hours"
            where="WHERE CAST(ts AS TIMESTAMP) >= now() - INTERVAL '$hours hours'"
        fi
        duckdb -csv -separator '	' -noheader -c "
            SELECT \"$group\", avg(\"$field\")
            FROM read_ndjson_auto('$path')
            $where
            GROUP BY \"$group\"
            ORDER BY 2 DESC;
        "
        ;;
    suite-p50)
        suite="${1:?suite required}"; n="${2:?n required}"
        _check_suite "$suite"; _check_n "$n"
        path="$(_check_family suite-timing)" || exit $?
        duckdb -json -c "
            WITH recent AS (
                SELECT wall_secs
                FROM read_ndjson_auto('$path')
                WHERE suite = $(_sqlstr "$suite")
                ORDER BY ts DESC
                LIMIT $n
            )
            SELECT quantile_cont(wall_secs, 0.5) AS p50, count(*) AS n FROM recent;
        "
        ;;
    suite-medians)
        n="${1:?n required}"
        _check_n "$n"
        path="$(_check_family suite-timing)" || exit $?
        duckdb -json -c "
            WITH ranked AS (
                SELECT suite, wall_secs,
                       row_number() OVER (PARTITION BY suite ORDER BY ts DESC) AS rn
                FROM read_ndjson_auto('$path')
            )
            SELECT suite, quantile_cont(wall_secs, 0.5) AS median, count(*) AS n
            FROM ranked
            WHERE rn <= $n
            GROUP BY suite
            ORDER BY suite;
        "
        ;;
    last-run)
        path="$(_check_family suite-timing)" || exit $?
        duckdb -json -c "
            WITH latest AS (
                SELECT run_id FROM read_ndjson_auto('$path') ORDER BY ts DESC LIMIT 1
            )
            SELECT
                (SELECT run_id FROM latest) AS run_id,
                (SELECT COALESCE(sum(wall_secs), 0) FROM read_ndjson_auto('$path')
                    WHERE run_id = (SELECT run_id FROM latest) AND suite != '__batch__') AS sum_wall,
                (SELECT wall_secs FROM read_ndjson_auto('$path')
                    WHERE run_id = (SELECT run_id FROM latest) AND suite = '__batch__'
                    LIMIT 1) AS batch_wall;
        "
        ;;
    slow-in-branch)
        branch="${1:?branch required}"; n="${2:?n required}"
        _check_branch "$branch"; _check_n "$n"
        path="$(_check_family suite-timing)" || exit $?
        duckdb -json -c "
            SELECT suite, wall_secs
            FROM read_ndjson_auto('$path')
            WHERE branch = $(_sqlstr "$branch") AND suite != '__batch__'
            ORDER BY wall_secs DESC
            LIMIT $n;
        "
        ;;
    *)
        usage; exit 2 ;;
esac
