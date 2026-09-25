#!/usr/bin/env bash
# spira-mail-deliver.sh — wake registered readers on new mail, and keep waking them.
#
# Reads SPIRA_MAIL_READERS (<mailbox>=<wake command>, one per line, # comments).
# Spawns one inotifywait watcher per registered mailbox, plus an immediate catch-up pass
# for mail already unread when the daemon (re)starts. On arrival: waits SPIRA_MAIL_SETTLE
# for a burst to finish, then wakes and keeps waking on SPIRA_MAIL_WAKE_BACKOFF (last step
# repeats) for as long as the mailbox has unread mail, logging every attempt. Reading is
# the ack, not the wake — a mailbox with nothing unread is never retried, and mail.sh's own
# new/ -> cur/ move is what makes a wake for an already-read message impossible to send
# twice. Unregistered mailboxes are never watched.
#
# NOTE: SPIRA_WAKE types text into the concierge's tmux pane. A locally
# attached operator who is mid-keystroke when the wake fires may see the
# injected text mix into their half-written line. Typing from the phone is
# not affected.

set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

# _wake_loop <mailbox> <wake_cmd> — resend the wake on SPIRA_MAIL_WAKE_BACKOFF for as long
# as <mailbox>/new is non-empty; returns as soon as it is empty. Guarded by flock in _kick
# so a mailbox never has two loops retrying over each other.
_wake_loop() {
    local mailbox="$1" wake_cmd="$2"
    local dir="$SPIRA_MAIL/$mailbox/new"
    local -a backoff
    read -r -a backoff <<< "${SPIRA_MAIL_WAKE_BACKOFF:-15 60 300 900}"
    local attempt=0
    while true; do
        local count
        count="$(ls "$dir" 2>/dev/null | wc -l | tr -d ' ')"
        [ "${count:-0}" -gt 0 ] || return 0
        local age
        age="$("$SPIRA_HOME/mail.sh" unread-age "$mailbox" 2>/dev/null)"
        case "$age" in ''|*[!0-9]*) age=0 ;; esac
        attempt=$((attempt+1))
        # shellcheck disable=SC2086  # wake_cmd may be multi-word
        if $wake_cmd "You have $count unread messages in $mailbox, oldest ${age}s old — mail.sh list $mailbox --unread" 2>/dev/null; then
            printf '%s spira-mail-deliver: %s: wake sent (attempt %d, %d unread, oldest %ss)\n' \
                "$(date -u +%FT%TZ)" "$mailbox" "$attempt" "$count" "$age"
        else
            printf '%s spira-mail-deliver: %s: wake failed (attempt %d, reader not running?)\n' \
                "$(date -u +%FT%TZ)" "$mailbox" "$attempt" >&2
        fi
        local idx=$((attempt-1))
        [ "$idx" -ge "${#backoff[@]}" ] && idx=$(( ${#backoff[@]} - 1 ))
        sleep "${backoff[idx]}"
    done
}

# _kick <mailbox> <wake_cmd> — start a wake loop unless one is already retrying this
# mailbox. flock -n makes the second caller a no-op rather than a second loop stacked on
# the first: the running loop already recomputes count and age every attempt, so mail that
# arrives mid-backoff is picked up on its next cycle without a second loop to coordinate.
_kick() {
    local mailbox="$1" wake_cmd="$2"
    local lock="$SPIRA_RUN/mail-deliver-$mailbox.lock"
    (
        flock -n 9 || exit 0
        _wake_loop "$mailbox" "$wake_cmd"
    ) 9>"$lock" &
}

_watch() {
    local mailbox="$1" wake_cmd="$2"
    local dir="$SPIRA_MAIL/$mailbox/new"
    mkdir -p "$dir"
    # Catch-up: mail already unread when this daemon (re)starts gets no inotify event.
    _kick "$mailbox" "$wake_cmd"
    inotifywait -m -q -e close_write -e moved_to "$dir" 2>/dev/null | \
    while IFS= read -r _event; do
        sleep "${SPIRA_MAIL_SETTLE:-2}"
        # Drain events that piled up during the settle period.
        while IFS= read -r -t 0.1 _burst 2>/dev/null; do :; done
        _kick "$mailbox" "$wake_cmd"
    done
}

# SOURCEABLE, AND SILENT WHEN IT IS — same convention as watchd.sh. A test drives _wake_loop
# and _kick directly, against a stub wake command and a real Maildir, without inotifywait or
# a spawned daemon in the loop; sourcing must not itself start watching mailboxes.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then

_pids=()
_cleanup() {
    for _p in "${_pids[@]:-}"; do
        kill -- -"$_p" 2>/dev/null || kill "$_p" 2>/dev/null || true
    done
}
trap _cleanup EXIT INT TERM

if ! command -v inotifywait >/dev/null 2>&1; then
    printf '%s spira-mail-deliver: inotifywait not found — install inotify-tools\n' \
        "$(date -u +%FT%TZ)" >&2
    exit 1
fi

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

fi
