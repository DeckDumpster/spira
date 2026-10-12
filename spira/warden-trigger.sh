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

ATTEMPTS="${SPIRA_WARDEN_ATTEMPTS:-4}"
BACKOFF="${SPIRA_WARDEN_BACKOFF:-20}"
CALL_TIMEOUT="${SPIRA_WARDEN_CALL_TIMEOUT:-30}"

# Counting is retried with growing backoff: a starved dolt server times the read out, and
# one failed pass otherwise costs a whole timer period.
count_open() {
    local n=1
    while [ "$n" -le "$ATTEMPTS" ]; do
        if open_count="$(spira_open_trigger_count "$LABELS")"; then return 0; fi
        log "cannot count open triggers (attempt $n/$ATTEMPTS)"
        [ "$n" -lt "$ATTEMPTS" ] && sleep $((BACKOFF * n))
        n=$((n + 1))
    done
    return 1
}

if ! count_open; then
    log "cannot count open triggers (spira-lc or bd unreadable) — refusing to file, retrying next pass"
    exit 0
fi
if [ "${open_count:-0}" -gt 0 ] 2>/dev/null; then
    log "sweep already open ($open_count bead(s) with labels [$LABELS]) — skipping"
    exit 0
fi

LC="${SPIRA_LC_BIN:-spira-lc}"

# A failed create may still have committed (indeterminate after connection loss), so each
# retry first re-counts: a sweep that appeared is not filed twice.
file_sweep() {
    timeout "$CALL_TIMEOUT" "$BD" -C "$DB" create \
        "Warden sweep — landed work in force, anomalies attributed" \
        --type task \
        --silent \
        --label "$LABELS,delivers:note:${SPIRA_RUN}/warden.log" \
        --priority 3 \
        --description "Scheduled sweep: the warden persona judges whether landed fixes are in force (filing a follow-up for any that are not) and attributes anomalies no probe could. See spira/chamber/warden.md."
}

id=""
n=1
while [ "$n" -le "$ATTEMPTS" ]; do
    if id="$(file_sweep)" && [ -n "$id" ]; then break; fi
    id=""
    log "failed to file warden sweep bead (attempt $n/$ATTEMPTS)"
    [ "$n" -lt "$ATTEMPTS" ] || break
    sleep $((BACKOFF * n))
    if count_open && [ "${open_count:-0}" -gt 0 ] 2>/dev/null; then
        log "a sweep appeared after the failed create ($open_count bead(s)) — not filing again"
        exit 0
    fi
    n=$((n + 1))
done
if [ -z "$id" ]; then
    log "ERROR: failed to file warden sweep bead"
    exit 1
fi
id="$(printf '%s\n' "$id" | tail -n 1)"

# A bead with no lifecycle row is unclaimable; the row is created in the same pass as the filing.
if ! timeout "$CALL_TIMEOUT" "$LC" create-bead "$id" --priority 3 >/dev/null 2>&1; then
    log "ERROR: warden sweep $id filed but its lifecycle row was not created — unclaimable until: spira-lc create-bead $id"
    exit 1
fi
log "warden sweep bead $id filed with lifecycle row (labels: $LABELS)"
