#!/usr/bin/env bash
#
# inbox-triage.sh — the concierge's in-session Monitor over SPIRA_CONCIERGE_INBOX.
#
# The mandatory first action a concierge session takes (see hooks/session.sh) is arming
# this as a Monitor: it tails the durable inbox every watcher and mail-deliver appends to
# (inbox-append.sh) and is how events reach the session with no keystroke into the pane.
# It must stay constantly attached — see inbox-keeper.sh for what re-arms it if it lapses.
#
# Reads new lines as they are written (tail -F follows inotify), then applies cheap
# heuristics:
#   DROP  echoes of the concierge's own actions and purely informational events
#   DEDUP the same text within SPIRA_CONCIERGE_INBOX_DEDUP seconds
#   PASS  everything else — the events that need a decision or an action
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"
LOG="$SPIRA_CONCIERGE_INBOX"
touch "$LOG" 2>/dev/null || { echo "inbox-triage: cannot create $LOG" >&2; exit 1; }
declare -A last
tail -n 0 -F "$LOG" 2>/dev/null | while IFS= read -r line; do
    body="${line#* }"                       # strip the leading UTC timestamp
    case "$body" in
        *"ROUND RESULT"*|*" OPENED "*|*"pool: NEW CERTIFIED"*) continue ;;
    esac
    key="$(printf '%s' "$body" | sed -E 's/[0-9]{2}:[0-9]{2}:[0-9]{2}Z?//g; s/oldest [0-9]+s/oldest Ns/')"
    now=$(date +%s)
    if [ -n "${last[$key]:-}" ] && [ $((now - last[$key])) -lt "${SPIRA_CONCIERGE_INBOX_DEDUP:-600}" ]; then
        continue
    fi
    last[$key]=$now
    printf '%s\n' "$body"
done
