#!/usr/bin/env bash
#
# test-unpoison.sh — unpoison.sh clears spira-poison so that CHECK 4 will not put it back.
#
# The failure this guards: clearing a poison by removing the label left the attempt count at
# the threshold, so the very next sentinel pass re-poisoned the bead (2026-09-26, six beads).
# The CONTROL case below reproduces exactly that with a label-only clear; the tool's case must
# come out the other way, judged by check4_decide — the function CHECK 4 itself calls.
#
# tier: T2
# covers: spira/unpoison.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-unpoison.sh"
. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require unpoison
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — attempts_of and the poison.cleared floor read the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up unpoison || skip "server testdb not available"
. "$HERE/lib.sh"
export SPIRA_POISON_ASKED="$TMP/poison-asked"; mkdir -p "$SPIRA_POISON_ASKED"

seedt() {   # seedt <id> <event_type> <new_value> <created_at>
    local uuid; uuid="$(python3 -c 'import uuid; print(uuid.uuid4())')"
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$1', '$2', 'harness', '$3', '$4')" >/dev/null 2>&1
}
labels_of() { bdjson show "$1" | python3 -c 'import sys,json
d=json.load(sys.stdin); b=(d if isinstance(d,list) else [d])[0]; print(",".join(b.get("labels") or []))'; }
status_of() { bdjson show "$1" | python3 -c 'import sys,json
d=json.load(sys.stdin); b=(d if isinstance(d,list) else [d])[0]; print(b.get("status",""))'; }
decide() { check4_decide "$(attempts_of "$1")" "$(requeues_of "$1")" "$(reclaims_of "$1")" "$(labels_of "$1")"; }
UNPOISON="$HERE/unpoison.sh"

testdb_reset
testdb_seed <<JSONL
{"id":"pz1","title":"poisoned by three failed claims","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"pz2","title":"control: label-only clear","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"pz3","title":"held by a live aeon","status":"in_progress","assignee":"aeon-test","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"pz4","title":"healthy","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"pzask","title":"Spira bead pz1 — 3 in_progress transition(s) without landing (3 attempts) — change the approach or drop it?","status":"open","issue_type":"decision","labels":["$SPIRA_ASK_LABEL","overseer"],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
for id in pz1 pz2 pz3; do
    for t in '2026-09-01 01:00:00' '2026-09-01 02:00:00' '2026-09-01 03:00:00'; do seedt "$id" claimed '' "$t"; done
done

echo
echo "CONTROL — removing only the label leaves CHECK 4 about to re-poison:"
bdq label remove pz2 spira-poison >/dev/null 2>&1
want "label-only clear: check4 still decides poison" "poison" "$(decide pz2)"

echo
echo "unpoison.sh clears so it sticks:"
out="$(bash "$UNPOISON" --bead pz1 --cause "every session ended waiting for a background batch" 2>&1)"; rc=$?
is   "exit 0" "0" "$rc"
want "reports OK" "OK   pz1" "$out"
nowant "label removed" "spira-poison" "$(labels_of pz1)"
is   "attempt count floored to 0" "0" "$(attempts_of pz1)"
nowant "check4 no longer decides poison" "poison" "$(decide pz1)"
is   "the poisoning's operator ask is resolved" "closed" "$(status_of pzask)"
want "the cause is recorded on the bead" "every session ended waiting" "$(bdq show pz1 2>/dev/null)"

echo
echo "refusals:"
out="$(bash "$UNPOISON" --bead pz1 2>&1)"; is "no --cause is a usage error" "2" "$?"
out="$(bash "$UNPOISON" --bead pz3 --cause x 2>&1)"; rc=$?
is   "a bead a live aeon holds is refused" "1" "$rc"
want "and says who holds it" "held by aeon-test" "$out"
want "and leaves its poison" "spira-poison" "$(labels_of pz3)"
out="$(bash "$UNPOISON" --bead pz4 --cause x 2>&1)"; rc=$?
is   "a healthy bead is skipped, not an error" "0" "$rc"
want "and says so" "SKIP pz4" "$out"
out="$(bash "$UNPOISON" pz1 --cause x 2>&1)"; is "a positional bead id is refused" "2" "$?"

tl_summary
