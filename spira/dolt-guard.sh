#!/usr/bin/env bash
#
# dolt-guard.sh — the only way a watcher signals the Dolt server.
#
#   dolt-guard.sh busy                          print the declared maintenance operation running; exit 0 when one is
#   dolt-guard.sh signal --reason TEXT [-- CMD...]   signal the server unless an operation is running
#
# An operation is running when its lock under $SPIRA_RUN/maintenance or lc-backup's per-database
# lock is held, or one of SPIRA_DOLT_MAINTENANCE_UNITS is active or activating. Load is never read:
# a backup's own load looks like a runaway.
#
# covers: spira/dolt-guard.sh spira/lc-backup.sh spira/conf.d/SPIRA_DOLT_MAINTENANCE_UNITS
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

_dg_lock_held() { [ -e "$1" ] && ! ( flock -n 9 ) 9<"$1" 2>/dev/null; }

dg_busy() {
    local f name u st
    for f in "$SPIRA_RUN"/maintenance/*.lock "$SPIRA_RUN"/lc-backup/*/.lock; do
        [ -e "$f" ] || continue
        _dg_lock_held "$f" || continue
        name="$(basename "$f" .lock)"; [ "$name" != .lock ] || name="lc-backup"
        printf '%s\n' "$name"; return 0
    done
    for u in $SPIRA_DOLT_MAINTENANCE_UNITS; do
        st="$(systemctl --user is-active "$u" 2>/dev/null)"
        case "$st" in active|activating|reloading) printf '%s\n' "${u%.service}"; return 0 ;; esac
    done
    return 1
}

dg_signal() {
    local reason="" op
    while [ $# -gt 0 ]; do
        case "$1" in
            --reason) reason="${2:?--reason needs a value}"; shift 2 ;;
            --) shift; break ;;
            *) echo "dolt-guard: signal: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    [ -n "$reason" ] || { echo "dolt-guard: signal needs --reason (the query or connection judged runaway)" >&2; return 2; }
    [ $# -gt 0 ] || set -- systemctl --user kill --signal=SIGTERM dolt-beads.service
    if op="$(dg_busy)"; then
        echo "dolt-guard: deferring: $op running (would have signalled for: $reason)"
        return 0
    fi
    echo "dolt-guard: signalling dolt: $reason"
    "$@"
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        busy)   dg_busy; exit $? ;;
        signal) shift; dg_signal "$@"; exit $? ;;
        *) echo "usage: dolt-guard.sh busy | signal --reason TEXT [-- CMD...]" >&2; exit 2 ;;
    esac
fi
