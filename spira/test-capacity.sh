#!/usr/bin/env bash
#
# test-capacity.sh — capacity.sh reclassify --apply withdraws an attempt through the content
# door (spira-lc content label remove / note --stdin) and forks no bdq.
#
# Every binary capacity.sh reaches is a stub on PATH that records its argv, so the assertion
# is on which door was knocked on.
#
# tier: T1
# covers: spira/capacity.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-capacity.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
BIN="$TMP/bin"; RUN="$TMP/run"; CALLS="$TMP/calls"; mkdir -p "$BIN" "$RUN"
: > "$CALLS"

stub() {  # stub <name> <body>
    printf '#!/usr/bin/env bash\n%s\n' "$2" > "$BIN/$1"; chmod +x "$BIN/$1"
}
stub spira-lc 'printf "spira-lc %s\n" "$*" >> "$CALLS_FILE"
case "$1 $2" in "content note") printf "stdin: %s\n" "$(cat)" >> "$CALLS_FILE" ;; esac
[ "$1" = held ] && exit 1
exit 0'
stub bdq 'printf "bdq %s\n" "$*" >> "$CALLS_FILE"; exit 0'
stub bd 'printf "bd %s\n" "$*" >> "$CALLS_FILE"; exit 0'
stub spira-claim 'case "$1" in attempts) echo 2 ;; counter-label) echo sp-attempt-2-unlanded ;; *) exit 1 ;; esac'
stub dwarf 'case "$2" in reset-at) echo 1700000000 ;; log-fingerprint) echo fp-1 ;; withdrawn-fp) ;; *) ;; esac; exit 0'

printf '{"type":"rate_limit_event"}\n' > "$RUN/sp-cap1.log"

out="$(env -i PATH="$BIN:$PATH" HOME="$TMP" SPIRA_CONF="$TMP/no-such-conf" SPIRA_RUN="$RUN" SPIRA_DB="$TMP/db" \
    CALLS_FILE="$CALLS" bash "$HERE/capacity.sh" reclassify --apply 2>&1)"

want "the withdrawal is reported restored" "RESTORED sp-cap1" "$out"
want "the rung is removed through the content door" "spira-lc content label remove sp-cap1 sp-attempt-2-unlanded" "$(cat "$CALLS")"
want "the note goes through the content door on stdin" "spira-lc content note sp-cap1 --stdin" "$(cat "$CALLS")"
want "the note text arrives on stdin" "stdin: Attempt 2 withdrawn" "$(cat "$CALLS")"
nowant "no bdq runs" "bdq " "$(cat "$CALLS")"
nowant "no bd runs" "bd " "$(grep -v '^spira-lc' "$CALLS")"

tl_summary
