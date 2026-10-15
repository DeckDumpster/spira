#!/usr/bin/env bash
#
# test-concierge-waiter-stop.sh — the Concierge Stop hook refuses idle without a live inbox
# waiter descended from this session's claude process.
#
# Acceptance: no waiter child -> block naming inbox-next.sh; a live waiter child (proc fixture) -> allow;
# another session's waiter does not satisfy it; override env var allows idle.
#
# tier: T2
# covers: spira/hooks/concierge-waiter-stop.sh concierge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"
PIDS=()
trap 'kill "${PIDS[@]}" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM

HOOK="$HERE/hooks/concierge-waiter-stop.sh"
mkdir -p "$TMP/run/concierge-notes"
printf '#!/usr/bin/env bash\nsleep 300\n' > "$TMP/run/concierge-notes/inbox-next.sh"
chmod +x "$TMP/run/concierge-notes/inbox-next.sh"
W="$TMP/run/concierge-notes/inbox-next.sh"

# A stand-in session: a bash that optionally starts the waiter as its child.
session() {   # session <with-waiter 0|1> -> sets SESS (no command substitution: the child holds the pipe)
    if [ "$1" = 1 ]; then bash -c "$W & wait" </dev/null >/dev/null 2>&1 & else bash -c 'sleep 300 & wait' </dev/null >/dev/null 2>&1 & fi
    SESS=$!; PIDS+=("$SESS")
}
run_hook() {  # run_hook <session pid> [extra env...]
    local sp="$1"; shift
    env -i PATH="$PATH" SPIRA_CONCIERGE=1 SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_SESSION_PID="$sp" "$@" \
        bash "$HOOK" </dev/null
}

session 0; bare=$SESS

out="$(run_hook "$bare")"
want "no waiter: blocks" '"decision": "block"' "$out"
want "the refusal names the waiter" "inbox-next.sh" "$out"
want "the refusal names the exit" "SPIRA_CONCIERGE_ALLOW_IDLE" "$out"

want "another session's waiter does not satisfy it" '"decision": "block"' "$(run_hook "$bare")"
# Siblings that vanish between the hook's listing and its read must neither be reported nor hide the waiter.
FX="$TMP/proc"
mkdir -p "$FX/100" "$FX/200" "$FX/300" "$FX/400" "$FX/500"
printf '100 (claude) S 1 ' > "$FX/100/stat"
printf '200 (bash) S 100 ' > "$FX/200/stat"
printf 'bash\0%s\0' "$W" > "$FX/200/cmdline"
: > "$FX/300/cmdline"
printf '400 (sleep) S 100 ' > "$FX/400/stat"
printf 'sleep\0300\0' > "$FX/400/cmdline"
fx_hook() { env -i PATH="$PATH" SPIRA_CONCIERGE=1 SPIRA_RUN="$TMP/run" SPIRA_PROC_ROOT="$FX" SPIRA_CONCIERGE_SESSION_PID=100 bash "$HOOK" </dev/null 2>&1; }
is "vanishing siblings: live waiter still found, no noise" "" "$(fx_hook)"
rm "$FX/200/cmdline"
want "fixture control: without the waiter it blocks" '"decision": "block"' "$(fx_hook)"
is "override allows idle" "" "$(run_hook "$bare" SPIRA_CONCIERGE_ALLOW_IDLE=1)"
is "outside the Concierge it is inert" "" "$(run_hook "$bare" SPIRA_CONCIERGE=0)"

tl_summary
