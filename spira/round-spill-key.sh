#!/usr/bin/env bash
#
# round-spill-key.sh — warns before the round-spill tailscale auth key expires, as a watchd daemon.
#
#   round-spill-key.sh pass                              one check
#   round-spill-key.sh watch [--interval S] [--ticks N]  loop forever (or N times)
#   round-spill-key.sh health                            non-zero when the loop stopped polling
#
# The key file is SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE; its expiry (epoch seconds) is recorded
# beside it in <file>.expires, written by whoever pastes the key. A spilled pass cannot join the
# tailnet with an expired key, so the warning comes SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS before.
# Each pass also runs `round-vm ec2-probe --dry-run`, so a launch path that stops authorising alarms
# within one interval. Silent while SPIRA_ROUND_VM_SPILL is not ec2, ec2-always or ec2-only. Each distinct condition is said once.
#
# covers: spira/round-spill-key.sh spira/watchers spira/conf.d/SPIRA_ROUND_VM_EC2_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

HEALTH_FILE="$SPIRA_RUN/watchd/round-spill-key.health"

_rsk_say() {
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:round-spill-key] %s\n' "$(date -u +%FT%TZ)" "$1" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null
    return 0
}

_rsk_once() {
    local mark="$SPIRA_RUN/watchd/round-spill-key.said"
    grep -qxF -- "$1" "$mark" 2>/dev/null && return 0
    _rsk_say "$2"
    mkdir -p "$(dirname "$mark")" && printf '%s\n' "$1" >> "$mark"
}

_rsk_probe() {
    local out mark="$SPIRA_RUN/watchd/round-spill-key.said"
    if out="$(timeout 120 round-vm ec2-probe --dry-run 2>&1)"; then
        [ -f "$mark" ] && { grep -v '^probe-' "$mark" > "$mark.tmp"; mv "$mark.tmp" "$mark"; }
        return 0
    fi
    out="$(printf '%s' "$out" | tail -n 3 | tr '\n' ' ')"
    _rsk_once "probe-$(printf '%s' "$out" | cksum | cut -d' ' -f1)" "ROUND SPILL LAUNCH: round-vm ec2-probe --dry-run failed; EC2 rounds cannot launch: $out"
}

cmd_pass() {
    case "${SPIRA_ROUND_VM_SPILL:-}" in ec2|ec2-always|ec2-only) ;; *) return 0 ;; esac
    _rsk_probe
    local file="$SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE" exp now days left
    [ -n "$file" ] || { _rsk_once "nofile" "ROUND SPILL KEY: SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE is not set"; return 0; }
    exp="$(tr -d '[:space:]' < "$file.expires" 2>/dev/null)"
    case "$exp" in
        ''|*[!0-9]*) _rsk_once "unrecorded" "ROUND SPILL KEY: no expiry recorded in $file.expires; record the key's expiry (epoch seconds) beside it"; return 0 ;;
    esac
    now="$(date +%s)"
    days="${SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS:-14}"
    left=$(( exp - now ))
    if [ "$left" -le 0 ]; then
        _rsk_once "expired-$exp" "ROUND SPILL KEY: the tailscale auth key in $file EXPIRED; spilled passes cannot join the tailnet until it is replaced"
    elif [ "$left" -le $(( days * 86400 )) ]; then
        _rsk_once "warn-$exp" "ROUND SPILL KEY: the tailscale auth key in $file expires in $(( left / 86400 )) days; replace it and record the new expiry in $file.expires"
    fi
}

_rsk_write_health() {
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$1" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "round-spill-key: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=3600 ;; esac
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "round-spill-key: last poll ${age}s ago (interval ${iv}s) — hung or dead" >&2; return 1; }
}

cmd_watch() {
    local interval=3600 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "round-spill-key.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        cmd_pass
        _rsk_write_health "$interval"
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
        *) echo "usage: round-spill-key.sh pass | watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
