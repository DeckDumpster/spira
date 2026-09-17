#!/usr/bin/env bash
# test-mail-decision-ask.sh — mail.sh send --kind question|decision --bead <work>
#   creates a blocking decision bead; operator reply closes the decision bead and
#   surfaces the verdict to the work bead; persona replies route to concierge.
#
# Acceptance criteria from sp-egge2:
#   (a) a question with --bead leaves the work bead blocked and not ready
#   (b) a reply closes the decision bead, work bead becomes ready with verdict visible
#   (c) a reply to an aeon persona's message lands in concierge
#
# SEEN RED (law-absence-needs-a-positive-control):
#   (a) work bead confirmed open+unblocked before send, then confirmed blocked after
#   (b) decision bead confirmed open before reply, then confirmed closed; work bead
#       confirmed still open with verdict note
#   (c) persona mailbox confirmed empty before reply, then confirmed message in concierge
#
# covers: spira/mail.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz() { [ "$2" != 0 ]   && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-mail-decision-ask.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-mail-decision-ask
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up mail-decision-ask || { echo "test-mail-decision-ask: could not build fixture database"; exit 1; }

export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_HOME="$TMP/home"
mkdir -p "$SPIRA_HOME/chamber"

MAIL="$HERE/mail.sh"
run() { bash "$MAIL" "$@"; }

bead_status() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")' 2>/dev/null
}

bead_open_deps() {   # bead_open_deps <id> — number of open blocking deps
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
deps = d[0].get("dependencies") or []
print(len([x for x in deps if x.get("status") != "closed"]))' 2>/dev/null
}

bead_notes() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("notes") or "")' 2>/dev/null
}

seed_bead() {
    printf '{"id":"%s","title":"test bead %s","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-01-01T00:00:00Z"}\n' \
        "$1" "$1" | testdb_seed >/dev/null 2>&1
}

# Helper: send a question mail and return the Message-ID of the filed message.
send_question() {
    local mailbox="$1" work_bead="$2"
    SPIRA_MAIL_LINT_CONSIDERED="test" run send "$mailbox" \
        --from "Builder <builder@spira>" \
        --subject "Should I proceed with option A?" \
        --kind question \
        --default "proceed with option A" \
        --bead "$work_bead" <<'BODY' >/dev/null 2>&1
## Question

Should I proceed with option A or wait?

## Default

proceed with option A
BODY
    # Find the newest message in the mailbox and return its Message-ID.
    local newest
    newest="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -z "$newest" ] && return 1
    awk '/^[[:space:]]*$/ { exit }
        tolower($0) ~ /^message-id:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[<>]/, ""); print; exit }
    ' "$SPIRA_MAIL/$mailbox/new/$newest"
}

compose_reply() {
    local irt="$1" body="$2"
    printf 'From: Operator <operator@spira>\n'
    printf 'Subject: Re: Should I proceed?\n'
    printf 'In-Reply-To: <%s>\n' "$irt"
    printf 'Date: %s\n' "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')"
    printf '\n'
    printf '%s\n' "$body"
}

# ==========================================================================
# (a) QUESTION WITH --bead LEAVES WORK BEAD BLOCKED
# ==========================================================================
echo
echo "(a) question with --bead blocks the work bead"

WORK_A="sp-da-work"
seed_bead "$WORK_A" || { echo "test-mail-decision-ask: could not seed work bead"; exit 1; }

open_deps_before="$(bead_open_deps "$WORK_A")"
is "SEEN RED: work bead has no open deps before send" "0" "$open_deps_before"

MSGID_A="$(send_question operator "$WORK_A")"
[ -n "$MSGID_A" ] || { echo "test-mail-decision-ask: could not send question"; exit 1; }

# The message's X-Spira-Bead should name the decision bead, not the work bead.
newest_msg="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
x_bead_a="$(awk '/^[[:space:]]*$/ { exit }
    tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
' "$SPIRA_MAIL/operator/new/$newest_msg")"
lacks "X-Spira-Bead is not the work bead (it is the decision bead)" "$WORK_A" "$x_bead_a"
is "decision bead id is present" "0" "$([ -n "$x_bead_a" ] && echo 0 || echo 1)"

open_deps_after="$(bead_open_deps "$WORK_A")"
is "work bead has one open dep after send" "1" "$open_deps_after"

work_status_a="$(bead_status "$WORK_A")"
is "work bead is still open (not closed)" "open" "$work_status_a"

dec_status_a="$(bead_status "$x_bead_a")"
is "decision bead is open (waiting for reply)" "open" "$dec_status_a"

# ==========================================================================
# (b) REPLY CLOSES DECISION BEAD, NOT WORK BEAD; VERDICT VISIBLE ON WORK BEAD
# ==========================================================================
echo
echo "(b) reply closes decision bead, work bead gets verdict note"

compose_reply "$MSGID_A" "Yes, proceed with option A." | run sendmail 2>&1; rc=$?
isz "sendmail exits 0 on reply" "$rc"

dec_status_b="$(bead_status "$x_bead_a")"
is "decision bead is closed after reply" "closed" "$dec_status_b"

work_status_b="$(bead_status "$WORK_A")"
is "work bead is still open (reply did not close it)" "open" "$work_status_b"

open_deps_b="$(bead_open_deps "$WORK_A")"
is "work bead has no open deps after reply (unblocked)" "0" "$open_deps_b"

notes_b="$(bead_notes "$WORK_A")"
want "work bead note contains operator verdict" "Operator verdict on decision bead $x_bead_a" "$notes_b"
want "verdict note includes the reply body" "Yes, proceed with option A." "$notes_b"

# ==========================================================================
# (c) REPLY TO AEON PERSONA LANDS IN CONCIERGE
# ==========================================================================
echo
echo "(c) reply to aeon persona message lands in concierge"

# Create a builder persona file so the test matches the real persona detection.
printf '# builder persona\n' > "$SPIRA_HOME/chamber/builder.md"

WORK_C="sp-dc-work"
seed_bead "$WORK_C"
mkdir -p "$SPIRA_MAIL/operator/new" "$SPIRA_MAIL/operator/tmp" "$SPIRA_MAIL/operator/cur"
MSGID_C="$(send_question operator "$WORK_C")"
[ -n "$MSGID_C" ] || { echo "test-mail-decision-ask: could not send test message"; exit 1; }

newest_c="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
x_bead_c="$(awk '/^[[:space:]]*$/ { exit }
    tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
' "$SPIRA_MAIL/operator/new/$newest_c")"

# Create a mailbox for builder to confirm routing does NOT go there.
mkdir -p "$SPIRA_MAIL/builder/new" "$SPIRA_MAIL/builder/tmp" "$SPIRA_MAIL/builder/cur"
mkdir -p "$SPIRA_MAIL/concierge/new" "$SPIRA_MAIL/concierge/tmp" "$SPIRA_MAIL/concierge/cur"

builder_before="$(ls "$SPIRA_MAIL/builder/new" 2>/dev/null | wc -l | tr -d ' ')"
conc_before="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"

compose_reply "$MSGID_C" "Go ahead." | run sendmail 2>&1

builder_after="$(ls "$SPIRA_MAIL/builder/new" 2>/dev/null | wc -l | tr -d ' ')"
conc_after="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"

is "SEEN RED: builder mailbox did not grow" "$builder_before" "$builder_after"
is "reply to aeon persona (builder) routes to concierge" "$((conc_before + 1))" "$conc_after"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
