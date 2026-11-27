#!/usr/bin/env bash
# concierge-turn-failed.sh — StopFailure hook for the Concierge session: a turn that ended on an
# API error hands its last `watchd next concierge-inbox` delivery back, so the next such read
# prints it again. Prints nothing; always exits 0.
# Test seams: SPIRA_CONCIERGE_SESSION_PID, SPIRA_PROC_ROOT.
set -uo pipefail

[ -t 0 ] || cat >/dev/null 2>&1 || true
[ "${SPIRA_CONCIERGE:-}" = 1 ] || exit 0

PROC="${SPIRA_PROC_ROOT:-/proc}"
ppid_of() { awk '{ sub(/^.*\) /, ""); print $2 }' "$PROC/$1/stat" 2>/dev/null; }

session="${SPIRA_CONCIERGE_SESSION_PID:-}"
if [ -z "$session" ]; then
    p=$$
    while [ -n "$p" ] && [ "$p" -gt 1 ] 2>/dev/null; do
        [ "$(cat "$PROC/$p/comm" 2>/dev/null)" = claude ] && { session=$p; break; }
        p="$(ppid_of "$p")"
    done
fi
[ -n "$session" ] || exit 0

watchd undeliver concierge-inbox --session "$session" >/dev/null 2>&1 || true
exit 0
