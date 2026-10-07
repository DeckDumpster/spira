#!/usr/bin/env bash
#
# test-asks.sh — asks.sh announces a newly opened ask once, mails it to the inbox, and moves
# asks.cursor past it so watchd notify has nothing left to re-escalate.
#
# tier: T1
# covers: spira/asks.sh spira/watchers spira/round-duty.sh spira/conf.d/SPIRA_ASK_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-asks.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run/watchd"
INBOX="$TMP/inbox.log"; ASKS="$TMP/asks.json"
printf '#!/usr/bin/env bash\ncat "$FAKE_ASKS"\n' > "$TMP/fakebd"; chmod +x "$TMP/fakebd"
LOG="$TMP/run/watchd/asks.log"; CUR="$TMP/run/watchd/asks.cursor"

arun() {
    tl_config SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/db" SPIRA_ASK_LABEL=ask-x \
        SPIRA_CONCIERGE_INBOX="${WINBOX:-$INBOX}" SPIRA_BD="$TMP/fakebd"
    env -i PATH="$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" FAKE_ASKS="$ASKS" \
        bash "$HERE/asks.sh" "$@" 2>&1
}

echo '[{"id":"sp-old","title":"already open"}]' > "$ASKS"
arun watch --interval 1 --ticks 8 | tee "$LOG" > "$TMP/watch.out" &
wpid=$!
for _ in $(seq 100); do [ -e "$TMP/run/watchd/asks.health" ] && break; sleep 0.1; done
echo '[{"id":"sp-old","title":"already open"},{"id":"sp-new","title":"decide X"}]' > "$ASKS"
wait "$wpid"; out="$(cat "$TMP/watch.out")"
want "a new ask is printed to the log" "NEW ASK sp-new: decide X" "$out"
nowant "a baseline ask is not announced" "NEW ASK sp-old" "$out"
want "the ask is appended to the inbox" "[watch:asks] NEW ASK sp-new: decide X" "$(cat "$INBOX")"
is "the cursor is moved past the delivered line" "1" "$(cat "$CUR" 2>/dev/null)"

rm -f "$LOG" "$CUR" "$TMP/run/watchd/asks.health"
echo '[{"id":"sp-old","title":"already open"}]' > "$ASKS"
(WINBOX="$TMP" arun watch --interval 1 --ticks 6 >> "$LOG") &
wpid=$!
for _ in $(seq 100); do [ -e "$TMP/run/watchd/asks.health" ] && break; sleep 0.1; done
echo '[{"id":"sp-old","title":"already open"},{"id":"sp-n2","title":"and Y"}]' > "$ASKS"
wait "$wpid"
grep -q "NEW ASK sp-n2" "$LOG"; is "the ask reached the log though the inbox write failed" "0" "$?"
[ "$(cat "$CUR" 2>/dev/null || echo 0)" -lt "$(wc -l < "$LOG")" ]; is "a failed inbox write leaves the cursor behind the undelivered line" "0" "$?"

arun health >/dev/null; is "health passes after a fresh poll" "0" "$?"
echo "ok 1 100 7" > "$TMP/run/watchd/asks.health"
arun health >/dev/null; is "health fails once the last poll is stale" "1" "$?"

grep -q '^asks|daemon|' "$HERE/watchers"; is "watchers manifest carries the asks row" "0" "$?"
tl_summary
