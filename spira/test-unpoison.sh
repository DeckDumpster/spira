#!/usr/bin/env bash
#
# test-unpoison.sh — spira-claim unpoison clears spira-poison so that CHECK 4 will not put it back.
#
# The failure this guards: clearing a poison by removing the label left the attempt count at
# the threshold, so the very next sentinel pass re-poisoned the bead (2026-09-26, six beads).
# The CONTROL case below reproduces exactly that with a label-only clear; the tool's case must
# come out the other way, judged by `spira-claim decide` — the function CHECK 4 itself calls.
#
# tier: T2
# covers: spira-claim/* spira-lc/* lifecycle/* spira/testlib/lc-fixture.sh UC-dwarf-execution-21
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# spira-lc and spira-claim are the tree's own build, by name on the suite's PATH (sp-gypjk).
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
command -v spira-claim >/dev/null 2>&1 || bail "spira-claim is not on PATH"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

echo "test-unpoison.sh"
. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require unpoison
TMP="$(mktemp -d)"
# testdb-mode: server — attempts_of and the poison.cleared floor read the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up unpoison || skip "server testdb not available"
. "$HERE/lib.sh"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
export SPIRA_POISON_ASKED="$TMP/poison-asked"; mkdir -p "$SPIRA_POISON_ASKED"

# A REAL spira-lc against a throwaway lifecycle database (sp-rlyl0), so the poison hold this
# bead makes spira-claim unpoison release is proven against the actual machine, not assumed from
# lc.sh's own exit code. It lives on the bead store's own server, as in production: the ask
# listing reads its titles from the beads over the lifecycle connection, and unpoison finds the
# poisoning's ask by that title.
. "$HERE/testlib/lc-fixture.sh"
trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM

SPIRA_RUN="$TMP/run"; tl_config SPIRA_RUN="$SPIRA_RUN"; mkdir -p "$SPIRA_RUN"
lcfix_up_on_testdb
wantrc "spira-lc schema applies cleanly" 0 $?

lc_seed_working_poisoned() {   # lc_seed_working_poisoned <bead-id> [holder]
    local holder="NULL"; [ -n "${2:-}" ] && holder="'$2'"
    lcfix_sql -q \
        "INSERT INTO bead (bead_id, state, holder, holds, version, updated_at) VALUES ('$1','WORKING',$holder,'[\"poison\"]',0,0)" >/dev/null 2>&1
}

seedt() {   # seedt <id> <event_type> <new_value> <created_at>
    local uuid; uuid="$(python3 -c 'import uuid; print(uuid.uuid4())')"
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$1', '$2', 'harness', '$3', '$4')" >/dev/null 2>&1
}
labels_of() { bdjson show "$1" | python3 -c 'import sys,json
d=json.load(sys.stdin); b=(d if isinstance(d,list) else [d])[0]; print(",".join(b.get("labels") or []))'; }
ask_state() { spira-lc show-ask "$1" 2>/dev/null | python3 -c 'import sys,json; print(json.load(sys.stdin).get("state",""))' 2>/dev/null; }
status_of() { bdjson show "$1" | python3 -c 'import sys,json
d=json.load(sys.stdin); b=(d if isinstance(d,list) else [d])[0]; print(b.get("status",""))'; }
# reclaims_of (lib.sh) was retired at sp-8itaf — zero live callers. _counter_events_query,
# the shared helper it and requeues_of both used, stays (requeues_of still calls it), so
# this reaches the identical count the old function did.
reclaims_of() { _counter_events_query "${1:-}" reclaimed; }
decide() { spira-claim decide "$(attempts_of "$1")" "$(requeues_of "$1")" "$(reclaims_of "$1")" "$(labels_of "$1")"; }
UNPOISON=spira-claim

testdb_reset
testdb_seed <<JSONL
{"id":"sp-pz1","title":"poisoned by three failed claims","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"sp-pz2","title":"control: label-only clear","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"sp-pz3","title":"held by a live dwarf","status":"in_progress","assignee":"dwarf-test","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"sp-pz4","title":"healthy","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-01T00:00:00Z"}
{"id":"sp-pzask","title":"Spira bead sp-pz1 — 3 in_progress transition(s) without landing (3 attempts) — change the approach or drop it?","status":"open","issue_type":"decision","labels":["$SPIRA_ASK_LABEL","overseer"],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
for id in sp-pz1 sp-pz2 sp-pz3; do
    for t in '2026-09-01 01:00:00' '2026-09-01 02:00:00' '2026-09-01 03:00:00'; do seedt "$id" claimed '' "$t"; done
done
# CHECK 4's ask is its control-plane record, opened as mail opens it beside the decision bead.
spira-lc create-ask sp-pzask --work-bead sp-pz1 >/dev/null 2>&1
is "SEEN OPEN: the poisoning's ask is open in the control plane" "OPEN" "$(ask_state sp-pzask)"
lc_seed_working_poisoned sp-pz1
# sp-pz3 is held: the claim is the row's WORKING holder, never bd's in_progress (sp-mve9i).
lc_seed_working_poisoned sp-pz3 dwarf-test

echo
echo "CONTROL — removing only the label leaves CHECK 4 about to re-poison:"
bdq label remove sp-pz2 spira-poison >/dev/null 2>&1
want "label-only clear: check4 still decides poison" "poison" "$(decide sp-pz2)"

echo
echo "spira-claim unpoison clears so it sticks:"
out="$("$UNPOISON" unpoison --bead sp-pz1 --cause "every session ended its turn 'waiting for' a background batch" 2>&1)"; rc=$?
is   "exit 0" "0" "$rc"
want "reports OK" "OK   sp-pz1" "$out"
# THE LABEL IS VESTIGIAL, NOT CHECK 4's TO MANAGE (sp-i2m7y): CHECK 4 no longer writes or
# clears spira-poison at all once its poison hold lives entirely in spira-lc, so nothing
# would ever take a stale label off if unpoison left it — it removes it itself,
# best-effort, alongside the hold that actually matters.
nowant "the vestigial label is removed too" "spira-poison" "$(labels_of sp-pz1)"
is   "attempt count floored to 0" "0" "$(attempts_of sp-pz1)"
nowant "check4 no longer decides poison" "poison" "$(decide sp-pz1)"
nowant "the lifecycle poison hold is released, against a real spira-lc" "poison" "$(spira-lc holds sp-pz1)"
is   "...and the lifecycle row's WORKING state is undisturbed by releasing the hold" \
     "WORKING" "$(spira-lc state sp-pz1)"
is   "the poisoning's operator ask is resolved in the control plane" "WITHDRAWN" "$(ask_state sp-pzask)"
is   "...and its decision bead follows the ask record in bd" "closed" "$(status_of sp-pzask)"
want "the cause is recorded on the bead" "every session ended its turn" "$(bdjson show sp-pz1 | python3 -c 'import sys,json
d=json.load(sys.stdin); b=(d if isinstance(d,list) else [d])[0]; print(b.get("notes") or "")')"

echo
echo "refusals:"
out="$("$UNPOISON" unpoison --bead sp-pz1 2>&1)"; is "no --cause is a usage error" "1" "$?"
out="$("$UNPOISON" unpoison --bead sp-pz3 --cause x 2>&1)"; rc=$?
is   "a bead a live dwarf holds is refused" "3" "$rc"
want "and says who holds it" "held by dwarf-test" "$out"
want "and leaves its poison" "spira-poison" "$(labels_of sp-pz3)"
want "and leaves its lifecycle poison hold too, against a real spira-lc" "poison" "$(spira-lc holds sp-pz3)"
out="$("$UNPOISON" unpoison --bead sp-pz4 --cause x 2>&1)"; rc=$?
is   "a healthy bead is skipped, not an error" "0" "$rc"
want "and says so" "SKIP sp-pz4" "$out"
out="$("$UNPOISON" unpoison sp-pz1 --cause x 2>&1)"; is "a positional bead id is refused" "1" "$?"

tl_summary
