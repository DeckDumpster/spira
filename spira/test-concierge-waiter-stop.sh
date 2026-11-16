#!/usr/bin/env bash
#
# test-concierge-waiter-stop.sh — the Concierge Stop hook allows idle with a live inbox waiter of
# this session, or with a waiter of this session that delivered during the current turn;
# otherwise it blocks, naming the waiter and the override.
#
# tier: T1
# covers: spira/hooks/concierge-waiter-stop.sh concierge.sh UC-operator-channel-49
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

HOOK="$HERE/hooks/concierge-waiter-stop.sh"
FX="$TMP/proc"
RUN="$TMP/run"
TR="$TMP/transcript.jsonl"
REC="$RUN/watchd/concierge-inbox.delivered"
mkdir -p "$FX/100" "$FX/200" "$FX/300" "$FX/400" "$RUN/watchd"
printf '100 (claude) S 1 ' > "$FX/100/stat"
printf '200 (bash) S 100 ' > "$FX/200/stat"
: > "$FX/300/cmdline"
printf '400 (sleep) S 100 ' > "$FX/400/stat"
printf 'sleep\0300\0' > "$FX/400/cmdline"

PROMPT_MS=1700000100000
QUEUED_MS=1700000200000
iso() { python3 -c 'import datetime,sys; print(datetime.datetime.fromtimestamp(int(sys.argv[1])/1000, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3]+"Z")' "$1"; }
user() { printf '{"type":"user","message":{"content":"hi"},"timestamp":"%s"}\n' "$(iso "$1")"; }
result() { printf '{"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]},"timestamp":"%s"}\n' "$(iso "$1")"; }
queued() { printf '{"type":"attachment","attachment":{"type":"queued_command","prompt":"<task-notification>"},"timestamp":"%s"}\n' "$(iso "$1")"; }
record() {   # record <exited_ms> <lines> <ancestors json>
    printf '{"exited_ms": %s, "lines": %s, "ancestors": %s}\n' "$1" "$2" "$3" > "$REC"
}
hook() {
    printf '{"transcript_path": "%s"}' "${TRANSCRIPT:-$TR}" |
        env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" SPIRA_CONCIERGE=1 SPIRA_RUN="$RUN" SPIRA_PROC_ROOT="$FX" SPIRA_CONCIERGE_SESSION_PID=100 "$@" \
            bash "$HOOK" 2>&1
}
blocks() { [ "$(grep -c '"decision": "block"' <<<"$1")" = 1 ] && echo one || echo "$1"; }

user "$PROMPT_MS" > "$TR"

printf '%s\0next\0concierge-inbox\0' "/x/bin/watchd" > "$FX/200/cmdline"
is "live waiter under the session: allow, vanishing siblings silent" "" "$(hook)"
rm "$FX/200/cmdline"

out="$(hook)"
is "no waiter, no record: exactly one block" one "$(blocks "$out")"
want "the block names the waiter" "watchd next concierge-inbox" "$out"
want "the block names the override" "SPIRA_CONCIERGE_ALLOW_IDLE" "$out"

printf 'bash\0%s\0' "$RUN/concierge-notes/inbox-next.sh" > "$FX/200/cmdline"
want "the retired inbox-next.sh argv does not satisfy it" '"decision": "block"' "$(hook)"
printf '/x/bin/watchd\0next\0other-watcher\0' > "$FX/200/cmdline"
want "watchd on another watcher does not satisfy it" '"decision": "block"' "$(hook)"
rm "$FX/200/cmdline"

record $((PROMPT_MS + 5000)) 1 '[100, 1]'
is "a waiter of this session delivered during the turn: allow" "" "$(hook)"

record $((PROMPT_MS - 5000)) 1 '[100, 1]'
is "delivery before the last prompt: block" one "$(blocks "$(hook)")"

record $((QUEUED_MS - 5000)) 1 '[100, 1]'
{ user "$PROMPT_MS"; queued "$QUEUED_MS"; } > "$TR"
is "delivery before the last queued notice: block" one "$(blocks "$(hook)")"

user "$PROMPT_MS" > "$TR"
record $((PROMPT_MS + 5000)) 1 '[300, 1]'
is "another session's waiter delivered: block" one "$(blocks "$(hook)")"

record $((PROMPT_MS + 5000)) 0 '[100, 1]'
is "a record of zero lines: block" one "$(blocks "$(hook)")"

record $((PROMPT_MS + 5000)) 1 '[100, 1]'
TRANSCRIPT="$TMP/missing.jsonl"
is "transcript missing: block" one "$(blocks "$(hook)")"
unset TRANSCRIPT

: > "$TR"
is "no input entry in the transcript: block" one "$(blocks "$(hook)")"

{ user "$PROMPT_MS"; result $((PROMPT_MS + 9000)); } > "$TR"
record $((PROMPT_MS + 5000)) 1 '[100, 1]'
is "a later tool_result does not move the turn start" "" "$(hook)"

printf 'not json' > "$REC"
user "$PROMPT_MS" > "$TR"
is "an unparseable record: block" one "$(blocks "$(hook)")"
rm "$REC"

is "override allows idle" "" "$(hook SPIRA_CONCIERGE_ALLOW_IDLE=1)"
is "outside the Concierge it is inert" "" "$(hook SPIRA_CONCIERGE=0)"

tl_summary
