#!/usr/bin/env bash
#
# lc-backup.sh — off-box backup of every database the harness owns, its age alarm and its restore drill.
#
#   lc-backup.sh run                 per database: snapshot locally, tar it, upload ONE object, stamp it
#   lc-backup.sh age [--now EPOCH]   exit 0 when every database's newest good backup is younger than SPIRA_LC_BACKUP_MAX_AGE
#   lc-backup.sh drill [--db NAME] [TARBALL]   per database: restore a snapshot into a scratch dir under SPIRA_RUN; row counts must equal live; exit 75 when deferred past the cap (the unit retries)
#   lc-backup.sh watch [--interval S] [--ticks N]   watchd daemon: mails the crossing into and out of stale
#   lc-backup.sh health              exit non-zero when the watch loop has stopped polling
#   lc-backup.sh explain             after a failed run: put the log's last error line on stderr and in the journal
#   lc-backup.sh explain-drill       after a failed drill: journal each database's drill.verdict and mail it to the operator
#
# SPIRA_LC_BACKUP_URL is s3://bucket/prefix/ (credentials and region from the environment, which
# the service loads from the operator's env file) or file://dir. The key is write-only: the
# upload is one s3:PutObject, and nothing here lists, reads or deletes the bucket — so the age
# check reads the local stamp, and the periodic drill restores the local copy of the exact
# tarball that was uploaded. Restoring from the bucket is the operator's, under their own identity.
#
# The snapshot goes through the running server (`dolt sql` in the database directory), so it is
# one consistent commit however busy the store is. Only committed history snapshots, so the
# working set is committed first.
#
# SPIRA_LC_BACKUP_DB is a space-separated list of databases. Each has its own stamp
# (lc-backup.<db>.ok) and local tarball, so one database's fresh backup cannot mask another's.
#
# A stamp is written only after the upload exits clean, and carries the commit it holds. The
# drill restores that tarball and compares every table's count AS OF the stamped commit, live and restored alike, so
# writes landing meanwhile cannot move either side.
#
# No destination, an unreadable stamp or an age beyond the limit is red, never a skip — a
# store that was never backed up must look different from one that is.

# covers: spira/lc-backup.sh spira/watchers spira/conf.d/SPIRA_LC_BACKUP_* systemd/lc-backup.service systemd/lc-backup.timer systemd/lc-backup-drill.*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

STATE_FILE="$SPIRA_RUN/watchd/lc-backup.state"
HEALTH_FILE="$SPIRA_RUN/watchd/lc-backup.health"

_lcb_dbs() { printf '%s\n' $SPIRA_LC_BACKUP_DB; }
_lcb_db_dir() { printf '%s/%s' "$SPIRA_DOLT_DATA" "$1"; }
_lcb_stamp() { printf '%s/lc-backup.%s.ok' "$SPIRA_RUN" "$1"; }

_lcb_refuse_unconfigured() {
    [ -n "$SPIRA_LC_BACKUP_URL" ] || { echo "lc-backup: SPIRA_LC_BACKUP_URL is empty — the stores have no backup destination" >&2; return 1; }
    [ -n "$(_lcb_dbs)" ] || { echo "lc-backup: SPIRA_LC_BACKUP_DB names no database" >&2; return 1; }
}

_lcb_require_db() {
    [ -n "$SPIRA_DOLT_DATA" ] && [ -d "$(_lcb_db_dir "$1")/.dolt" ] \
        || { echo "lc-backup: no dolt database at $(_lcb_db_dir "$1")" >&2; return 1; }
}

# A file:// destination on the data directory's own filesystem dies with the disk it protects.
_lcb_refuse_same_disk() {
    local path dev_dest dev_data
    case "$SPIRA_LC_BACKUP_URL" in file://*) path="${SPIRA_LC_BACKUP_URL#file://}" ;; *) return 0 ;; esac
    case "$SPIRA_LC_BACKUP_ALLOW_LOCAL" in 1|true|yes|on) return 0 ;; esac
    mkdir -p "$path" 2>/dev/null
    dev_dest="$(stat -c %d "$path" 2>/dev/null)"
    dev_data="$(stat -c %d "$SPIRA_DOLT_DATA" 2>/dev/null)"
    if [ -z "$dev_dest" ] || [ -z "$dev_data" ] || [ "$dev_dest" = "$dev_data" ]; then
        echo "lc-backup: $SPIRA_LC_BACKUP_URL is not off-box: same filesystem as $SPIRA_DOLT_DATA (SPIRA_LC_BACKUP_ALLOW_LOCAL=1 accepts it)" >&2
        return 1
    fi
}

# batch-job: snapshot and restore of the whole store run for as long as the store is large
_lcb_sql() { local db="$1"; shift; ( cd "$(_lcb_db_dir "$db")" && dolt sql "$@" ); }

_lcb_upload() {
    local file="$1" key="$2" url="$SPIRA_LC_BACKUP_URL" bucket rest region host sum
    case "$url" in
        file://*) mkdir -p "${url#file://}/$(dirname "$key")" && cp "$file" "${url#file://}/$key"; return ;;
        s3://*) ;;
        *) echo "lc-backup: unsupported destination scheme in $url" >&2; return 1 ;;
    esac
    rest="${url#s3://}"; bucket="${rest%%/*}"
    [ -n "${AWS_ACCESS_KEY_ID:-}" ] && [ -n "${AWS_SECRET_ACCESS_KEY:-}" ] \
        || { echo "lc-backup: AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY are not in the environment" >&2; return 1; }
    region="${AWS_DEFAULT_REGION:-${AWS_REGION:-}}"
    [ -n "$region" ] || { echo "lc-backup: AWS_DEFAULT_REGION is not in the environment" >&2; return 1; }
    sum="$(sha256sum "$file" | cut -d' ' -f1)"
    if [ -n "${AWS_ENDPOINT_URL_S3:-}" ]; then host="${AWS_ENDPOINT_URL_S3%/}/$bucket"
    else host="https://$bucket.s3.$region.amazonaws.com"; fi
    # batch-job: the snapshot upload is bounded by size, not by a round trip
    curl -sS --fail-with-body --max-time 1500 -X PUT -T "$file" \
        --aws-sigv4 "aws:amz:$region:s3" --user "$AWS_ACCESS_KEY_ID:$AWS_SECRET_ACCESS_KEY" \
        ${AWS_SESSION_TOKEN:+-H "x-amz-security-token: $AWS_SESSION_TOKEN"} \
        -H "x-amz-content-sha256: $sum" -H "x-amz-server-side-encryption: AES256" \
        "$host/$key" >/dev/null
}

_lcb_lock() { mkdir -p "$SPIRA_RUN/lc-backup/$1" && printf '%s/lc-backup/%s/.lock' "$SPIRA_RUN" "$1"; }

_lcb_run_db() {
    local db="$1" lock
    lock="$(_lcb_lock "$db")" || return 1
    ( flock -w "${SPIRA_LC_BACKUP_LOCK_WAIT_SECS:-3600}" 9 || { echo "lc-backup: $db: another run or drill holds the lock" >&2; exit 1; }
      _lcb_run_db_locked "$db" ) 9>"$lock"
}

_lcb_run_db_locked() {
    local db="$1" out head work stage tarball prefix stamp key ext
    _lcb_require_db "$db" || return 1
    out="$(_lcb_sql "$db" -q "CALL DOLT_COMMIT('-Am', 'lc-backup $(date -u +%Y-%m-%dT%H:%M:%SZ)')" 2>&1)" \
        || case "$out" in *"nothing to commit"*) ;; *) echo "lc-backup: commit failed: $out" >&2; return 1 ;; esac
    work="$SPIRA_RUN/lc-backup/$db"; stage="$work/stage"
    if command -v zstd >/dev/null 2>&1; then ext=zst; else ext=gz; fi
    tarball="$work/latest.tar.$ext"
    rm -rf "$stage" && mkdir -p "$stage" || return 1
    out="$(_lcb_sql "$db" -r csv -q "SELECT commit_hash FROM dolt_log LIMIT 1; CALL DOLT_BACKUP('sync-url', 'file://$stage')" 2>&1)" \
        || { echo "lc-backup: local snapshot of $db failed: $out" >&2; return 1; }
    head="$(printf '%s\n' "$out" | sed -n '2p')"
    case "$head" in *[!0-9a-v]*|"") head="" ;; esac
    [ -n "$head" ] || { echo "lc-backup: snapshot taken but cannot read $db's head commit" >&2; return 1; }
    tar -C "$stage" -caf "$tarball.tmp" --format=posix . 2>/dev/null && mv "$tarball.tmp" "$tarball" \
        || { rm -f "$tarball.tmp"; echo "lc-backup: cannot pack the snapshot" >&2; return 1; }
    find "$work" -maxdepth 1 -name 'latest.tar.*' ! -name "latest.tar.$ext" -delete
    rm -rf "$stage"
    prefix=""
    case "$SPIRA_LC_BACKUP_URL" in s3://*/*) prefix="${SPIRA_LC_BACKUP_URL#s3://*/}"; prefix="${prefix%/}"; [ -z "$prefix" ] || prefix="$prefix/" ;; esac
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    key="$prefix$db/$stamp.tar.$ext"
    _lcb_upload "$tarball" "$key" || { echo "lc-backup: upload of $key failed" >&2; return 1; }
    printf '%s %s\n' "$(date +%s)" "$head" > "$(_lcb_stamp "$db").tmp" && mv "$(_lcb_stamp "$db").tmp" "$(_lcb_stamp "$db")"
    echo "lc-backup: uploaded $db at $head as $key to $SPIRA_LC_BACKUP_URL"
}

cmd_run() {
    _lcb_refuse_unconfigured || return 1
    _lcb_refuse_same_disk || return 1
    local db rc=0 mlock="$SPIRA_RUN/maintenance/lc-backup.lock"
    mkdir -p "$SPIRA_RUN/maintenance" || return 1
    exec 8>"$mlock" && flock -n 8 || { echo "lc-backup: another run holds $mlock" >&2; return 1; }
    for db in $(_lcb_dbs); do _lcb_run_db "$db" || rc=1; done
    return "$rc"
}

cmd_explain() {
    [ "${SERVICE_RESULT:-}" != success ] || return 0
    local line
    line="$(grep '^lc-backup:' "$SPIRA_RUN/lc-backup.log" 2>/dev/null | tail -n1)"
    [ -n "$line" ] || line="lc-backup: failed with no error line in $SPIRA_RUN/lc-backup.log"
    printf '%s\n' "$line" >&2
    command -v systemd-cat >/dev/null 2>&1 && printf '%s\n' "$line" | systemd-cat -t lc-backup -p err
    return 0
}

cmd_explain_drill() {
    [ "${SERVICE_RESULT:-}" != success ] || return 0
    local db f msg=""
    for db in $(_lcb_dbs); do
        f="$(_lcb_verdict "$db")"
        [ -s "$f" ] && ! grep -q 'drill ok' "$f" && msg="$msg$(cat "$f")"$'\n'
    done
    [ -n "$msg" ] || msg="lc-backup: drill: failed with no verdict recorded (see $SPIRA_RUN/lc-backup.log)"
    printf '%s' "$msg" >&2
    command -v systemd-cat >/dev/null 2>&1 && printf '%s' "$msg" | systemd-cat -t lc-backup-drill -p err
    _lcb_report "restore drill FAILED — $(printf '%s' "$msg" | tr '\n' ';' | cut -c1-1500)" >/dev/null
    return 0
}

# _lcb_measure_db db [now] -> "ok <age>" | "stale <age>" | "never"; non-zero when the stamp cannot be read
_lcb_measure_db() {
    local db="$1" now="${2:-$(date +%s)}" t head age stamp
    stamp="$(_lcb_stamp "$db")"
    [ -r "$stamp" ] || { echo "never"; return 0; }
    read -r t head < "$stamp" 2>/dev/null
    case "$t" in ''|*[!0-9]*) return 1 ;; esac
    age=$((now - t))
    if [ "$age" -gt "$SPIRA_LC_BACKUP_MAX_AGE" ]; then echo "stale $age"; else echo "ok $age"; fi
}

# _lcb_measure [now] -> one "<db>: <state>" line per database; "no-destination" when unconfigured.
# Non-zero when any stamp cannot be read; the aggregate is ok only when every database is.
_lcb_measure() {
    local now="${1:-}" db out rc=0
    [ -n "$SPIRA_LC_BACKUP_URL" ] || { echo "no-destination"; return 0; }
    [ -n "$(_lcb_dbs)" ] || { echo "no-database"; return 0; }
    for db in $(_lcb_dbs); do
        out="$(_lcb_measure_db "$db" ${now:+"$now"})" || { echo "$db: unreadable"; rc=1; continue; }
        echo "$db: $out"
    done
    [ "$rc" = 0 ] || return 1
    return 0
}

_lcb_all_ok() { ! grep -qv '^[^ ]*: ok ' <<<"$1"; }

cmd_age() {
    local now=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --now) now="${2:?--now needs a value}"; shift 2 ;;
            *) echo "lc-backup.sh: age: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    local out
    out="$(_lcb_measure "$now")" || { printf '%s\n' "$out"; echo "lc-backup: unreadable stamp under $SPIRA_RUN" >&2; return 2; }
    printf '%s\n' "$out"
    _lcb_all_ok "$out"
}

_lcb_drill_db() {
    local db="$1" tarball="$2" lock
    if [ -z "$tarball" ]; then
        lock="$(_lcb_lock "$db")" || return 1
        ( flock -n 9 || { echo "lc-backup: drill: $db: a run holds the lock, deferred"; exit 0; }
          _lcb_drill_db_locked "$db" "" ) 9>"$lock"
    else
        _lcb_drill_db_locked "$db" "$tarball"
    fi
}

_lcb_verdict() { printf '%s/lc-backup/%s/drill.verdict' "$SPIRA_RUN" "$1"; }

_lcb_drill_fail() {
    local db="$1"; shift
    echo "lc-backup: drill: $*" >&2
    printf 'lc-backup: drill: %s\n' "$*" >> "$(_lcb_verdict "$db")"
    return 1
}

_lcb_drill_db_locked() {
    local db="$1" tarball="$2" stamp verdict
    stamp="$(_lcb_stamp "$db")"
    _lcb_require_db "$db" || return 1
    verdict="$(_lcb_verdict "$db")"
    mkdir -p "$(dirname "$verdict")" && : > "$verdict" || return 1
    [ -n "$tarball" ] || tarball="$(ls "$SPIRA_RUN/lc-backup/$db"/latest.tar.zst "$SPIRA_RUN/lc-backup/$db"/latest.tar.gz 2>/dev/null | head -1)"
    local t head scratch rc=0 tables tbl live restored live_head err
    [ -r "$tarball" ] || { _lcb_drill_fail "$db" "$db: no snapshot at $tarball"; return 1; }
    [ -r "$stamp" ] || { _lcb_drill_fail "$db" "$db: no stamp — nothing has been backed up"; return 1; }
    read -r t head < "$stamp"
    local need=$(( $(stat -c %s "$tarball") * 4 / 1024 )) have
    mkdir -p "$SPIRA_LC_BACKUP_DRILL_DIR" && scratch="$(mktemp -d "$SPIRA_LC_BACKUP_DRILL_DIR/$db.XXXXXX")" \
        || { _lcb_drill_fail "$db" "$db: cannot make a scratch dir under $SPIRA_LC_BACKUP_DRILL_DIR"; return 1; }
    have="$(df -Pk "$scratch" | awk 'NR==2{print $4}')"
    [ "${have:-0}" -ge "$need" ] \
        || { _lcb_drill_fail "$db" "$SPIRA_LC_BACKUP_DRILL_DIR has ${have:-0}KiB free, the restore needs ~${need}KiB"; rm -rf "$scratch"; return 1; }
    err="$(mkdir "$scratch/snap" && tar -C "$scratch/snap" -xaf "$tarball" 2>&1)" \
        || { _lcb_drill_fail "$db" "unpack: cannot unpack $tarball: $err"; rm -rf "$scratch"; return 1; }
    # batch-job: restoring a snapshot into the scratch dir
    ( cd "$scratch" && dolt backup restore "file://$scratch/snap" "$db" ) >"$scratch/restore.log" 2>&1 \
        || { _lcb_drill_fail "$db" "restore failed: $(cat "$scratch/restore.log")"; rm -rf "$scratch"; return 1; }
    # batch-job: reads of the restored scratch copy
    live_head="$( cd "$scratch/$db" && dolt sql -r csv -q "SELECT commit_hash FROM dolt_log LIMIT 1" 2>"$scratch/sql.err" | tail -n +2 )"
    [ "$live_head" = "$head" ] \
        || { _lcb_drill_fail "$db" "restored head ${live_head:-?} is not the stamped head $head: $(cat "$scratch/sql.err")"; rm -rf "$scratch"; return 1; }
    tables="$(_lcb_sql "$db" -r csv -q "SHOW FULL TABLES AS OF '$head' WHERE Table_type='BASE TABLE'" 2>"$scratch/sql.err" | awk -F, 'NR>1{print $1}')"
    [ -n "$tables" ] || { _lcb_drill_fail "$db" "list tables as of $head: none listed: $(cat "$scratch/sql.err")"; rm -rf "$scratch"; return 1; }
    for tbl in $tables; do
        live="$(_lcb_sql "$db" -r csv -q "SELECT COUNT(*) FROM \`$tbl\` AS OF '$head'" 2>"$scratch/live.err" | tail -n +2)"
        # batch-job: reads of the restored scratch copy
        restored="$( cd "$scratch/$db" && dolt sql -r csv -q "SELECT COUNT(*) FROM \`$tbl\` AS OF '$head'" 2>"$scratch/restored.err" | tail -n +2 )"
        if [ -z "$live" ] || [ "$live" != "$restored" ]; then
            _lcb_drill_fail "$db" "count $tbl: live ${live:-?} != restored ${restored:-?}: $(cat "$scratch/live.err" "$scratch/restored.err" 2>/dev/null | tr '\n' ' ')"; rc=1
        fi
    done
    rm -rf "$scratch"
    if [ "$rc" = 0 ]; then
        echo "lc-backup: drill ok — $db: $(printf '%s\n' "$tables" | wc -l) table(s) identical at $head" | tee "$verdict"
    fi
    return "$rc"
}

# Prints why the drill must not start now; prints nothing when it may. An unanswered probe defers.
_lcb_drill_busy() {
    local t0 ms db
    db="$(_lcb_dbs | head -1)"
    t0="$(date +%s%N)"
    _lcb_sql "$db" -q "SELECT 1" >/dev/null 2>&1 || { echo "live store did not answer a probe"; return; }
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    [ "$ms" -le "$SPIRA_LC_BACKUP_DRILL_BUDGET_MS" ] || echo "live store probe took ${ms}ms, over the ${SPIRA_LC_BACKUP_DRILL_BUDGET_MS}ms budget"
}

_lcb_drill_wait_quiet() {
    local deadline why
    deadline=$(( $(date +%s) + SPIRA_LC_BACKUP_DRILL_WAIT_SECS ))
    while why="$(_lcb_drill_busy)"; [ -n "$why" ]; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            echo "lc-backup: drill: DEFERRED past ${SPIRA_LC_BACKUP_DRILL_WAIT_SECS}s, never ran — $why"
            return 75
        fi
        echo "lc-backup: drill: deferring — $why"
        sleep "$SPIRA_LC_BACKUP_DRILL_POLL_SECS"
    done
}

cmd_drill() {
    local only="" tarball="" db rc=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --db) only="${2:?--db needs a value}"; shift 2 ;;
            *) tarball="$1"; shift ;;
        esac
    done
    [ -z "$tarball" ] || [ -n "$only" ] || only="$(_lcb_dbs | head -1)"
    _lcb_drill_wait_quiet || return $?
    for db in ${only:-$(_lcb_dbs)}; do _lcb_drill_db "$db" "$tarball" || rc=1; done
    return "$rc"
}

_lcb_report() {
    printf '%s\n' "$1"
    mail send operator \
        --from "LC Backup <lc-backup@spira>" \
        --subject "lc-backup: $1" \
        --kind alert <<MAILEOF >/dev/null || printf 'lc-backup: mail send failed for: %s\n' "$1" >&2
## Alert
$1
MAILEOF
}

_lcb_tick() {
    local out state prev=ok
    out="$(_lcb_measure)" || out="unreadable"
    if _lcb_all_ok "$out"; then state=ok; else state=red; fi
    out="$(printf '%s' "$out" | tr '\n' ';')"
    [ -r "$STATE_FILE" ] && read -r prev < "$STATE_FILE"
    if [ "$state" = red ] && [ "$prev" != red ]; then
        _lcb_report "RED: a store has no good backup within ${SPIRA_LC_BACKUP_MAX_AGE}s ($out) — losing its disk now loses its data and history"
    elif [ "$state" = ok ] && [ "$prev" = red ]; then
        _lcb_report "CLEAR: every store has a fresh backup again ($out)"
    fi
    mkdir -p "$(dirname "$STATE_FILE")"
    printf '%s\n' "$state" > "$STATE_FILE.tmp" && mv "$STATE_FILE.tmp" "$STATE_FILE"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "lc-backup: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local t iv age
    read -r t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=900 ;; esac
    age=$(( $(date +%s) - t ))
    if [ "$age" -gt $(( 3 * iv )) ]; then
        echo "lc-backup: last poll ${age}s ago (interval ${iv}s) — the watcher is hung or dead" >&2
        return 1
    fi
}

cmd_watch() {
    local interval=900 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "lc-backup.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        _lcb_tick
        mkdir -p "$(dirname "$HEALTH_FILE")"
        printf '%s %s\n' "$(date +%s)" "$interval" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        run)    cmd_run; exit $? ;;
        age)    shift; cmd_age "$@"; exit $? ;;
        drill)  shift; cmd_drill "$@"; exit $? ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        explain) cmd_explain; exit $? ;;
        explain-drill) cmd_explain_drill; exit $? ;;
        *) echo "usage: lc-backup.sh run | age [--now EPOCH] | drill [--db NAME] [TARBALL] | watch [--interval S] [--ticks N] | health | explain | explain-drill" >&2
           exit 2 ;;
    esac
fi
