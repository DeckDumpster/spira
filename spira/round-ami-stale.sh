#!/usr/bin/env bash
#
# round-ami-stale.sh — says when no round AMI carries the running release's recipe hash, as a watchd daemon.
#
#   round-ami-stale.sh pass                              one check
#   round-ami-stale.sh watch [--interval S] [--ticks N]  loop forever (or N times)
#   round-ami-stale.sh health                            non-zero when the loop stopped polling
#
# Asks `round-vm ami-check`; each distinct stale line is said once, and a match clears it so a
# later mismatch is said again. An unreadable image list is not "fresh" and is said as such.
# Silent while SPIRA_ROUND_VM_SPILL is not ec2, ec2-always or ec2-only.
#
# covers: spira/round-ami-stale.sh spira/watchers round-vm/src/ec2.rs round-vm/src/cli.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

HEALTH_FILE="$SPIRA_RUN/watchd/round-ami-stale.health"
SAID="$SPIRA_RUN/watchd/round-ami-stale.said"

_ras_say() {
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:round-ami-stale] %s\n' "$(date -u +%FT%TZ)" "$1" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null
    return 0
}

cmd_pass() {
    case "${SPIRA_ROUND_VM_SPILL:-}" in ec2|ec2-always|ec2-only) ;; *) return 0 ;; esac
    local out rc line
    out="$(timeout 60 round-vm ami-check 2>&1)"; rc=$? # batch-job: AWS image listing, polled off the hot path
    case "$rc" in
        0) rm -f "$SAID"; return 0 ;;
        1) line="$(printf '%s' "$out" | tail -n 1)" ;;
        *) line="round AMI check could not read the images: $(printf '%s' "$out" | tail -n 1)" ;;
    esac
    grep -qxF -- "$line" "$SAID" 2>/dev/null && return 0
    _ras_say "$line; EC2 rounds refuse the image until the Round AMI workflow builds one"
    mkdir -p "$(dirname "$SAID")" && printf '%s\n' "$line" > "$SAID"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "round-ami-stale: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=300 ;; esac
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "round-ami-stale: last poll ${age}s ago (interval ${iv}s) — hung or dead" >&2; return 1; }
}

cmd_watch() {
    local interval=300 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "round-ami-stale.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        cmd_pass
        mkdir -p "$(dirname "$HEALTH_FILE")"
        printf 'ok %s %s\n' "$(date +%s)" "$interval" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        pass)   cmd_pass ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: round-ami-stale.sh pass | watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
