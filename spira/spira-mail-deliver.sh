#!/usr/bin/env bash
# spira-mail-deliver.sh — wake registered readers on new mail.
#
# Reads SPIRA_MAIL_READERS (<mailbox>=<wake command>, one per line, # comments).
# Spawns one inotifywait watcher per registered mailbox. On arrival: waits
# SPIRA_MAIL_SETTLE for a burst to finish, then calls the wake with:
#   "You have N unread messages — mail.sh list <mailbox> --unread"
# A failed wake logs one line; the mail stays unread. Unregistered mailboxes
# are never watched.
#
# NOTE: SPIRA_WAKE types text into the concierge's tmux pane. A locally
# attached operator who is mid-keystroke when the wake fires may see the
# injected text mix into their half-written line. Typing from the phone is
# not affected.

set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

if ! command -v inotifywait >/dev/null 2>&1; then
    printf '%s spira-mail-deliver: inotifywait not found — install inotify-tools\n' \
        "$(date -u +%FT%TZ)" >&2
    exit 1
fi

_pids=()
_cleanup() {
    for _p in "${_pids[@]:-}"; do
        kill -- -"$_p" 2>/dev/null || kill "$_p" 2>/dev/null || true
    done
}
trap _cleanup EXIT INT TERM

_watch() {
    local mailbox="$1" wake_cmd="$2"
    local dir="$SPIRA_MAIL/$mailbox/new"
    mkdir -p "$dir"
    inotifywait -m -q -e close_write -e moved_to "$dir" 2>/dev/null | \
    while IFS= read -r _event; do
        sleep "${SPIRA_MAIL_SETTLE:-2}"
        # Drain events that piled up during the settle period.
        while IFS= read -r -t 0.1 _burst 2>/dev/null; do :; done
        local count
        count="$(ls "$dir" 2>/dev/null | wc -l | tr -d ' ')"
        # shellcheck disable=SC2086  # wake_cmd may be multi-word
        if ! $wake_cmd "You have $count unread messages — mail.sh list $mailbox --unread" 2>/dev/null; then
            printf '%s spira-mail-deliver: %s: wake failed (reader not running?)\n' \
                "$(date -u +%FT%TZ)" "$mailbox" >&2
        fi
    done
}

_count=0
while IFS= read -r _line; do
    case "$_line" in ''|'#'*) continue ;; esac
    _mb="${_line%%=*}"
    _wk="${_line#*=}"
    if [ -z "$_mb" ] || [ -z "$_wk" ]; then continue; fi
    (
        _watch "$_mb" "$_wk"
    ) &
    _pids+=($!)
    _count=$((_count+1))
done <<< "${SPIRA_MAIL_READERS:-}"

if [ "$_count" -eq 0 ]; then
    printf '%s spira-mail-deliver: SPIRA_MAIL_READERS is empty — nothing to watch\n' \
        "$(date -u +%FT%TZ)" >&2
    # No watchers: stay active so systemd does not restart us in a tight loop.
    # A restart would re-read SPIRA_MAIL_READERS, so changes take effect.
    while true; do sleep 60; done
fi

wait
