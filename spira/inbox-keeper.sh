#!/usr/bin/env bash
#
# inbox-keeper.sh — the ONLY path that may type into the concierge pane, and only a
# re-arm line. Everything else reaches the concierge session through the durable inbox
# (SPIRA_CONCIERGE_INBOX), read by the concierge's own inbox-triage Monitor — never a
# keystroke.
#
# A /clear or a compaction kills that Monitor; the SessionStart hook re-arms it (see
# hooks/session.sh), but only once a new context exists to run it in. If no inbox-triage
# has been running for SPIRA_CONCIERGE_INBOX_STALL seconds while the inbox holds lines
# written since it last ran, this wakes the pane with ONE re-arm instruction — held by
# `concierge.sh wake` until the input line is empty — then stays quiet for
# SPIRA_CONCIERGE_INBOX_BACKOFF.
#
# A watchd `daemon` row: loops forever, restarted by systemd, its stdout appended to its
# own watcher log (see spira/watchers).
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"
LOG="$SPIRA_CONCIERGE_INBOX"
CONCIERGE="$SPIRA_REPO/concierge.sh"

triage_alive() {
    local p
    for p in /proc/[0-9]*; do
        case "$(tr '\0' ' ' < "$p/cmdline" 2>/dev/null)" in
            # Matches both the rewritten binary's bare cmdline and any stray old-style
            # invocation (sp-48f6g: inbox-triage.sh retired in favour of the Rust binary
            # `inbox-triage`, invoked by bare name — this pattern must keep matching
            # "inbox-triage" without the ".sh" suffix or triage_alive() never sees it, and
            # this loop wakes the pane every backoff period even while it is running fine).
            *inbox-triage*) return 0 ;;
        esac
    done
    return 1
}

last_seen=$(date +%s); last_wake=0
while true; do
    now=$(date +%s)
    if triage_alive; then
        last_seen=$now
    else
        lm="$(stat -c %Y "$LOG" 2>/dev/null || echo 0)"
        if [ $((now - last_seen)) -ge "${SPIRA_CONCIERGE_INBOX_STALL:-600}" ] \
           && [ "$lm" -gt "$last_seen" ] \
           && [ $((now - last_wake)) -ge "${SPIRA_CONCIERGE_INBOX_BACKOFF:-3600}" ]; then
            echo "$(date -u +%FT%TZ) no triage monitor for $(( (now-last_seen)/60 ))m with events waiting — re-arm wake"
            "$CONCIERGE" wake "[inbox-keeper] events are waiting and no inbox monitor is running — re-arm: Monitor inbox-triage"
            last_wake=$now
        fi
    fi
    sleep 60
done
