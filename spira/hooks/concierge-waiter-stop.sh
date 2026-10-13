#!/usr/bin/env bash
# concierge-waiter-stop.sh — Stop hook for the Concierge session: refuse idle without a live
# inbox waiter descended from this session's claude process.
#
# The waiter is found by walking /proc from the session pid and comparing argv basenames,
# never by a pattern over the whole process table: another session's waiter must not count.
# Override: SPIRA_CONCIERGE_ALLOW_IDLE=1. Test seam: SPIRA_CONCIERGE_SESSION_PID names the
# session pid instead of the nearest claude ancestor.
set -uo pipefail

[ -t 0 ] || cat >/dev/null 2>&1 || true
[ "${SPIRA_CONCIERGE:-}" = 1 ] || exit 0
[ "${SPIRA_CONCIERGE_ALLOW_IDLE:-}" = 1 ] && exit 0

WAITER="${SPIRA_CONCIERGE_WAITER:-${SPIRA_RUN:-}/concierge-notes/inbox-next.sh}"
WNAME="${WAITER##*/}"

ppid_of() { awk '{ sub(/^.*\) /, ""); print $2 }' "/proc/$1/stat" 2>/dev/null; }

session="${SPIRA_CONCIERGE_SESSION_PID:-}"
if [ -z "$session" ]; then
    p=$$
    while [ -n "$p" ] && [ "$p" -gt 1 ] 2>/dev/null; do
        [ "$(cat "/proc/$p/comm" 2>/dev/null)" = claude ] && { session=$p; break; }
        p="$(ppid_of "$p")"
    done
fi

alive=0
if [ -n "$session" ]; then
    for d in /proc/[0-9]*; do
        pid="${d#/proc/}"
        [ "$pid" = "$$" ] && continue
        argv=()
        while IFS= read -r -d '' a; do argv+=("$a"); done < "$d/cmdline" 2>/dev/null
        hit=0
        for a in "${argv[@]:0:2}"; do [ "${a##*/}" = "$WNAME" ] && hit=1; done
        [ "$hit" = 1 ] || continue
        p="$pid"; n=0
        while [ -n "$p" ] && [ "$p" -gt 1 ] 2>/dev/null && [ "$n" -lt 64 ]; do
            if [ "$p" = "$session" ]; then alive=1; break 2; fi
            p="$(ppid_of "$p")"; n=$((n+1))
        done
    done
fi

[ "$alive" = 1 ] && exit 0

reason="No inbox waiter ($WNAME) is running under this session, so a P0 landing in the inbox would go unseen. Re-arm it before idling: run \`$WAITER\` with Bash run_in_background. To idle anyway, set SPIRA_CONCIERGE_ALLOW_IDLE=1."
python3 -c 'import json,sys; print(json.dumps({"decision": "block", "reason": sys.argv[1]}))' "$reason"
