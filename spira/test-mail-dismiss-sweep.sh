#!/usr/bin/env bash
# test-mail-dismiss-sweep.sh — deleting an ask mail dismisses the bead it tracked.
#
# mail.sh send records, for every operator question/decision that carries a bead, a
# msgid -> bead line in SPIRA_MAIL_INDEX (spira/mail.sh's _index_record). mail.sh
# sweep-dismissed reads that index and, for each entry whose mail file can no longer be
# found by message-id anywhere under SPIRA_MAIL, closes the bead with
# "dismissed by operator (mail deleted) — default taken: <X-Spira-Default>" — unless the
# bead already carries an audit event authored by the operator actor (a reply outranks a
# deletion even if something else left the bead open), or the message was found elsewhere
# (moved, not deleted).
#
# POSITIVE CONTROL: a still-present ask is confirmed NOT dismissed (SEEN RED would be a
# sweep that dismisses everything indexed, deletion or not) before the deleted ask is
# confirmed dismissed, so "nothing to dismiss" printed by an inert sweep cannot be mistaken
# for a working one (law-absence-needs-a-positive-control).
#
# SPIRA_OPERATOR_ACTOR and SPIRA_MAIL_INDEX are pinned to non-default values so a hardcoded
# "operator" or a hardcoded "$SPIRA_MAIL/index" in the sweep would fail the fixture rather
# than pass it by accident (law-gates-run-in-a-clean-environment).
#
# tier: T2
# covers: spira/mail.sh spira/conf.sh UC-operator-channel-27
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-mail-dismiss-sweep.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-mail-dismiss-sweep
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up dismiss-sweep || { echo "test-mail-dismiss-sweep: could not build fixture database"; exit 1; }

export SPIRA_MAIL="$TMP/mail"
export SPIRA_MAIL_INDEX="$TMP/nonstandard-index-path/log"   # non-default: catches a hardcoded path
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_HOME="$TMP/home"
export SPIRA_RUN="$TMP/run"
export SPIRA_OPERATOR_ACTOR="ryan-op"                         # non-default: catches a hardcoded "operator"
mkdir -p "$SPIRA_HOME/chamber" "$SPIRA_RUN"

MAIL="$HERE/mail.sh"
run() { bash "$MAIL" "$@"; }

bead_status() {
    "$SPIRA_BD" -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")' 2>/dev/null
}

bead_close_reason() {
    "$SPIRA_BD" -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("close_reason") or "")' 2>/dev/null
}

msgid_of() {   # msgid_of <mailbox> -> bare Message-ID of the newest message in new/
    local mailbox="$1" newest
    newest="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -z "$newest" ] && return 1
    awk '/^[[:space:]]*$/ { exit }
        tolower($0) ~ /^message-id:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[<>]/, ""); print; exit }
    ' "$SPIRA_MAIL/$mailbox/new/$newest"
}

xbead_of() {   # xbead_of <mailbox> -> X-Spira-Bead of the newest message in new/
    local mailbox="$1" newest
    newest="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -z "$newest" ] && return 1
    awk '/^[[:space:]]*$/ { exit }
        tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
    ' "$SPIRA_MAIL/$mailbox/new/$newest"
}

newest_path() {   # newest_path <mailbox> -> full path of newest message in new/
    local mailbox="$1" newest
    newest="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -z "$newest" ] && return 1
    printf '%s/%s/new/%s' "$SPIRA_MAIL" "$mailbox" "$newest"
}

send_question() {   # send_question <subject> -> leaves message in operator/new for the caller
    local subject="$1"
    SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_REPEAT_CONSIDERED="test" \
        run send operator --from "Builder <builder@spira>" --subject "$subject" \
            --kind question --default "proceed with the default" \
            <<'BODY' >/dev/null 2>"$TMP/send.err"
## Question

Should I proceed?

## Default

proceed with the default
BODY
}

index_lines() { wc -l < "$SPIRA_MAIL_INDEX" 2>/dev/null | tr -d ' '; }

# ==========================================================================
# Index is written at send time
# ==========================================================================
echo
echo "index recording"

is "index does not exist before any question is sent" "" "$(index_lines)"

send_question "Should I proceed with A?"
BEAD_A="$(xbead_of operator)"
[ -n "$BEAD_A" ] || { echo "test-mail-dismiss-sweep: no decision bead filed"; exit 1; }
MSGID_A="$(msgid_of operator)"
PATH_A="$(newest_path operator)"

is "SEEN RED: index has exactly 1 line after one question" "1" "$(index_lines)"
want "index line names the bead" "$BEAD_A" "$(cat "$SPIRA_MAIL_INDEX")"
want "index line names the msgid" "$MSGID_A" "$(cat "$SPIRA_MAIL_INDEX")"
is "decision bead open before sweep" "open" "$(bead_status "$BEAD_A")"

# ==========================================================================
# POSITIVE CONTROL — sweep finds nothing to dismiss while the mail is present
# ==========================================================================
echo
echo "positive control: present mail is not dismissed"

out="$(run sweep-dismissed operator 2>&1)"; rc=$?
is "sweep exits 0 with the mail present" "0" "$rc"
is "SEEN RED (sweep looked): dismissed 0 while mail is present" "0" "$(bead_status "$BEAD_A" | grep -c closed || true)"
is "decision bead still open (mail was never deleted)" "open" "$(bead_status "$BEAD_A")"
want "sweep reports kept, not dismissed, for the present mail" "kept 1" "$out"

# ==========================================================================
# Deletion dismisses the bead
# ==========================================================================
echo
echo "deletion dismisses the bead with the default"

rm -f "$PATH_A"
out="$(run sweep-dismissed operator 2>&1)"; rc=$?
is "sweep exits 0" "0" "$rc"
want "sweep reports 1 dismissed" "dismissed 1" "$out"
is "SEEN RED (sweep acted): decision bead is closed after deletion" "closed" "$(bead_status "$BEAD_A")"
want "close reason says dismissed by operator" "dismissed by operator (mail deleted)" "$(bead_close_reason "$BEAD_A")"
want "close reason carries the default" "proceed with the default" "$(bead_close_reason "$BEAD_A")"

# Re-running the sweep does nothing further (already closed).
out2="$(run sweep-dismissed operator 2>&1)"; rc2=$?
is "second sweep run exits 0" "0" "$rc2"
want "second run reports nothing new dismissed" "dismissed 0" "$out2"

# ==========================================================================
# A reply in the thread outranks a deletion, even if the bead is still open
# ==========================================================================
echo
echo "a reply keeps the bead open despite the deleted mail"

send_question "Should I proceed with B?"
BEAD_B="$(xbead_of operator)"
PATH_B="$(newest_path operator)"
is "decision bead B open before reply" "open" "$(bead_status "$BEAD_B")"

BEADS_ACTOR="$SPIRA_OPERATOR_ACTOR" "$SPIRA_BD" -C "$SPIRA_DB" note "$BEAD_B" "Go ahead." >/dev/null 2>&1
rm -f "$PATH_B"

out="$(run sweep-dismissed operator 2>&1)"; rc=$?
is "sweep exits 0" "0" "$rc"
is "SEEN RED (sweep would dismiss without the reply check): bead B stays open" "open" "$(bead_status "$BEAD_B")"
want "sweep reports the replied bead as kept" "kept" "$out"

# ==========================================================================
# A moved (not deleted) mail is not a dismissal
# ==========================================================================
echo
echo "a moved mail (found elsewhere by message-id) is not a dismissal"

send_question "Should I proceed with C?"
BEAD_C="$(xbead_of operator)"
PATH_C="$(newest_path operator)"

mkdir -p "$SPIRA_MAIL/archive/new" "$SPIRA_MAIL/archive/cur" "$SPIRA_MAIL/archive/tmp"
mv "$PATH_C" "$SPIRA_MAIL/archive/cur/$(basename "$PATH_C")"

out="$(run sweep-dismissed operator 2>&1)"; rc=$?
is "sweep exits 0" "0" "$rc"
is "moved bead C stays open (message found in another mailbox)" "open" "$(bead_status "$BEAD_C")"
want "sweep reports the moved bead as kept" "kept" "$out"

# ==========================================================================
# Non-ask mail is never indexed (no --kind question/decision, or no bead)
# ==========================================================================
echo
echo "a notice with no bead is never indexed"

before="$(index_lines)"
SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_REPEAT_CONSIDERED="test" \
    run send operator --from "Gate <gate@spira>" --subject "FYI: nothing to see" \
        <<< "just an FYI" >/dev/null 2>&1
after="$(index_lines)"
is "index unchanged by a bead-less, kind-less send" "$before" "$after"

# ==========================================================================
# FAIL CLOSED — an unreadable index dismisses nothing
# ==========================================================================
echo
echo "guard: unreadable index refuses to dismiss anything"

send_question "Should I proceed with D?"
BEAD_D="$(xbead_of operator)"
PATH_D="$(newest_path operator)"
rm -f "$PATH_D"

chmod 000 "$SPIRA_MAIL_INDEX"
out="$(run sweep-dismissed operator 2>&1)"; rc=$?
chmod 644 "$SPIRA_MAIL_INDEX"
[ "$rc" -ne 0 ] && ok "sweep refuses (non-zero exit) when the index is unreadable" \
                 || bad "sweep refuses when the index is unreadable" "expected non-zero exit, got 0"
want "refusal message mentions the index" "unreadable" "$out"
is "bead D untouched while the index is unreadable" "open" "$(bead_status "$BEAD_D")"

# Now readable again: the deferred deletion is picked up.
out="$(run sweep-dismissed operator 2>&1)"
want "sweep reports 1 dismissed once the index is readable again" "dismissed 1" "$out"
is "bead D closed once the index is readable again" "closed" "$(bead_status "$BEAD_D")"

# ==========================================================================
# FAIL CLOSED — no bead store configured
# ==========================================================================
echo
echo "guard: no SPIRA_DB refuses to dismiss anything"

rc=0
out="$(SPIRA_DB="" run sweep-dismissed operator 2>&1)" || rc=$?
[ "$rc" -ne 0 ] && ok "sweep refuses (non-zero exit) with no bead store configured" \
                 || bad "sweep refuses with no bead store configured" "expected non-zero exit, got 0"
want "refusal message mentions the store" "bead store" "$out"

tl_summary
