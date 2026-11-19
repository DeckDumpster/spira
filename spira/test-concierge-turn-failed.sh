#!/usr/bin/env bash
#
# test-concierge-turn-failed.sh — the Concierge StopFailure hook hands this session's last inbox
# delivery back (`watchd undeliver concierge-inbox --session <pid>`) and is inert outside the
# Concierge. The verb itself is proven in watchd's own tests; a stub records the call here.
#
# tier: T1
# covers: spira/hooks/concierge-turn-failed.sh concierge.sh UC-operator-channel-50
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

HOOK="$HERE/hooks/concierge-turn-failed.sh"
FX="$TMP/proc"
RUN="$TMP/run"
STUBS="$TMP/stubs"
CALLS="$TMP/calls"
mkdir -p "$FX/100" "$FX/200" "$RUN/watchd" "$STUBS"
printf '100 (claude) S 1 ' > "$FX/100/stat"
printf '200 (bash) S 100 ' > "$FX/200/stat"
printf 'claude' > "$FX/100/comm"

cat > "$STUBS/watchd" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CALLS"
[ "$1 $2 $3" = "undeliver concierge-inbox --session" ] || exit 2
grep -q "\"ancestors\": \[.*\b$4\b" "$SPIRA_RUN/watchd/concierge-inbox.delivered" || exit 0
sed 's/.*"text": "\(.*\)"}$/\1/' "$SPIRA_RUN/watchd/concierge-inbox.delivered" > "$SPIRA_RUN/watchd/concierge-inbox.redeliver"
STUB
chmod +x "$STUBS/watchd"
export PATH="$STUBS:$PATH"
tl_declare_release

REC="$RUN/watchd/concierge-inbox.delivered"
REDELIVER="$RUN/watchd/concierge-inbox.redeliver"
printf '{"exited_ms": 1, "lines": 1, "ancestors": [100,1], "text": "a thing needs you"}\n' > "$REC"

hook() {
    printf '{"hook_event_name": "StopFailure", "error": "overloaded_error"}' |
        env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" CALLS="$CALLS" SPIRA_RUN="$RUN" SPIRA_PROC_ROOT="$FX" SPIRA_CONCIERGE_SESSION_PID=100 "$@" \
            bash "$HOOK" 2>&1
}

is "outside the Concierge it prints nothing" "" "$(hook SPIRA_CONCIERGE=0)"
[ ! -e "$REDELIVER" ] && [ ! -e "$CALLS" ]; is "outside the Concierge it hands nothing back" 0 "$?"

is "a failed turn prints nothing" "" "$(hook SPIRA_CONCIERGE=1)"
is "the delivery is handed back for the next waiter" "a thing needs you" "$(cat "$REDELIVER" 2>/dev/null)"
is "the verb is asked for this session only" "undeliver concierge-inbox --session 100" "$(cat "$CALLS")"

rm -f "$REDELIVER"
printf '{"exited_ms": 1, "lines": 1, "ancestors": [300,1], "text": "x"}\n' > "$REC"
hook SPIRA_CONCIERGE=1 >/dev/null
[ ! -e "$REDELIVER" ]; is "another session's delivery is not handed back" 0 "$?"

rm -f "$CALLS"
env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" CALLS="$CALLS" SPIRA_RUN="$RUN" SPIRA_PROC_ROOT="$FX" SPIRA_CONCIERGE=1 bash "$HOOK" </dev/null >/dev/null 2>&1
is "with no claude ancestor the hook asks nothing and exits 0" 0 "$?"
[ ! -e "$CALLS" ]; is "no session pid, no call" 0 "$?"

tl_summary
