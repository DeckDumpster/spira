#!/usr/bin/env bash
#
# legacy-tsd-query.sh — the DuckDB queries tsd-query.sh ran over run/tsd/*.jsonl before the
# observational families moved to the metrics store, frozen as the oracle tsd/tests/parity.rs
# compares tsd-read against. Nothing else runs it. Reads $SPIRA_RUN/tsd/<family>.jsonl.
set -uo pipefail

FAMILY_RE='^[a-z][a-z0-9-]*$'
FIELD_RE='^[a-z_][a-z0-9_]*$'
NUM_RE='^[0-9]+$'
FRAC_RE='^(0(\.[0-9]+)?|1(\.0+)?)$'
BEAD_RE='^[a-z][a-z0-9]*-[a-z0-9.]+$'
DEFAULT_HOURS=720
STAGE_FAMILY=bead-stage

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
    where)
        hours="${1:-$DEFAULT_HOURS}"; _check_hours "$hours"
        path="$(_check_family "$STAGE_FAMILY")" || exit $?
        duckdb -json -c "
            WITH latest AS (
                SELECT to_state, epoch(now()) - epoch(CAST(ts AS TIMESTAMPTZ)) AS dwell_s,
                       row_number() OVER (PARTITION BY key ORDER BY seq DESC) AS rn
                FROM $(_rows $path ts seq machine key from_state to_state applied reason)
                WHERE machine = 'bead' AND applied
                  AND CAST(ts AS TIMESTAMPTZ) >= now() - INTERVAL '$hours hours'
            )
            SELECT to_state AS state, count(*) AS wip,
                   CAST(quantile_cont(dwell_s, 0.5) AS BIGINT) AS dwell_p50_s,
                   CAST(max(dwell_s) AS BIGINT) AS dwell_max_s
            FROM latest
            WHERE rn = 1 AND to_state IN ('READY','WORKING','SUBMITTED','CERTIFIED','IN_DELIVERY','REWORK')
            GROUP BY to_state
            ORDER BY CASE to_state WHEN 'READY' THEN 1 WHEN 'WORKING' THEN 2 WHEN 'SUBMITTED' THEN 3
                                   WHEN 'CERTIFIED' THEN 4 WHEN 'IN_DELIVERY' THEN 5 ELSE 6 END;
        "
        ;;
    rework)
        hours="${1:-$DEFAULT_HOURS}"; _check_hours "$hours"
        path="$(_check_family "$STAGE_FAMILY")" || exit $?
        duckdb -json -c "
            WITH win AS (
                SELECT key, to_state, COALESCE(reason, 'unknown') AS reason
                FROM $(_rows $path ts seq machine key from_state to_state applied reason)
                WHERE machine = 'bead' AND applied
                  AND CAST(ts AS TIMESTAMPTZ) >= now() - INTERVAL '$hours hours'
            ), landed AS (
                SELECT count(DISTINCT key) AS n FROM win WHERE to_state = 'LANDED'
            ), reopens AS (
                SELECT reason, count(*) AS n FROM win WHERE to_state = 'REWORK' GROUP BY reason
            )
            SELECT '*' AS reason, COALESCE((SELECT sum(n) FROM reopens), 0)::BIGINT AS reopens,
                   (SELECT n FROM landed) AS landed, 0 AS ord
            UNION ALL
            SELECT reason, n, (SELECT n FROM landed), 1 FROM reopens
            ORDER BY ord, reopens DESC, reason;
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
    time)
        hours="${1:-$DEFAULT_HOURS}"; _check_hours "$hours"
        apath="$(_check_family aeon-session)" || exit $?
        gpath="$(_check_family gate-run)" || exit $?
        spath="$(_check_family slots)" || exit $?
        rpath="$(_family_path batch-round)"
        rsrc="SELECT 0.0 AS secs WHERE false"
        [ -f "$rpath" ] && rsrc="SELECT CAST(duration_ms AS DOUBLE) / 1000 AS secs FROM $(_rows $rpath ts duration_ms) WHERE CAST(ts AS TIMESTAMPTZ) >= now() - INTERVAL '$hours hours'"
        duckdb -json -c "
            WITH sl AS (
                SELECT CAST(ts AS TIMESTAMPTZ) AS t, ceiling, live,
                       CASE WHEN lead(ts) OVER (ORDER BY ts) IS NULL THEN NULL
                            ELSE least(epoch(lead(ts) OVER (ORDER BY ts)) - epoch(ts), 300) END AS dt
                FROM $(_rows $spath ts live ceiling ready) WHERE ts IS NOT NULL
            )
            SELECT
                (SELECT CAST(COALESCE(sum(wall_s), 0) AS BIGINT) FROM $(_rows $apath ts bead rune status wall_s)
                    WHERE CAST(ts AS TIMESTAMPTZ) >= now() - INTERVAL '$hours hours') AS aeon_s,
                (SELECT CAST(COALESCE(sum(ran_secs), 0) AS BIGINT) FROM $(_rows $gpath ts bead status reason ran_secs)
                    WHERE CAST(ts AS TIMESTAMPTZ) >= now() - INTERVAL '$hours hours') AS gate_s,
                (SELECT CAST(COALESCE(sum(secs), 0) AS BIGINT) FROM ($rsrc)) AS round_s,
                (SELECT CAST(COALESCE(sum(greatest(ceiling - live, 0) * dt), 0) AS BIGINT) FROM sl
                    WHERE t >= now() - INTERVAL '$hours hours' AND dt IS NOT NULL) AS idle_slot_s;
        "
        ;;
    sentinel)
        hours="${1:-$DEFAULT_HOURS}"; _check_hours "$hours"
        path="$(_check_family sentinel-phase)" || exit $?
        duckdb -json -c "
            WITH w AS (
                SELECT pass, \"check\" AS phase, secs
                FROM $(_rows $path ts pass check secs)
                WHERE CAST(ts AS TIMESTAMPTZ) >= now() - INTERVAL '$hours hours'
            ), per_pass AS (
                SELECT pass, sum(secs) AS total FROM w GROUP BY pass
            ), top AS (
                SELECT phase, sum(secs) AS secs FROM w GROUP BY phase ORDER BY secs DESC, phase LIMIT 3
            )
            SELECT quantile_cont(total, 0.5) AS p50, quantile_cont(total, 0.9) AS p90, count(*) AS passes,
                   (SELECT list(phase || ':' || CAST(secs AS BIGINT)) FROM top) AS top_phases
            FROM per_pass;
        "
        ;;
    bead)
        id="${1:?bead id required}"
        [[ "$id" =~ $BEAD_RE ]] || { printf 'tsd-query: bad bead %q\n' "$id" >&2; exit 2; }
        path="$(_check_family "$STAGE_FAMILY")" || exit $?
        extra=""
        [ -f "$(_family_path aeon-session)" ] && extra="$extra
            UNION ALL SELECT ts, 'aeon', rune || ' ' || status, CAST(wall_s AS BIGINT)
            FROM $(_rows $(_family_path aeon-session) ts bead rune status wall_s) WHERE bead = $(_sqlstr "$id")"
        [ -f "$(_family_path gate-run)" ] && extra="$extra
            UNION ALL SELECT ts, 'gate', status || ' ' || COALESCE(reason, ''), CAST(ran_secs AS BIGINT)
            FROM $(_rows $(_family_path gate-run) ts bead status reason ran_secs) WHERE bead = $(_sqlstr "$id")"
        duckdb -json -c "
            SELECT * FROM (
                SELECT ts, 'stage' AS kind,
                       from_state || ' -> ' || to_state || COALESCE(' (' || reason || ')', '') AS what,
                       CAST(epoch(CAST(ts AS TIMESTAMPTZ)) - epoch(CAST(lag(ts) OVER (ORDER BY seq) AS TIMESTAMPTZ)) AS BIGINT) AS secs
                FROM $(_rows $path ts seq machine key from_state to_state applied reason)
                WHERE machine = 'bead' AND applied AND key = $(_sqlstr "$id")
                $extra
            ) ORDER BY ts, kind;
        "
        ;;
    *)
        usage; exit 2 ;;
esac
