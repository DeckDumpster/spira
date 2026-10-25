#!/bin/sh
# supersede-health.sh — the duty's liveness: a polled loop is healthy, one whose last poll is long past is not.
set -u
fail() { echo "supersede-health: $1" >&2; exit 9; }
export SPIRA_CONCIERGE_INBOX="$SPIRA_RUN/duty-inbox.log"
bash spira/supersede-duty.sh watch --interval 7 --ticks 1 >/dev/null || fail "watch --ticks 1 failed"
bash spira/supersede-duty.sh health 2>/dev/null || fail "health fails right after a poll"
echo "ok 1 7" > "$SPIRA_RUN/watchd/supersede-duty.health"
if bash spira/supersede-duty.sh health 2>/dev/null; then fail "health passes with a stale last poll"; fi
