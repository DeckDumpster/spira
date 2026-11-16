#!/usr/bin/env bash
# concierge-waiter-stop.sh — Stop hook for the Concierge session: refuse idle without a live
# inbox waiter descended from this session's claude process, or a delivery by this session's
# waiter (watchd's concierge-inbox.delivered record) after the turn's last input.
#
# The one waiter is `watchd next concierge-inbox`, found by walking /proc from the session pid
# and comparing argv basenames and subcommand, never by a pattern over the whole process table:
# another session's waiter must not count.
# Override: SPIRA_CONCIERGE_ALLOW_IDLE=1. Test seam: SPIRA_CONCIERGE_SESSION_PID names the
# session pid instead of the nearest claude ancestor; SPIRA_PROC_ROOT replaces /proc.
set -uo pipefail

input=''
[ -t 0 ] || input="$(cat 2>/dev/null)" || true
[ "${SPIRA_CONCIERGE:-}" = 1 ] || exit 0
[ "${SPIRA_CONCIERGE_ALLOW_IDLE:-}" = 1 ] && exit 0

WAITER="watchd next concierge-inbox"
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

alive=0
if [ -n "$session" ]; then
    for d in "$PROC"/[0-9]*; do
        pid="${d##*/}"
        [ "$pid" = "$$" ] && continue
        argv=()
        { while IFS= read -r -d '' a; do argv+=("$a"); done < "$d/cmdline"; } 2>/dev/null
        hit=0
        for i in 0 1; do
            [ "${argv[i]:+${argv[i]##*/}}" = watchd ] && [ "${argv[i+1]:-}" = next ] \
                && [ "${argv[i+2]:-}" = concierge-inbox ] && hit=1
        done
        [ "$hit" = 1 ] || continue
        p="$pid"; n=0
        while [ -n "$p" ] && [ "$p" -gt 1 ] 2>/dev/null && [ "$n" -lt 64 ]; do
            if [ "$p" = "$session" ]; then alive=1; break 2; fi
            p="$(ppid_of "$p")"; n=$((n+1))
        done
    done
fi

[ "$alive" = 1 ] && exit 0

# A waiter that exits delivering is what wakes the session; its notice is queued, so a delivery
# during this turn means the next turn is already coming.
delivered_this_turn() {
    python3 - "$SPIRA_RUN/watchd/concierge-inbox.delivered" "$session" "$input" <<'PY'
import json, sys
try:
    rec = json.load(open(sys.argv[1]))
    session = int(sys.argv[2])
    path = json.loads(sys.argv[3])["transcript_path"]
    if session not in rec["ancestors"] or rec["lines"] <= 0:
        sys.exit(1)
    from datetime import datetime
    start = None
    for raw in open(path):
        try:
            e = json.loads(raw)
        except ValueError:
            continue
        t = e.get("type")
        if t == "user":
            c = e.get("message", {}).get("content")
            fresh = isinstance(c, str) or (isinstance(c, list) and any(isinstance(b, dict) and b.get("type") != "tool_result" for b in c))
        else:
            fresh = t == "attachment" and e.get("attachment", {}).get("type") == "queued_command"
        if fresh and e.get("timestamp"):
            ms = datetime.fromisoformat(e["timestamp"].replace("Z", "+00:00")).timestamp() * 1000
            start = ms if start is None else max(start, ms)
    sys.exit(0 if start is not None and rec["exited_ms"] > start else 1)
except Exception:
    sys.exit(1)
PY
}
[ -n "$session" ] && [ -n "${SPIRA_RUN:-}" ] && delivered_this_turn && exit 0

reason="No inbox waiter ($WAITER) is running under this session, so a P0 landing in the inbox would go unseen. Re-arm it before idling: run \`$WAITER\` (with SPIRA_TOML set) with Bash run_in_background. To idle anyway, set SPIRA_CONCIERGE_ALLOW_IDLE=1."
python3 -c 'import json,sys; print(json.dumps({"decision": "block", "reason": sys.argv[1]}))' "$reason"
