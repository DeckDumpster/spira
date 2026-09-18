#!/usr/bin/env bash
# test-mail-decision-ask.sh — mail.sh send --kind question|decision --bead <work>
#   files a decision bead and wires a relates_to link (NOT a blocking dep) to the cited
#   work bead. SPIRA_MAIL_ALLOW_BLOCKING=1 restores blocking for deliberate gates.
#   (sp-aybfy: a question never blocks a work bead by default.)
#
# Acceptance criteria:
#   (a) a question with --bead on a task bead does NOT block the work bead; the guard
#       logs its refusal; decision bead is still filed; relates_to link is wired
#   (b) reply closes the decision bead; work bead stays open (was never blocked)
#   (c) reply to an aeon persona's message lands in concierge
#   (d) SPIRA_MAIL_ALLOW_BLOCKING=1 blocks the work bead; reply unblocks it and
#       surfaces the verdict note
#
# SEEN RED (law-absence-needs-a-positive-control):
#   (a) work bead confirmed 0 blocking-deps before send; confirmed still 0 after send
#   (b) decision bead confirmed open before reply, then confirmed closed
#   (c) persona mailbox confirmed empty before reply
#   (d) work bead confirmed 0 blocking-deps before send; confirmed 1 after override send
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

bead_open_deps() {   # count open BLOCKING deps only; relates-to links are not blockers
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
deps = d[0].get("dependencies") or []
print(len([x for x in deps if x.get("status") != "closed" and x.get("dependency_type") == "blocks"]))' 2>/dev/null
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

# Helper: send a question mail; returns Message-ID.
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
    local newest
    newest="$(ls -t "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | head -1)"
    [ -z "$newest" ] && return 1
    awk '/^[[:space:]]*$/ { exit }
        tolower($0) ~ /^message-id:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[<>]/, ""); print; exit }
    ' "$SPIRA_MAIL/$mailbox/new/$newest"
}

# Helper: send a question with SPIRA_MAIL_ALLOW_BLOCKING=1; returns Message-ID.
send_question_blocking() {
    local mailbox="$1" work_bead="$2"
    SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_ALLOW_BLOCKING=1 \
        run send "$mailbox" \
        --from "Builder <builder@spira>" \
        --subject "Should I proceed (blocking)?" \
        --kind question \
        --default "proceed" \
        --bead "$work_bead" <<'BODY' >/dev/null 2>&1
## Question

Should I proceed?

## Default

proceed
BODY
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
# (a) QUESTION WITH --bead DOES NOT BLOCK THE WORK BEAD (default)
# ==========================================================================
echo
echo "(a) question with --bead does not block the work bead (default non-blocking)"

WORK_A="sp-da-work"
seed_bead "$WORK_A" || { echo "test-mail-decision-ask: could not seed work bead"; exit 1; }

open_deps_before="$(bead_open_deps "$WORK_A")"
is "SEEN RED: work bead has 0 open blocking-deps before send" "0" "$open_deps_before"

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
is "work bead has 0 open blocking-deps after send (guard prevented blocking)" "0" "$open_deps_after"

work_status_a="$(bead_status "$WORK_A")"
is "work bead is still open (not closed)" "open" "$work_status_a"

dec_status_a="$(bead_status "$x_bead_a")"
is "decision bead is open (waiting for reply)" "open" "$dec_status_a"

# ==========================================================================
# (b) REPLY CLOSES DECISION BEAD; WORK BEAD (NOT BLOCKED) STAYS OPEN
# ==========================================================================
echo
echo "(b) reply closes decision bead; work bead stays open"

compose_reply "$MSGID_A" "Yes, proceed with option A." | run sendmail 2>&1; rc=$?
isz "sendmail exits 0 on reply" "$rc"

dec_status_b="$(bead_status "$x_bead_a")"
is "decision bead is closed after reply" "closed" "$dec_status_b"

work_status_b="$(bead_status "$WORK_A")"
is "work bead is still open (reply did not close it)" "open" "$work_status_b"

open_deps_b="$(bead_open_deps "$WORK_A")"
is "work bead still has 0 open blocking-deps after reply" "0" "$open_deps_b"

# ==========================================================================
# (c) REPLY TO AEON PERSONA LANDS IN CONCIERGE
# ==========================================================================
echo
echo "(c) reply to aeon persona message lands in concierge"

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

mkdir -p "$SPIRA_MAIL/builder/new" "$SPIRA_MAIL/builder/tmp" "$SPIRA_MAIL/builder/cur"
mkdir -p "$SPIRA_MAIL/concierge/new" "$SPIRA_MAIL/concierge/tmp" "$SPIRA_MAIL/concierge/cur"

builder_before="$(ls "$SPIRA_MAIL/builder/new" 2>/dev/null | wc -l | tr -d ' ')"
conc_before="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"

compose_reply "$MSGID_C" "Go ahead." | run sendmail 2>&1

builder_after="$(ls "$SPIRA_MAIL/builder/new" 2>/dev/null | wc -l | tr -d ' ')"
conc_after="$(ls "$SPIRA_MAIL/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"

is "SEEN RED: builder mailbox did not grow" "$builder_before" "$builder_after"
is "reply to aeon persona (builder) routes to concierge" "$((conc_before + 1))" "$conc_after"

# ==========================================================================
# (d) SPIRA_MAIL_ALLOW_BLOCKING=1 BLOCKS THE WORK BEAD; VERDICT PROPAGATED
# ==========================================================================
echo
echo "(d) SPIRA_MAIL_ALLOW_BLOCKING=1: work bead blocked; verdict note on reply"

WORK_D="sp-dd-work"
seed_bead "$WORK_D"

open_deps_d_before="$(bead_open_deps "$WORK_D")"
is "SEEN RED: work bead has 0 blocking-deps before override send" "0" "$open_deps_d_before"

MSGID_D="$(send_question_blocking operator "$WORK_D")"
[ -n "$MSGID_D" ] || { echo "test-mail-decision-ask: could not send blocking question"; exit 1; }

newest_d="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
x_bead_d="$(awk '/^[[:space:]]*$/ { exit }
    tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
' "$SPIRA_MAIL/operator/new/$newest_d")"

open_deps_d_after="$(bead_open_deps "$WORK_D")"
is "with override, work bead IS blocked (1 open blocking-dep)" "1" "$open_deps_d_after"

compose_reply "$MSGID_D" "Yes, proceed." | run sendmail 2>&1; rc=$?
isz "sendmail exits 0 on blocking reply" "$rc"

dec_status_d="$(bead_status "$x_bead_d")"
is "blocking decision bead is closed after reply" "closed" "$dec_status_d"

open_deps_d_reply="$(bead_open_deps "$WORK_D")"
is "work bead unblocked after reply (0 open blocking-deps)" "0" "$open_deps_d_reply"

notes_d="$(bead_notes "$WORK_D")"
want "work bead note contains operator verdict" "Operator verdict on decision bead $x_bead_d" "$notes_d"
want "verdict note includes the reply body" "Yes, proceed." "$notes_d"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
