#!/usr/bin/env bash
#
# warden-trigger.sh — file the scheduled sweep bead that wakes the warden persona.
# At most one open sweep at a time: a lane with one slot and two sweeps only queues noise.
#
# covers: spira/warden-trigger.sh spira/chamber/warden.fayth spira/chamber/warden.md systemd/spira-warden.*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

BD="${SPIRA_BD:-bd}"
DB="${SPIRA_DB:-.}"

log() { printf '%s warden-trigger: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }

LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_WARDEN_LABEL}"

exec 9>"${SPIRA_RUN:-.}/warden-trigger.lock"
if ! flock --nonblock 9; then
    log "another instance holds the lock — skipping"
    exit 0
fi

if ! open_count="$(spira_open_trigger_count "$LABELS")"; then
    log "cannot count open triggers (spira-lc or bd unreadable) — refusing to file, retrying next pass"
    exit 0
fi
if [ "${open_count:-0}" -gt 0 ] 2>/dev/null; then
    log "sweep already open ($open_count bead(s) with labels [$LABELS]) — skipping"
    exit 0
fi

if timeout 5 "$BD" -C "$DB" create \
    "Warden sweep — landed work in force, anomalies attributed" \
    --type task \
    --label "$LABELS,delivers:note:${SPIRA_RUN}/warden.log" \
    --priority 3 \
    --description "Scheduled sweep: the warden persona judges whether landed fixes are in force (filing a follow-up for any that are not) and attributes anomalies no probe could. See spira/chamber/warden.md." \
; then
    log "warden sweep bead filed (labels: $LABELS)"
else
    log "ERROR: failed to file warden sweep bead"
    exit 1
fi
