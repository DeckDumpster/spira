#!/usr/bin/env bash
# spira-mail-deliver.sh — wake registered readers on new mail, and keep waking them.
#
# Reads SPIRA_MAIL_READERS (<mailbox>=<wake command>, one per line, # comments).
# Spawns one inotifywait watcher per registered mailbox, plus an immediate catch-up pass
# for mail already unread when the daemon (re)starts. On arrival: waits SPIRA_MAIL_SETTLE
# for a burst to finish — SPIRA_MAIL_SETTLE_EVENT instead, the moment any waiting mail is a
# machine `--kind event`, so a live PR/watcher event never sits through the window that
# exists to batch the operator's own replies — then wakes and keeps waking on
# SPIRA_MAIL_WAKE_BACKOFF (last step repeats) for as long as the mailbox has unread mail,
# logging every attempt. Reading is
# the ack, not the wake — a mailbox with nothing unread is never retried, and mail's own
# new/ -> cur/ move is what makes a wake for an already-read message impossible to send
# twice. Unregistered mailboxes are never watched.
#
#   spira-mail-deliver.sh health   exit 0 if every registered mailbox has a running watch lease —
#                                  watchd's health probe for this daemon's `extern` row.
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
        age="$(mail unread-age "$mailbox" 2>/dev/null)"
        case "$age" in ''|*[!0-9]*) age=0 ;; esac
        attempt=$((attempt+1))
        local wake_err
        # shellcheck disable=SC2086  # wake_cmd may be multi-word
        if wake_err="$($wake_cmd "You have $count unread messages in $mailbox, oldest ${age}s old — mail list $mailbox --unread" 2>&1)"; then
            printf '%s spira-mail-deliver: %s: wake sent (attempt %d, %d unread, oldest %ss)\n' \
                "$(date -u +%FT%TZ)" "$mailbox" "$attempt" "$count" "$age"
        else
            # THE REASON, NOT JUST THE FACT. A wake command that refuses says why on stderr
            # (e.g. concierge.sh wake: the pane exists but its process has exited) — losing
            # that behind 2>/dev/null left every failure reading as "reader not running?"
            # whether or not that was true.
            printf '%s spira-mail-deliver: %s: wake failed (attempt %d): %s\n' \
                "$(date -u +%FT%TZ)" "$mailbox" "$attempt" "${wake_err:-reader not running?}" >&2
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

# _msg_kind <file> -> its X-Spira-Kind header value, or empty.
_msg_kind() {
    awk '/^[[:space:]]*$/ { exit }
         tolower($0) ~ /^x-spira-kind:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }' "$1" 2>/dev/null
}

# _dir_has_event_kind <dir> -> true if any file currently in <dir> is `--kind event`.
_dir_has_event_kind() {
    local d="$1" f
    for f in "$d"/*; do
        [ -f "$f" ] || continue
        [ "$(_msg_kind "$f")" = event ] && return 0
    done
    return 1
}

# _settle_wait <dir> — LIVENESS (UC-operator-channel-38): a machine event (`--kind event` —
# PR transitions, watcher events) must not sit through SPIRA_MAIL_SETTLE, the window that
# exists to batch the OPERATOR's own replies landing in this same mailbox. Polls in 1s ticks
# rather than sleeping the whole reply window up front, so an event that arrives mid-window
# is seen and cuts the wait to SPIRA_MAIL_SETTLE_EVENT immediately, instead of queuing behind
# whatever reply-settle sleep was already running.
#
# CHOICE MADE: an event arriving mid-window also flushes whatever reply is already waiting,
# one tick early — the simpler design, over tracking two independent timers per mailbox for
# a wake that is the same generic "you have unread mail" ping either way.
_settle_wait() {
    local dir="$1"
    local budget="${SPIRA_MAIL_SETTLE:-2}" event_settle="${SPIRA_MAIL_SETTLE_EVENT:-0}"
    local waited=0
    while [ "$waited" -lt "$budget" ]; do
        if _dir_has_event_kind "$dir"; then
            sleep "$event_settle"
            return
        fi
        sleep 1
        waited=$((waited + 1))
    done
}

_WATCH_BEAT=15
_WATCH_LEASE=60

_watch_lease_file() { printf '%s/mail-deliver-%s.lease' "$SPIRA_RUN" "$1"; }

# The watcher's only liveness signal: a deadline it keeps ahead of the clock while it lives.
_renew_watch_lease() {
    local f; f="$(_watch_lease_file "$1")"
    printf '%s\n' "$(( $(date +%s) + _WATCH_LEASE ))" > "$f.tmp" && mv -f "$f.tmp" "$f"
}

_watch() {
    local mailbox="$1" wake_cmd="$2"
    local dir="$SPIRA_MAIL/$mailbox/new"
    mkdir -p "$dir"
    # Catch-up: mail already unread when this daemon (re)starts gets no inotify event.
    _kick "$mailbox" "$wake_cmd"
    inotifywait -m -q -e close_write -e moved_to "$dir" 2>/dev/null | \
    while :; do
        _renew_watch_lease "$mailbox"
        IFS= read -r -t "$_WATCH_BEAT" _event
        _rc=$?
        [ "$_rc" -gt 128 ] && continue
        [ "$_rc" -eq 0 ] || break
        _settle_wait "$dir"
        # Drain events that piled up during the settle period.
        while IFS= read -r -t 0.1 _burst 2>/dev/null; do :; done
        _kick "$mailbox" "$wake_cmd"
    done
}

# cmd_health — is every registered mailbox still being watched. This is watchd's only
# liveness signal for this daemon (its `extern` health command); aged unread mail is
# mail-health.sh's alarm, not this one. Exit 0: every mailbox has a live watcher. Exit 1:
# at least one does not (named on stderr).
cmd_health() {
    local rc=0 _line _mb _until
    while IFS= read -r _line; do
        case "$_line" in ''|'#'*) continue ;; esac
        _mb="${_line%%=*}"
        [ -z "$_mb" ] && continue
        _until="$(cat "$(_watch_lease_file "$_mb")" 2>/dev/null)"
        case "$_until" in ''|*[!0-9]*) _until=0 ;; esac
        [ "$_until" -gt "$(date +%s)" ] || {
            echo "spira-mail-deliver: not watching $_mb (no running watch lease)" >&2
            rc=1
        }
    done <<< "${SPIRA_MAIL_READERS:-}"
    return "$rc"
}

# SOURCEABLE, AND SILENT WHEN IT IS — same convention as watchd. A test drives _wake_loop
# and _kick directly, against a stub wake command and a real Maildir, without inotifywait or
# a spawned daemon in the loop; sourcing must not itself start watching mailboxes.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then

if [ "${1:-}" = health ]; then
    cmd_health; exit $?
fi

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
