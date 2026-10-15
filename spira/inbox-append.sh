#!/usr/bin/env bash
#
# inbox-append.sh "<text>" — record an event for the Concierge WITHOUT touching the pane.
#
# This is the default `concierge` reader in SPIRA_MAIL_READERS (see conf.sh), and every
# watchd watcher able to reach the concierge's inbox writes here too. The concierge's own
# inbox-triage Monitor tails SPIRA_CONCIERGE_INBOX and is the normal reader.
#
# A line matching SPIRA_CONCIERGE_PUSH_PATTERN is also pushed: queued to a spool, and one
# detached flusher delivers the whole burst, once the spool is quiet for SPIRA_CONCIERGE_PUSH_IDLE
# (capped at SPIRA_CONCIERGE_PUSH_WINDOW), as ONE `concierge.sh wake` (held by
# the wake until the input line is empty, so it cannot split the operator's half-written
# message). The urgent class then does not depend on the Monitor being armed.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"
mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" || exit 1
line="$(date -u +%FT%TZ) $*"
printf '%s\n' "$line" >> "$SPIRA_CONCIERGE_INBOX"

[ -n "$SPIRA_CONCIERGE_PUSH_WAKE" ] || exit 0
printf '%s' "$*" | grep -qE -- "$SPIRA_CONCIERGE_PUSH_PATTERN" || exit 0

SPOOL="$SPIRA_CONCIERGE_INBOX.push"
printf '%s\n' "$line" >> "$SPOOL"

flush() {
    local waited=0 quiet=0 size last=-1
    while [ "$waited" -lt "$((SPIRA_CONCIERGE_PUSH_WINDOW * 5))" ] && [ "$quiet" -lt "$((SPIRA_CONCIERGE_PUSH_IDLE * 5))" ]; do
        sleep 0.2
        waited=$((waited + 1))
        size="$(stat -c %s "$SPOOL" 2>/dev/null || echo 0)"
        if [ "$size" = "$last" ]; then quiet=$((quiet + 1)); else quiet=0; fi
        last="$size"
    done
    local batch="$SPOOL.$$" n text
    mv "$SPOOL" "$batch" 2>/dev/null || return 0
    n="$(wc -l < "$batch")"
    text="[inbox-push] $n urgent event(s): $(tr '\n' ';' < "$batch")"
    rm -f "$batch"
    # shellcheck disable=SC2086
    $SPIRA_CONCIERGE_PUSH_WAKE "$text"
}

# The lock holder drains until the spool is empty, so a line appended during a held wake is
# never stranded behind a flusher that failed to take the lock.
(
    flock -n 9 || exit 0
    while [ -e "$SPOOL" ]; do flush; done
) 9>"$SPOOL.lock" </dev/null >/dev/null 2>&1 &
disown 2>/dev/null
exit 0
