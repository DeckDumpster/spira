#!/usr/bin/env bash
#
# test-cockpit-db.sh — unanswered.sh ordering and the ask path, against a real bd.
#
#   ./test-cockpit-db.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. unanswered.sh read rows[-1] positionally and never
# sorted. bd comments orders by created_at at second resolution, so two comments written
# in the same second come back in an order nothing defines. The turn marker inverted: a
# thread Ryan spoke last on read as answered and dropped off the "waiting on a reply" list,
# which is the exact failure that page exists to prevent. Observed 2026-09-06 in
# test-cockpit-dbs.sh (later deleted): a fixture that added my reply and his answer
# back-to-back produced identical created_at stamps and unanswered.sh reported "no threads
# are waiting on a reply". The suite worked around it with a sleep 2 — the tell that the
# ordering is the program's to establish and not the test's to arrange around.
#
# defect: sp-b0c
# covers: cockpit/unanswered.sh cockpit/ask.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
. "$HERE/testdb.sh"
testdb_require test-cockpit-db
testdb_up cockpitdb || { echo "testdb_up failed"; exit 1; }

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
eq()     { [ "$3" = "$2" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

DB="$SPIRA_DB"
export COCKPIT_DB="$DB"
BD="${TESTDB_BD:-bd}"

# THE ASK LABEL IS CONFIGURED, AND THIS FIXTURE USED TO WRITE IT AS A LITERAL. conf.sh
# defaults SPIRA_ASK_LABEL to "needs-operator"; the box this was written on sets "needs-ryan"
# in its spira.conf, so seeding the literal "needs-ryan" matched on that box and nowhere else.
# Inside a container there is no operator config, unanswered.sh looked for "needs-operator",
# and every seeded thread was invisible: "no threads are waiting on a reply" — the reassuring
# answer, from a fixture that had built four threads it could not see.
#
# PINNED TO A NON-DEFAULT ON PURPOSE. Seeding the default would pass just as well against
# code with the literal written in, which is the thing the key exists to stop (CLAUDE.md).
# This value is neither the shipped default nor the one this box happens to use, so both
# failures are visible here.
export SPIRA_ASK_LABEL="fixture-owes-a-reply"

# THE OPERATOR'S ACTOR NAME IS CONFIGURED TOO, and this fixture used to write it as the
# literal "ryan". unanswered.sh decides whose turn it is by comparing each comment's author
# to SPIRA_OPERATOR_ACTOR, whose shipped default is "operator" — so on the box this was
# written on the seeded author matched, and in a container it matched nothing: every thread
# looked like one the human had never spoken in, and none of them was waiting on a reply.
# Same failure, same silence, same reassuring output as the label above.
#
# Pinned to a non-default for the same reason: seeding "operator" would pass against code
# with the default written in.
export SPIRA_OPERATOR_ACTOR="fixture-human"

bead() {   # bead <id> <labels-csv> <status> <description>
    printf '{"id":"%s","title":"t %s","description":"%s","status":"%s","issue_type":"task","labels":[%s],"updated_at":"2026-09-05T00:00:00Z"}\n' \
      "$1" "$1" "$4" "$3" "$(printf '"%s",' ${2//,/ } | sed 's/,$//')"
}

# ======================================================================================
echo "unanswered.sh — whose turn it is, on an order the program establishes"

# THE STAMPS ARE SEEDED, NOT TAKEN FROM THE CLOCK. `bd comments add` stamps from the clock
# at second resolution, so a same-second thread can only be produced by luck. A test that
# sleeps to avoid it proves the opposite of what is wanted — the ordering is the program's
# to establish, not the fixture's to arrange around. `bd import` carries created_at through
# verbatim, which makes every case below exact.
#
# And the tie really is undefined: seeded claude-then-ryan, `bd comments` returns sp-tie
# ryan-then-claude. Reading the positional last row called that thread answered and dropped
# it off this list, which is the failure this suite exists to catch.
comment() {  # comment <bead> <author> <stamp>
    printf '{"issue_id":"%s","author":"%s","text":"%s said at %s","created_at":"%s"},' \
        "$1" "$2" "$2" "$3" "$3"
}
convo() {    # convo <id> <comment-json...>  -> one import row
    printf '{"id":"%s","title":"t %s","description":"d","status":"open","issue_type":"task",' "$1" "$1"
    printf '"labels":["%s","overseer"],"updated_at":"2026-09-05T00:00:00Z","comments":[%s]}\n' \
        "$SPIRA_ASK_LABEL" "${2%,}"
}
testdb_seed <<EOF
$(convo sp-tie   "$(comment sp-tie   claude 2026-09-06T10:00:00Z)$(comment sp-tie   "$SPIRA_OPERATOR_ACTOR" 2026-09-06T10:00:00Z)")
$(convo sp-flip  "$(comment sp-flip  "$SPIRA_OPERATOR_ACTOR"   2026-09-06T10:00:00Z)$(comment sp-flip  claude 2026-09-06T10:00:00Z)")
$(convo sp-rev   "$(comment sp-rev   "$SPIRA_OPERATOR_ACTOR" 2026-09-06T12:00:00Z)$(comment sp-rev   claude 2026-09-06T09:00:00Z)")
$(convo sp-done2 "$(comment sp-done2 "$SPIRA_OPERATOR_ACTOR"   2026-09-06T09:00:00Z)$(comment sp-done2 claude 2026-09-06T12:00:00Z)")
EOF
out="$("$COCKPIT/unanswered.sh" 2>&1)"

# The tie resolves toward "he is owed a reply", in BOTH seeded orders, because neither
# order means anything. A thread wrongly listed costs a glance; a thread wrongly dropped
# is the multi-hour silence this file exists to end.
want "a same-second thread he is in still counts as waiting"        "sp-tie"  "$out"
want "and counts the same with the tie seeded the other way round"  "sp-flip" "$out"
# Sorted rather than positional: his comment is the newest by stamp but not last in the
# seeded array.
want "the newest comment is the newest by STAMP, not by position"   "sp-rev"  "$out"
# THE NEGATIVE CONTROL, and the one that matters most. Without it, "list every thread he
# has ever spoken on" passes all three assertions above — and that check can never read
# all-clear, which is the same reassuring lie in the other direction.
nowant "a thread answered strictly later is NOT waiting"            "sp-done2" "$out"
# stdout only: `--count` answers with a number, and folding stderr into it makes any
# warning from any layer read as a wrong count.
eq "so three of the four are waiting" "3" "$("$COCKPIT/unanswered.sh" --count 2>/dev/null)"

# COCKPIT_HUMAN is the knob that says whose voice counts as the ask. It resolves through
# $HUMAN in the exported environment, so `COCKPIT_HUMAN=claude` makes claude the asker
# and the default (ryan/operator) threads drop off.
out="$(COCKPIT_HUMAN=claude "$COCKPIT/unanswered.sh" 2>&1)"
want "COCKPIT_HUMAN reassigns whose turn it is" "sp-done2" "$out"
nowant "and takes the default's threads off the list" "sp-rev" "$out"

# ======================================================================================
# ======================================================================================
echo
echo "ask.sh note — an outcome, which is not an insight and not an ask"

testdb_reset
ev() { "$BD" -C "$DB" list --all --limit 0 -t event --json 2>/dev/null | sed -n '/^[[{]/,$p'; }
one() { ev | python3 -c 'import json,sys;r=json.load(sys.stdin);print((r[0] if r else {}).get(sys.argv[1]) or "")' "$1"; }

out="$("$COCKPIT/ask.sh" note "sp-x landed on master" --kind bead.landed --target sp-x \
        --why "the commit naming it is an ancestor of origin/master" 2>&1)"
want "the verb reports the kind it recorded" "bead.landed" "$out"
eq   "it is an event, not a task"        "event"        "$(one issue_type)"
eq   "event_kind is set to the taxonomy" "bead.landed"  "$(one event_kind)"
# CREATED CLOSED. An OPEN event is claimable work — that is trap 2 of sp-q7k — and the
# harness reaps and retries anything that sits open with the plan's labels on it.
eq   "and created closed, because it is a record" "closed" "$(one status)"
eq   "carrying no labels at all"          "[]"          "$(ev | python3 -c 'import json,sys;r=json.load(sys.stdin);print(json.dumps((r[0] if r else {}).get("labels") or []))')"

# THE LABEL THAT WOULD PUT IT IN THE WRONG QUEUE. `overseer` is what DECISIONS matches on,
# so an event carrying it renders as a thing awaiting the operator; `insight` puts it straight
# back in the bin this verb exists to empty.
body="$(one description)"
# The LABELS, not the whole row: `created_by` is `overseer` on everything this harness
# writes, so a grep over the JSON would pass for the wrong reason and keep passing.
labels="$(ev | python3 -c 'import json,sys;r=json.load(sys.stdin);print(",".join((r[0] if r else {}).get("labels") or []))')"
nowant "an event is not labelled overseer" "overseer" ",$labels,"
nowant "an event is not labelled insight"  "insight"  ",$labels,"
# It must not read like an ask either — the framing the operator objected to was in the text.
nowant "its body does not ask for a verdict" "Answer inline" "$body"
nowant "nor tell them something is blocked"  "What is blocked" "$body"
want   "it says nothing is owed"             "Nothing is owed" "$body"

# AN EMITTED EVENT IS NOT CLAIMABLE. This is the property, checked through the sentinel's
# own predicate rather than by reasoning about it: `bd ready` with the plan's labels.
ready="$("$BD" -C "$DB" ready --limit 0 --exclude-type epic --label spira,plan \
          --exclude-label "spira-poison,$SPIRA_ASK_LABEL" --json 2>/dev/null | sed -n '/^[[{]/,$p')"
nowant "an emitted event is not returned by the sentinel's ready predicate" \
       "$(one id)" "${ready:-[]}"
# ...and the check could have found something: a bead that IS claimable shows up in it.
testdb_seed <<'JSONL'
{"id":"sp-work","title":"real work","description":"d","status":"open","issue_type":"task","labels":["spira","plan"]}
JSONL
ready="$("$BD" -C "$DB" ready --limit 0 --exclude-type epic --label spira,plan \
          --exclude-label "spira-poison,$SPIRA_ASK_LABEL" --json 2>/dev/null | sed -n '/^[[{]/,$p')"
want "and that predicate does return claimable work, so its silence means something" \
     "sp-work" "${ready:-[]}"

# THE TAXONOMY IS VALIDATED, NOT TRUSTED. Free text in event_kind is what makes the panel's
# badge unreadable, and `event_kind` is varchar(32) — a longer one is truncated by the
# database rather than reported by the tool.
out="$("$COCKPIT/ask.sh" note "a thing" --kind "Pilgrimage Complete!" 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && ok "a free-text kind is refused" || bad "a free-text kind is refused" "$out"
out="$("$COCKPIT/ask.sh" note "a thing" --kind "$(printf 'a%.0s' $(seq 40))" 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && ok "a kind longer than the column is refused" \
                || bad "a kind longer than the column is refused" "$out"
eq "and neither wrote a bead" "1" "$(ev | python3 -c 'import json,sys;print(len(json.load(sys.stdin)))')"

out="$("$COCKPIT/ask.sh" list events 2>&1)"
want "the events view lists it by kind" "bead.landed" "$out"
nowant "and the insights view does not" "bead.landed" "$("$COCKPIT/ask.sh" list insights 2>&1)"
nowant "nor the needs-you view"         "bead.landed" "$("$COCKPIT/ask.sh" list 2>&1)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
