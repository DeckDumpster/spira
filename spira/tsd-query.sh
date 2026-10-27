#!/usr/bin/env bash
#
# tsd-query.sh — the queries the reconciler's flow invariants need (trailing baselines, rates,
# dwell percentiles) over run/tsd/'s families. A family the metrics store holds (`tsd-read
# moved <family>`) is answered by tsd-read, which refuses by name when the store is unreachable
# or holds nothing for the window; the files of the others are read in place by DuckDB, with no
# daemon and nothing loaded ahead of time.
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
#   tsd-query.sh count-by <family> <group> [<hours>]
#                                                           count(*) per distinct value of
#                                                           <group>, tab-separated "<group>\t<n>"
#                                                           with no header, most-frequent first
#   tsd-query.sh suite-p50      <suite> <n>                p50 wall_secs over <suite>'s last
#                                                           <n> suite-timing rows, every host
#                                                           (local and CI) counted together
#   tsd-query.sh suite-medians  <n>                        the same, for every suite in one
#                                                           query: suite, median, n
#   tsd-query.sh suite-p90s     <n>                        the same shape as suite-medians,
#                                                           quantile 0.9 instead of 0.5 —
#                                                           what gate-budget-select.sh read; suite-select
#                                                           computes the same P90 itself (sp-wx2tw)
#   tsd-query.sh last-run                                  most recent run_id: sum(wall_secs)
#                                                           over its suites, and its __batch__
#                                                           row's wall_secs
#   tsd-query.sh slow-in-branch <branch> <n>                the <n> slowest suite-timing rows
#                                                           (raw, not deduped) for <branch>
#
#   tsd-query.sh where  [<hours>]                          WIP and dwell per bead stage, now:
#                                                           {state, wip, dwell_p50_s, dwell_max_s}
#   tsd-query.sh rework [<hours>]                          reopens (to REWORK) vs landed beads:
#                                                           one '*' row, then one row per reason
#   tsd-query.sh time   [<hours>]                          seconds in dwarf sessions, gate runs,
#                                                           batch rounds, and empty slots
#   tsd-query.sh slots  [<hours>]                          empty-slot-minutes, split by whether
#                                                           work was ready
#   tsd-query.sh sentinel [<hours>]                        pass p50/p90 secs and top phases
#   tsd-query.sh bead   <id>                               one bead's timeline, oldest first
#   (<hours> defaults to 720, thirty days; each prints JSON, and a missing
#   family exits non-zero so a caller shows ?, never 0)
#
# <family>/<field> are read straight from a shell command line and interpolated into SQL, so
# both are restricted to a tight identifier charset before they ever reach a query string —
# not sanitized, refused, the same choice tsd::valid_family makes for a path component. A
# suite name is not an identifier (it carries dots and slashes-as-hyphens from a path), so it
# gets its own charset and is quoted as a SQL string literal, single quotes doubled.
#
# covers: tsd/src/lib.rs tsd/src/main.rs tsd/src/query.rs tsd/src/read_main.rs spira/deps.toml spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

FAMILY_RE='^[a-z][a-z0-9-]*$'
FIELD_RE='^[a-z_][a-z0-9_]*$'
NUM_RE='^[0-9]+$'
FRAC_RE='^(0(\.[0-9]+)?|1(\.0+)?)$'
SUITE_RE='^[A-Za-z0-9._-]+$'
BEAD_RE='^[a-z][a-z0-9]*-[a-z0-9.]+$'
DEFAULT_HOURS=720
BRANCH_RE='^[A-Za-z0-9._/-]+$'

usage() {
    cat >&2 <<'USAGE'
usage:
  tsd-query.sh baseline      <family> <field> <hours>
  tsd-query.sh rate          <family> <hours>
  tsd-query.sh dwell         <family> <field> <p> [<hours>]
  tsd-query.sh by-group      <family> <group> <field> [<hours>]
  tsd-query.sh count-by      <family> <group> [<hours>]
  tsd-query.sh suite-p50     <suite> <n>
  tsd-query.sh suite-medians <n>
  tsd-query.sh suite-p90s    <n>
  tsd-query.sh last-run
  tsd-query.sh slow-in-branch <branch> <n>
  tsd-query.sh where|rework|time|slots|sentinel [<hours>]
  tsd-query.sh bead          <id>
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

# _rows <jsonl path> <col>... — the columns as VARCHAR, malformed lines skipped: these
# families are appended by many producers, and one bad line must not blind a headline.
_rows() {
    local path="$1" cols="" casts="" c t; shift
    for c in "$@"; do
        cols="${cols:+$cols, }\"$c\": 'VARCHAR'"
        case "$c" in
            seq|live|ceiling|ready) t=BIGINT ;;
            wall_s|ran_secs|secs|duration_ms) t=DOUBLE ;;
            applied) t=BOOLEAN ;;
            ts) t=TIMESTAMPTZ ;;
            *) continue ;;
        esac
        casts="${casts:+$casts, }TRY_CAST(\"$c\" AS $t) AS \"$c\""
    done
    printf "(SELECT * REPLACE (%s) FROM read_json('%s', format='newline_delimited', columns={%s}, ignore_errors=true))" "${casts:-ts AS ts}" "$path" "$cols"
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
    baseline|rate|dwell|by-group|count-by)
        if tsd-read moved "${1:-}"; then exec tsd-read "$cmd" "$@"; fi ;;
esac
case "$cmd" in
    where|rework|time|sentinel|bead) ;;
    *) spira_require duckdb || exit 1 ;;
esac

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
    count-by)
        family="${1:?family required}"; group="${2:?group required}"; hours="${3:-}"
        _check_field "$group"
        path="$(_check_family "$family")" || exit $?
        where=""
        if [ -n "$hours" ]; then
            _check_hours "$hours"
            where="WHERE CAST(ts AS TIMESTAMP) >= now() - INTERVAL '$hours hours'"
        fi
        duckdb -csv -separator '	' -noheader -c "
            SELECT \"$group\", count(*)
            FROM read_ndjson_auto('$path')
            $where
            GROUP BY \"$group\"
            ORDER BY 2 DESC, 1;
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
    suite-p90s)
        n="${1:?n required}"
        _check_n "$n"
        path="$(_check_family suite-timing)" || exit $?
        duckdb -json -c "
            WITH ranked AS (
                SELECT suite, wall_secs,
                       row_number() OVER (PARTITION BY suite ORDER BY ts DESC) AS rn
                FROM read_ndjson_auto('$path')
            )
            SELECT suite, quantile_cont(wall_secs, 0.9) AS p90, count(*) AS n
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
                (SELECT CAST(COALESCE(sum(wall_secs), 0) AS BIGINT) FROM read_ndjson_auto('$path')
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
    slots)
        hours="${1:-$DEFAULT_HOURS}"; _check_hours "$hours"
        path="$(_check_family slots)" || exit $?
        duckdb -json -c "
            WITH s AS (
                SELECT CAST(ts AS TIMESTAMPTZ) AS t, ceiling, live, ready,
                       CASE WHEN lead(ts) OVER (ORDER BY ts) IS NULL THEN NULL
                            ELSE least(epoch(lead(ts) OVER (ORDER BY ts)) - epoch(ts), 300) END AS dt
                FROM $(_rows $path ts live ceiling ready) WHERE ts IS NOT NULL
            )
            SELECT CAST(COALESCE(sum(greatest(ceiling - live, 0) * dt) FILTER (WHERE ready > 0), 0) / 60 AS BIGINT) AS empty_with_work_min,
                   CAST(COALESCE(sum(greatest(ceiling - live, 0) * dt) FILTER (WHERE ready = 0), 0) / 60 AS BIGINT) AS empty_idle_min,
                   count(*) AS samples
            FROM s WHERE t >= now() - INTERVAL '$hours hours' AND dt IS NOT NULL;
        "
        ;;
    where|rework|time|sentinel)
        [[ "${1:-$DEFAULT_HOURS}" =~ $NUM_RE ]] || { printf 'tsd-query: bad hours %q — must be a non-negative integer\n' "$1" >&2; exit 2; }
        exec tsd-read "$cmd" "$@"
        ;;
    bead)
        id="${1:?bead id required}"
        [[ "$id" =~ $BEAD_RE ]] || { printf 'tsd-query: bad bead %q\n' "$id" >&2; exit 2; }
        exec tsd-read bead "$id"
        ;;
    *)
        usage; exit 2 ;;
esac
