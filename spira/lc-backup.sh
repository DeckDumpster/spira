#!/usr/bin/env bash
#
# lc-backup.sh — off-box backup of the lifecycle store, its age alarm and its restore drill.
#
#   lc-backup.sh run                 snapshot the store locally, tar it, upload ONE object, stamp it
#   lc-backup.sh age [--now EPOCH]   exit 0 when the newest good backup is younger than SPIRA_LC_BACKUP_MAX_AGE
#   lc-backup.sh drill [TARBALL]     restore a snapshot into a scratch dir; row counts must equal live
#   lc-backup.sh watch [--interval S] [--ticks N]   watchd daemon: mails the crossing into and out of stale
#   lc-backup.sh health              exit non-zero when the watch loop has stopped polling
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
# A stamp is written only after the upload exits clean, and carries the commit it holds. The
# drill restores that tarball and compares every table's count with live AS OF the same commit.
#
# No destination, an unreadable stamp or an age beyond the limit is red, never a skip — a
# store that was never backed up must look different from one that is.

# covers: spira/lc-backup.sh spira/watchers spira/conf.d/SPIRA_LC_BACKUP_* systemd/lc-backup.service systemd/lc-backup.timer
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

STAMP="$SPIRA_RUN/lc-backup.ok"
STATE_FILE="$SPIRA_RUN/watchd/lc-backup.state"
HEALTH_FILE="$SPIRA_RUN/watchd/lc-backup.health"

_lcb_db_dir() { printf '%s/%s' "$SPIRA_DOLT_DATA" "$SPIRA_LC_BACKUP_DB"; }

_lcb_refuse_unconfigured() {
    [ -n "$SPIRA_LC_BACKUP_URL" ] || { echo "lc-backup: SPIRA_LC_BACKUP_URL is empty — the lifecycle store has no backup destination" >&2; return 1; }
    [ -n "$SPIRA_DOLT_DATA" ] && [ -d "$(_lcb_db_dir)/.dolt" ] \
        || { echo "lc-backup: no dolt database at $(_lcb_db_dir)" >&2; return 1; }
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

_lcb_sql() { ( cd "$(_lcb_db_dir)" && dolt sql "$@" ); }

_lcb_head() { _lcb_sql -r csv -q "SELECT commit_hash FROM dolt_log LIMIT 1" 2>/dev/null | tail -n +2; }

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
    curl -sS --fail-with-body --max-time 1500 -X PUT -T "$file" \
        --aws-sigv4 "aws:amz:$region:s3" --user "$AWS_ACCESS_KEY_ID:$AWS_SECRET_ACCESS_KEY" \
        ${AWS_SESSION_TOKEN:+-H "x-amz-security-token: $AWS_SESSION_TOKEN"} \
        -H "x-amz-content-sha256: $sum" -H "x-amz-server-side-encryption: AES256" \
        "$host/$key" >/dev/null
}

cmd_run() {
    _lcb_refuse_unconfigured || return 1
    _lcb_refuse_same_disk || return 1
    local out head work stage tarball prefix stamp key
    out="$(_lcb_sql -q "CALL DOLT_COMMIT('-Am', 'lc-backup $(date -u +%Y-%m-%dT%H:%M:%SZ)')" 2>&1)" \
        || case "$out" in *"nothing to commit"*) ;; *) echo "lc-backup: commit failed: $out" >&2; return 1 ;; esac
    work="$SPIRA_RUN/lc-backup"; stage="$work/stage"; tarball="$work/latest.tar.zst"
    rm -rf "$stage" && mkdir -p "$stage" || return 1
    out="$(_lcb_sql -q "CALL DOLT_BACKUP('sync-url', 'file://$stage')" 2>&1)" \
        || { echo "lc-backup: local snapshot failed: $out" >&2; return 1; }
    head="$(_lcb_head)"
    [ -n "$head" ] || { echo "lc-backup: snapshot taken but cannot read the store's head commit" >&2; return 1; }
    tar -C "$stage" -cf - . | zstd -q -f -o "$tarball.tmp" && mv "$tarball.tmp" "$tarball" \
        || { echo "lc-backup: cannot pack the snapshot" >&2; return 1; }
    rm -rf "$stage"
    prefix="${SPIRA_LC_BACKUP_URL#*://}"; prefix="${prefix#*/}"; [ "$prefix" = "${SPIRA_LC_BACKUP_URL#*://}" ] && prefix=""
    prefix="${prefix%/}"; [ -z "$prefix" ] || prefix="$prefix/"
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    key="$prefix$SPIRA_LC_BACKUP_DB/$stamp.tar.zst"
    _lcb_upload "$tarball" "$key" || { echo "lc-backup: upload of $key failed" >&2; return 1; }
    printf '%s %s\n' "$(date +%s)" "$head" > "$STAMP.tmp" && mv "$STAMP.tmp" "$STAMP"
    echo "lc-backup: uploaded $SPIRA_LC_BACKUP_DB at $head as $key to $SPIRA_LC_BACKUP_URL"
}

# _lcb_measure [now] -> "ok <age>" | "stale <age>" | "never"; non-zero when the stamp cannot be read
_lcb_measure() {
    local now="${1:-$(date +%s)}" t head age
    [ -n "$SPIRA_LC_BACKUP_URL" ] || { echo "no-destination"; return 0; }
    [ -r "$STAMP" ] || { echo "never"; return 0; }
    read -r t head < "$STAMP" 2>/dev/null
    case "$t" in ''|*[!0-9]*) return 1 ;; esac
    age=$((now - t))
    if [ "$age" -gt "$SPIRA_LC_BACKUP_MAX_AGE" ]; then echo "stale $age"; else echo "ok $age"; fi
}

cmd_age() {
    local now=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --now) now="${2:?--now needs a value}"; shift 2 ;;
            *) echo "lc-backup.sh: age: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    local out
    out="$(_lcb_measure "$now")" || { echo "lc-backup: unreadable stamp $STAMP" >&2; return 2; }
    printf '%s\n' "$out"
    [ "${out%% *}" = ok ]
}

cmd_drill() {
    [ -n "$SPIRA_DOLT_DATA" ] && [ -d "$(_lcb_db_dir)/.dolt" ] \
        || { echo "lc-backup: no dolt database at $(_lcb_db_dir)" >&2; return 1; }
    local tarball="${1:-$SPIRA_RUN/lc-backup/latest.tar.zst}"
    local t head scratch rc=0 tables tbl live restored live_head
    [ -r "$tarball" ] || { echo "lc-backup: drill: no snapshot at $tarball" >&2; return 1; }
    [ -r "$STAMP" ] || { echo "lc-backup: drill: no stamp — nothing has been backed up" >&2; return 1; }
    read -r t head < "$STAMP"
    scratch="$(mktemp -d "${TMPDIR:-/tmp}/lc-backup-drill.XXXXXX")" || return 1
    mkdir "$scratch/snap" && zstd -dc -q "$tarball" | tar -C "$scratch/snap" -xf - \
        || { echo "lc-backup: drill: cannot unpack $tarball" >&2; rm -rf "$scratch"; return 1; }
    ( cd "$scratch" && dolt backup restore "file://$scratch/snap" "$SPIRA_LC_BACKUP_DB" ) >"$scratch/restore.log" 2>&1 \
        || { echo "lc-backup: drill: restore failed: $(cat "$scratch/restore.log")" >&2; rm -rf "$scratch"; return 1; }
    live_head="$( cd "$scratch/$SPIRA_LC_BACKUP_DB" && dolt sql -r csv -q "SELECT commit_hash FROM dolt_log LIMIT 1" | tail -n +2 )"
    [ "$live_head" = "$head" ] \
        || { echo "lc-backup: drill: restored head ${live_head:-?} is not the stamped head $head" >&2; rm -rf "$scratch"; return 1; }
    tables="$(_lcb_sql -r csv -q "SELECT table_name FROM information_schema.tables WHERE table_schema='$SPIRA_LC_BACKUP_DB' AND table_type='BASE TABLE' ORDER BY 1" | tail -n +2)"
    [ -n "$tables" ] || { echo "lc-backup: drill: live store lists no tables" >&2; rm -rf "$scratch"; return 1; }
    for tbl in $tables; do
        live="$(_lcb_sql -r csv -q "SELECT COUNT(*) FROM \`$tbl\` AS OF '$head'" | tail -n +2)"
        restored="$( cd "$scratch/$SPIRA_LC_BACKUP_DB" && dolt sql -r csv -q "SELECT COUNT(*) FROM \`$tbl\`" | tail -n +2 )"
        if [ -z "$live" ] || [ "$live" != "$restored" ]; then
            echo "lc-backup: drill: $tbl live ${live:-?} != restored ${restored:-?}" >&2; rc=1
        fi
    done
    rm -rf "$scratch"
    [ "$rc" = 0 ] && echo "lc-backup: drill ok — $(printf '%s\n' "$tables" | wc -l) table(s) identical at $head"
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
    state="${out%% *}"
    [ "$state" = ok ] || state=red
    [ -r "$STATE_FILE" ] && read -r prev < "$STATE_FILE"
    if [ "$state" = red ] && [ "$prev" != red ]; then
        _lcb_report "RED: the lifecycle store has no good backup within ${SPIRA_LC_BACKUP_MAX_AGE}s ($out) — losing its disk now loses every bead's state and history"
    elif [ "$state" = ok ] && [ "$prev" = red ]; then
        _lcb_report "CLEAR: the lifecycle store has a fresh backup again ($out)"
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
        *) echo "usage: lc-backup.sh run | age [--now EPOCH] | drill [TARBALL] | watch [--interval S] [--ticks N] | health" >&2
           exit 2 ;;
    esac
fi
